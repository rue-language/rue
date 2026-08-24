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
use rue_rir::{InstData, InstRef};
use rue_span::{FileId, Span};

use super::comptime::{
    ComptimeArgMode, ComptimeCallAdmission, ComptimeEngine, ComptimeHost, PreparedComptimeCall,
};
use super::context::{AnalysisContext, ConstValue, LocalVar, ParamIndex, ParamInfo};
use super::info::ConstInfo;
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
    pub(crate) producer: Option<InstRef>,
    pub(crate) canonical_identity: Option<super::anon_structs::IssuedStableProducerId>,
    /// Comptime type parameters in scope (e.g. `T` -> `i32`).
    pub(crate) type_subst: &'a AHashMap<Spur, Type>,
    /// Comptime value parameters in scope (e.g. `N` -> `42`).
    pub(crate) value_subst: &'a AHashMap<Spur, ConstValue>,
    /// Resolved types from HM inference for the function being analyzed.
    /// `None` when evaluating expressions outside a typed function context
    /// (comptime function bodies before specialization, const initializers).
    pub(crate) resolved_types: Option<&'a AHashMap<InstRef, Type>>,
    /// Runtime locals in scope at the point being evaluated. A runtime local
    /// shadows same-named comptime parameters and file-level constants, so a
    /// reference to it makes the expression non-evaluable — without this,
    /// `let n = x; g(n)` inside a body with `comptime n` in scope would
    /// wrongly evaluate `n` to the parameter's value (spec 4.14:6).
    pub(crate) runtime_locals: Option<&'a AHashMap<Spur, LocalVar>>,
    /// Runtime parameters in scope. They shadow same-named type values and
    /// constants just like locals; comptime parameters resolve through the
    /// substitution maps before this guard is consulted.
    pub(crate) runtime_params: Option<(&'a [ParamInfo], &'a ParamIndex)>,
    /// Runtime bindings known only by name during the pre-inference local
    /// type-alias walk. This lightweight lexical view prevents ordinary
    /// parameters and earlier `let` bindings from falling through to global
    /// constant/type lookup before `AnalysisContext` exists.
    pub(crate) runtime_binding_names: Option<&'a AHashSet<Spur>>,
    /// `let` bindings introduced by blocks inside the comptime expression.
    pub(crate) locals: AHashMap<Spur, ConstValue>,
    /// Values of module-member accesses (`m.CONST`) appearing in this
    /// expression, pre-resolved from the module's file (with privacy checks)
    /// before evaluation. The engine has no file/collector context of its own,
    /// so a `FieldGet` on a module is only evaluable as a sub-expression
    /// operand (`1 + m.CONST`) by looking its value up here (RUE-267). Keyed by
    /// the `FieldGet` instruction. Empty outside const-initializer evaluation.
    pub(crate) const_module_members: &'a AHashMap<InstRef, ConstValue>,
    /// The file whose code is currently being reduced (RUE-511). A
    /// module-qualified comptime call written in a `-> type` constructor body
    /// (`let O = b.Mk(T)`) names an import (`b`) of *this* file's import graph,
    /// not of the file that triggered the instantiation — so resolving the
    /// receiver as a module binding must key the tagged resolution by this file, not
    /// the instantiation site. Set from `ctx.current_file_id` when analyzing a
    /// body, and to the callee's `FunctionInfo.file_id` when reducing a
    /// type-constructor body. `None` where no file context is available (the
    /// receiver is then non-evaluable and the call is a runtime call).
    pub(crate) defining_file: Option<FileId>,
}

