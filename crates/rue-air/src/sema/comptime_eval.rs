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
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_rir::{InstData, InstRef, RirPattern};
use rue_span::Span;

use super::Sema;
use super::context::{AnalysisContext, ConstValue, LocalVar};
use crate::specialize::MAX_SPECIALIZATION_ROUNDS;
use crate::types::{StructField, Type, TypeKind};

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
    /// `let` bindings introduced by blocks inside the comptime expression.
    locals: HashMap<Spur, ConstValue>,
    /// Values of module-member accesses (`m.CONST`) appearing in this
    /// expression, pre-resolved from the module's file (with privacy checks)
    /// before evaluation. The engine has no file/collector context of its own,
    /// so a `FieldGet` on a module is only evaluable as a sub-expression
    /// operand (`1 + m.CONST`) by looking its value up here (RUE-267). Keyed by
    /// the `FieldGet` instruction. Empty outside const-initializer evaluation.
    const_module_members: &'a HashMap<InstRef, ConstValue>,
}

impl<'a> ComptimeEnv<'a> {
    /// An environment with no substitutions and no type information.
    pub(crate) fn new() -> Self {
        Self {
            type_subst: &EMPTY_TYPE_SUBST,
            value_subst: &EMPTY_VALUE_SUBST,
            resolved_types: None,
            runtime_locals: None,
            locals: HashMap::new(),
            const_module_members: &EMPTY_MODULE_MEMBERS,
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
            locals: HashMap::new(),
            const_module_members: &EMPTY_MODULE_MEMBERS,
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
            locals: HashMap::new(),
            const_module_members: &EMPTY_MODULE_MEMBERS,
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
    pub(crate) fn for_const_init(
        resolved_types: &'a HashMap<InstRef, Type>,
        const_module_members: &'a HashMap<InstRef, ConstValue>,
    ) -> Self {
        Self {
            type_subst: &EMPTY_TYPE_SUBST,
            value_subst: &EMPTY_VALUE_SUBST,
            resolved_types: Some(resolved_types),
            runtime_locals: None,
            locals: HashMap::new(),
            const_module_members,
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

    /// Resolve a name to a type value (primitive type names, structs, enums).
    fn resolve_named_type_value(&self, name: &Spur) -> Option<Type> {
        let name_str = self.interner.resolve(name);
        // Primitive names come from the single shared table (RUE-155) so the
        // evaluator can never drift from the resolver.
        let ty = match Type::from_primitive_name(name_str) {
            Some(t) => t,
            None => {
                if let Some(&struct_id) = self.structs.get(name) {
                    Type::new_struct(struct_id)
                } else if let Some(&enum_id) = self.enums.get(name) {
                    Type::new_enum(enum_id)
                } else {
                    return None;
                }
            }
        };
        Some(ty)
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
                            env.type_subst,
                            env.value_subst,
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

                let (struct_ty, _is_new) =
                    self.find_or_create_anon_struct(&struct_fields, &method_sigs, env.value_subst);

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
                                    env.type_subst,
                                    env.value_subst,
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
                        if needs_registration && !env.type_subst.is_empty() {
                            self.anon_struct_type_subst
                                .insert(struct_id, env.type_subst.clone());
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
                                env.type_subst,
                                env.value_subst,
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
                // Type parameters in scope substitute first.
                if let Some(&ty) = env.type_subst.get(type_name) {
                    return Ok(Some(ConstValue::Type(ty)));
                }
                Ok(self
                    .resolve_named_type_value(type_name)
                    .map(ConstValue::Type))
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
                // 5. File-level constants: the value was evaluated once
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
                // 6. Type names used as values (e.g. `Point` in
                //    `fn make_type() -> type { Point }`)
                Ok(self.resolve_named_type_value(name).map(ConstValue::Type))
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
            // owning-container well-formedness gate (RUE-388): std's `ArrayBuf(T)`
            // calls it in its `-> type` constructor body so that instantiating
            // the container with an element type it cannot yet correctly own —
            // one that is `linear` or carries a destructor — is rejected at
            // instantiation time (E0499 / E0498) rather than silently leaking the
            // element's `drop fn`. It reduces to unit so the surrounding block
            // body still yields the `struct { .. }` tail. `@size_of`/`@align_of`
            // are not comptime-foldable here and stay non-evaluable.
            InstData::TypeIntrinsic { name, type_arg } => {
                let (name, type_arg) = (*name, *type_arg);
                if self.interner.resolve(&name) != "require_droppable" {
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
                self.check_require_droppable(elem_ty, span)?;
                Ok(Some(ConstValue::Unit))
            }

            // Everything else requires runtime evaluation
            _ => Ok(None),
        }
    }

    /// The `@require_droppable(T)` well-formedness gate for owning growable
    /// containers (RUE-388, Steve's 2026-07-09 ruling).
    ///
    /// A source-level owning container such as `std/arraybuf.rue`'s `ArrayBuf(T)`
    /// owns a heap buffer of `T` and drops it wholesale in its `drop fn` at scope
    /// exit; it does **not** yet run per-element drop glue, nor track element
    /// linearity. So an element type that is `linear` (must be consumed) or that
    /// carries a destructor / drop glue would be silently leaked when the buffer
    /// is freed. Until container/element multiplicity propagation is designed
    /// (its own future ADR — deliberately out of scope here), the container gates
    /// its element type through this check and rejects those two classes:
    ///
    /// - `linear` (transitively — infectious linearity, spec 3.8:57): E0499.
    /// - carries a destructor / drop glue (transitively): E0498.
    ///
    /// Linearity takes precedence in the message: a linear type is the stronger
    /// "must be consumed" property. A trivially-droppable element (primitives,
    /// pointers, `Copy` structs of them) passes.
    pub(crate) fn check_require_droppable(&self, ty: Type, span: Span) -> CompileResult<()> {
        if self.type_carries_linear(ty) {
            return Err(CompileError::new(
                ErrorKind::ContainerElementIsLinear {
                    ty: self.format_type_name(ty),
                },
                span,
            ));
        }
        if self.type_needs_drop_gate(ty) {
            return Err(CompileError::new(
                ErrorKind::ContainerElementHasDestructor {
                    ty: self.format_type_name(ty),
                },
                span,
            ));
        }
        Ok(())
    }

    /// Whether dropping `ty` requires running any drop glue — a destructor
    /// (`drop fn`) on the type itself or, transitively, on a field / array
    /// element / enum-variant payload. Mirrors `rue-cfg`'s `type_needs_drop`
    /// (the drop-glue authority at CFG-build time), replicated here because that
    /// method lives in a different crate; the two must agree so the
    /// `@require_droppable` gate rejects exactly the element types whose drop
    /// glue the container would otherwise silently skip (RUE-388). Linearity is
    /// checked separately by [`Sema::type_carries_linear`].
    fn type_needs_drop_gate(&self, ty: Type) -> bool {
        match ty.kind() {
            TypeKind::Struct(struct_id) => {
                let struct_def = self.type_pool.struct_def(struct_id);
                if struct_def.destructor.is_some() {
                    return true;
                }
                let field_types: Vec<Type> = struct_def.fields.iter().map(|f| f.ty).collect();
                field_types
                    .iter()
                    .any(|&fty| self.type_needs_drop_gate(fty))
            }
            TypeKind::Enum(enum_id) => {
                let enum_def = self.type_pool.enum_def(enum_id);
                let payloads: Vec<Type> = enum_def
                    .variant_payloads
                    .iter()
                    .flatten()
                    .copied()
                    .collect();
                payloads.iter().any(|&pty| self.type_needs_drop_gate(pty))
            }
            TypeKind::Array(array_id) => {
                let (element_type, _length) = self.type_pool.array_def(array_id);
                self.type_needs_drop_gate(element_type)
            }
            _ => false,
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
    /// non-const argument, or an arity/mode that does not match the
    /// implicit-comptime gate: the call is then just a runtime call and simply
    /// non-evaluable here. Reducing the body may still raise `Err` (arithmetic
    /// overflow, recursion-depth overrun); opportunistic callers swallow it.
    ///
    /// [`eval_const_expr`]: Sema::eval_const_expr
    fn eval_comptime_type_call(
        &mut self,
        name: Spur,
        args_start: u32,
        args_len: u32,
        env: &mut ComptimeEnv,
    ) -> CompileResult<Option<ConstValue>> {
        let Some(fn_info) = self.functions.get(&name) else {
            return Ok(None);
        };
        let is_type_fn = fn_info.return_type == Type::COMPTIME_TYPE;
        let params = fn_info.params;
        let param_names = self.param_arena.names(params).to_vec();
        let param_comptime = self.param_arena.comptime(params).to_vec();
        let args = self.rir.get_call_args(args_start, args_len).to_vec();
        if args.len() != param_names.len() {
            return Ok(None);
        }
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
            match v {
                ConstValue::Type(t) => {
                    callee_types.insert(param_names[i], t);
                }
                value => {
                    callee_values.insert(param_names[i], value);
                }
            }
        }
        // The callee body sees only its own parameters. Reduce it with the
        // freshly-built substitution maps.
        self.reduce_type_ctor_body(name, &callee_types, &callee_values)
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
        let mut callee_env = ComptimeEnv::with_subst(callee_types, callee_values);
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
        result
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
    /// compile time are simply skipped (sema diagnoses them later). Like
    /// `AnalysisContext::comptime_type_vars`, the resulting map is flat
    /// (not scope-aware); a shadowed alias resolves to the type value, which
    /// matches the analysis pass's behavior.
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
    ) -> HashMap<Spur, Type> {
        let mut discovered: HashMap<Spur, Type> = HashMap::new();
        let mut eval_types: HashMap<Spur, Type> = type_subst.cloned().unwrap_or_default();
        let eval_values: HashMap<Spur, ConstValue> = value_subst.cloned().unwrap_or_default();
        self.walk_comptime_type_locals(body, &mut discovered, &mut eval_types, &eval_values);
        discovered
    }

    /// In-order walk over statement positions for
    /// [`precompute_comptime_type_locals`]. Only containers that can hold
    /// `let` statements are entered; everything else is left alone.
    fn walk_comptime_type_locals(
        &mut self,
        inst_ref: InstRef,
        discovered: &mut HashMap<Spur, Type>,
        eval_types: &mut HashMap<Spur, Type>,
        eval_values: &HashMap<Spur, ConstValue>,
    ) {
        match &self.rir.get(inst_ref).data {
            InstData::Block { extra_start, len } => {
                let stmts: Vec<InstRef> = self
                    .rir
                    .get_extra(*extra_start, *len)
                    .iter()
                    .map(|&raw| InstRef::from_raw(raw))
                    .collect();
                for stmt in stmts {
                    self.walk_comptime_type_locals(stmt, discovered, eval_types, eval_values);
                }
            }
            InstData::Alloc { name, init, .. } => {
                let (name, init) = (*name, *init);
                if let Some(name) = name {
                    if let Some(ty) = self.try_eval_type_alias_init(init, eval_types, eval_values) {
                        discovered.insert(name, ty);
                        eval_types.insert(name, ty);
                    }
                }
            }
            InstData::Branch {
                then_block,
                else_block,
                ..
            } => {
                let (then_block, else_block) = (*then_block, *else_block);
                self.walk_comptime_type_locals(then_block, discovered, eval_types, eval_values);
                if let Some(else_block) = else_block {
                    self.walk_comptime_type_locals(else_block, discovered, eval_types, eval_values);
                }
            }
            InstData::Loop { body, .. } | InstData::InfiniteLoop { body, .. } => {
                let body = *body;
                self.walk_comptime_type_locals(body, discovered, eval_types, eval_values);
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
                    self.walk_comptime_type_locals(body, discovered, eval_types, eval_values);
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
}