impl<'a> ComptimeEnv<'a> {
    /// The substitution maps augmented with this environment's comptime
    /// `let` locals (RUE-575): a type-valued local (`let Inner = Mk(T);`)
    /// participates in type resolution exactly like a `comptime T: type`
    /// parameter, and an integer/bool-valued local like a comptime value
    /// parameter, wherever the anonymous-type arms resolve field, payload,
    /// and method-signature types. Locals are inserted last, so an alias
    /// shadows a same-named enclosing parameter (lexical scoping).
    pub(crate) fn substs_with_locals(&self) -> (AHashMap<Spur, Type>, AHashMap<Spur, ConstValue>) {
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
            canonical_identity: Some(ctx.canonical_producer.clone()),
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
pub(crate) fn const_pattern_matches(
    pattern: &rue_rir::RirPatternView<'_>,
    scrut: ConstValue,
) -> Option<bool> {
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

    /// Thin adapter into the canonical compile-time dispatcher.
    pub(crate) fn eval_const_expr(
        &mut self,
        inst_ref: InstRef,
        env: &mut ComptimeEnv,
    ) -> CompileResult<Option<ConstValue>> {
        ComptimeEngine::new(self).evaluate(inst_ref, env)
    }

    /// Complete a child call after the engine has evaluated its body. This
    /// hook owns only semantic bookkeeping; it never walks RIR or starts a
    /// second evaluator.
    fn finish_comptime_call(
        &mut self,
        plan: &PreparedComptimeCall,
        result: CompileResult<Option<ConstValue>>,
    ) -> CompileResult<Option<ConstValue>> {
        if let Ok(Some(ConstValue::Type(ty))) = &result {
            self.record_ctor_type_display(plan.name, *ty, &plan.callee_types, &plan.callee_values);
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
    ) -> CompileResult<Option<ComptimeCallAdmission>> {
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
            function: fn_info,
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

    /// Bind already-evaluated values to an admitted call. This has no body
    /// lookup or substitution validation, allowing external/provider results
    /// to remain authoritative before local-only checks run.
    fn bind_comptime_call(
        &self,
        admission: &ComptimeCallAdmission,
        values: &[ConstValue],
        _span: Span,
    ) -> CompileResult<Option<(AHashMap<Spur, Type>, AHashMap<Spur, ConstValue>)>> {
        let param_data = self.body_param_data(admission.function.params);
        let param_names = param_data.names().to_vec();
        let param_comptime_type = self.comptime_type_param_flags(&admission.function);
        let mut callee_types: AHashMap<Spur, Type> = AHashMap::new();
        let mut callee_values: AHashMap<Spur, ConstValue> = AHashMap::new();
        for (i, v) in values.iter().copied().enumerate() {
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
                (true, _) => {
                    return Ok(None);
                }
                (false, value) => {
                    callee_values.insert(param_names[i], value);
                }
            }
        }
        Ok(Some((callee_types, callee_values)))
    }

    /// Finish an admitted local call after its arguments have been evaluated.
    /// Provider calls must be queried before this hook so their cached result
    /// or diagnostic remains authoritative.
    fn prepare_local_comptime_call(
        &mut self,
        admission: ComptimeCallAdmission,
        callee_types: AHashMap<Spur, Type>,
        callee_values: AHashMap<Spur, ConstValue>,
        span: Span,
    ) -> CompileResult<Option<PreparedComptimeCall>> {
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
        Ok(Some(PreparedComptimeCall {
            name: name_key,
            body: fn_body_info.body,
            file: fn_body_info.file_id,
            span,
            function_span: fn_body_info.span,
            callee_types,
            callee_values,
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

    fn prepare_comptime_body(
        &mut self,
        name: Spur,
        callee_types: &AHashMap<Spur, Type>,
        callee_values: &AHashMap<Spur, ConstValue>,
        span: Span,
    ) -> CompileResult<Option<PreparedComptimeCall>> {
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
        Ok(Some(PreparedComptimeCall {
            name,
            body: fn_body_info.body,
            file: fn_body_info.file_id,
            span,
            function_span: fn_body_info.span,
            callee_types: callee_types.clone(),
            callee_values: callee_values.clone(),
        }))
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
        let Some(plan) = self.prepare_comptime_body(name, callee_types, callee_values, span)?
        else {
            return Ok(None);
        };
        ComptimeEngine::new(self).evaluate_prepared_root(plan)
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

/// Local semantic adapter for the separated compile-time engine. The adapter only
/// exposes facts and named semantic hooks; recursive instruction traversal
/// remains in `comptime::ComptimeEngine`.
impl<'h, H: OrdinaryBodyAnalysisHost> ComptimeHost for OrdinaryBodyEngine<'h, H> {
    fn program_rir(&self) -> &rue_rir::Rir {
        OrdinaryBodyEngine::body_rir_ref(self)
    }
    fn body_interner(&self) -> &lasso::ThreadedRodeo {
        OrdinaryBodyEngine::body_interner(self)
    }
    fn body_type_pool(&self) -> &crate::intern_pool::TypeInternPool {
        OrdinaryBodyEngine::body_type_pool(self)
    }
    fn value_const(&self, key: &(FileId, Spur)) -> Option<ConstInfo> {
        OrdinaryBodyEngine::value_const(self, key)
    }
    fn require_preview(
        &self,
        feature: rue_error::PreviewFeature,
        what: &str,
        span: Span,
    ) -> CompileResult<()> {
        OrdinaryBodyEngine::require_preview(self, feature, what, span)
    }
    fn record_body_named_dependency(&mut self, target: super::NamedConstDependencyTargetEvent) {
        OrdinaryBodyEngine::record_body_named_dependency(self, target)
    }
    fn reduce_external_comptime_call(
        &mut self,
        name: Spur,
        types: &AHashMap<Spur, Type>,
        values: &AHashMap<Spur, ConstValue>,
        span: Span,
    ) -> Option<CompileResult<Option<ConstValue>>> {
        OrdinaryBodyEngine::reduce_external_comptime_call(self, name, types, values, span)
    }
    fn resolve_array_length(
        &mut self,
        length: &ArrayLen,
        span: Span,
        values: Option<&AHashMap<Spur, ConstValue>>,
    ) -> CompileResult<u64> {
        OrdinaryBodyEngine::resolve_array_length(self, length, span, values)
    }
    fn rir_type_named_symbol(&self, syntax: rue_rir::RirTypeSyntaxRef) -> Option<Spur> {
        OrdinaryBodyEngine::rir_type_named_symbol(self, syntax)
    }
    fn get_or_create_array_type(
        &mut self,
        element: Type,
        length: u64,
    ) -> crate::types::ArrayTypeId {
        OrdinaryBodyEngine::get_or_create_array_type(self, element, length)
    }
    fn extract_anon_method_sigs(
        &mut self,
        methods: &rue_rir::RirAnonStructMethodsRange,
        types: &AHashMap<Spur, Type>,
        values: &AHashMap<Spur, ConstValue>,
    ) -> Vec<super::AnonMethodSig> {
        OrdinaryBodyEngine::extract_anon_method_sigs(self, methods, types, values)
    }
    fn find_method_own_comptime_type_param(
        &self,
        methods: &rue_rir::RirAnonStructMethodsRange,
    ) -> Option<(Span, String)> {
        OrdinaryBodyEngine::find_method_own_comptime_type_param(self, methods)
    }
    fn find_or_create_anon_struct(
        &mut self,
        identity: super::anon_structs::IssuedAnonymousNominalKey,
        fields: &[StructField],
        sigs: &[super::AnonMethodSig],
        captured: &AHashMap<Spur, ConstValue>,
    ) -> CompileResult<(Type, bool)> {
        OrdinaryBodyEngine::find_or_create_anon_struct(self, identity, fields, sigs, captured)
    }
    fn find_or_create_anon_enum(
        &mut self,
        identity: super::anon_structs::IssuedAnonymousNominalKey,
        names: &[String],
        payloads: &[Vec<Type>],
    ) -> CompileResult<Type> {
        OrdinaryBodyEngine::find_or_create_anon_enum(self, identity, names, payloads)
    }
    fn has_method(&self, key: (crate::types::StructId, Spur)) -> bool {
        OrdinaryBodyEngine::has_method(self, key)
    }
    fn check_unqualified_visibility(
        &self,
        kind: &str,
        name: &str,
        file: FileId,
        is_pub: bool,
        span: Span,
    ) -> CompileResult<()> {
        OrdinaryBodyEngine::check_unqualified_visibility(self, kind, name, file, is_pub, span)
    }
    fn check_require_droppable(&mut self, ty: Type, span: Span) -> CompileResult<()> {
        OrdinaryBodyEngine::check_require_droppable(self, ty, span)
    }
    fn check_trivially_droppable(&mut self, ty: Type, span: Span) -> CompileResult<()> {
        OrdinaryBodyEngine::check_trivially_droppable(self, ty, span)
    }
    fn const_expr_type(&self, env: &ComptimeEnv<'_>, inst_ref: InstRef) -> Option<Type> {
        OrdinaryBodyEngine::const_expr_type(self, env, inst_ref)
    }
    fn finish_arith(
        &self,
        result: CheckedIntegerResult,
        ty: Option<Type>,
        op: &str,
        span: Span,
    ) -> CompileResult<Option<ConstValue>> {
        OrdinaryBodyEngine::finish_arith(self, result, ty, op, span)
    }
    fn resolve_named_type_value(&mut self, name: Spur, span: Span) -> CompileResult<Option<Type>> {
        OrdinaryBodyEngine::resolve_named_type_value(self, name, span)
    }
    fn resolve_comptime_type_path(
        &mut self,
        file: FileId,
        segments: &[Spur],
        span: Span,
    ) -> CompileResult<Option<ConstValue>> {
        OrdinaryBodyEngine::resolve_comptime_type_path(self, file, segments, span)
    }
    fn resolve_module_comptime_callable(
        &mut self,
        file: FileId,
        segments: &[Spur],
        method: Spur,
        span: Span,
    ) -> CompileResult<Option<Spur>> {
        OrdinaryBodyEngine::resolve_module_comptime_callable(self, file, segments, method, span)
    }
    fn admit_comptime_call(
        &mut self,
        name: Spur,
        count: usize,
        modes: &[ComptimeArgMode],
        env: &mut ComptimeEnv<'_>,
        resolved: bool,
    ) -> CompileResult<Option<ComptimeCallAdmission>> {
        OrdinaryBodyEngine::admit_comptime_call(self, name, count, modes, env, resolved)
    }
    fn bind_comptime_call(
        &self,
        admission: &ComptimeCallAdmission,
        values: &[ConstValue],
        span: Span,
    ) -> CompileResult<Option<(AHashMap<Spur, Type>, AHashMap<Spur, ConstValue>)>> {
        OrdinaryBodyEngine::bind_comptime_call(self, admission, values, span)
    }
    fn prepare_local_comptime_call(
        &mut self,
        admission: ComptimeCallAdmission,
        types: AHashMap<Spur, Type>,
        values: AHashMap<Spur, ConstValue>,
        span: Span,
    ) -> CompileResult<Option<PreparedComptimeCall>> {
        OrdinaryBodyEngine::prepare_local_comptime_call(self, admission, types, values, span)
    }
    fn finish_comptime_call(
        &mut self,
        plan: &PreparedComptimeCall,
        result: CompileResult<Option<ConstValue>>,
    ) -> CompileResult<Option<ConstValue>> {
        OrdinaryBodyEngine::finish_comptime_call(self, plan, result)
    }
    fn label_ctor_instantiation_site(error: CompileError, span: Span) -> CompileError {
        OrdinaryBodyEngine::<H>::label_ctor_instantiation_site(error, span)
    }
    fn canonical_function_producer(
        &self,
        name: Spur,
        types: &AHashMap<Spur, Type>,
        values: &AHashMap<Spur, ConstValue>,
    ) -> Result<super::anon_structs::IssuedStableProducerId, crate::SemanticBodyExportFailure> {
        OrdinaryBodyEngine::canonical_function_producer(self, name, types, values)
    }
    fn resolve_rir_type_for_comptime_with_subst_and_values_at_span(
        &mut self,
        syntax: rue_rir::RirTypeSyntaxRef,
        types: &AHashMap<Spur, Type>,
        values: &AHashMap<Spur, ConstValue>,
        span: Span,
    ) -> Option<Type> {
        OrdinaryBodyEngine::resolve_rir_type_for_comptime_with_subst_and_values_at_span(
            self, syntax, types, values, span,
        )
    }
    fn register_anon_struct_methods_for_comptime_with_subst(
        &mut self,
        id: crate::types::StructId,
        ty: Type,
        methods: &rue_rir::RirAnonStructMethodsRange,
        types: &AHashMap<Spur, Type>,
        values: &AHashMap<Spur, ConstValue>,
    ) -> Option<()> {
        OrdinaryBodyEngine::register_anon_struct_methods_for_comptime_with_subst(
            self, id, ty, methods, types, values,
        )
    }
    fn set_anon_struct_type_subst(
        &mut self,
        id: crate::types::StructId,
        subst: AHashMap<Spur, Type>,
    ) {
        OrdinaryBodyEngine::set_anon_struct_type_subst(self, id, subst)
    }
}
