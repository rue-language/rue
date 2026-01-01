//! Function body analysis and AIR generation.
//!
//! This module contains the core semantic analysis functionality:
//! - Function analysis (analyze_single_function, analyze_method_function, analyze_destructor_function)
//! - Hindley-Milner type inference (run_type_inference)
//! - RIR to AIR instruction lowering (analyze_inst)
//! - Helper functions for expression analysis
//!
//! # Parallel Analysis
//!
//! Function body analysis is parallelized using rayon. The architecture:
//! 1. Declaration gathering (sequential) builds an immutable `SemaContext`
//! 2. Function jobs are collected describing each function to analyze
//! 3. Jobs are processed in parallel using `par_iter`
//! 4. Results are merged (strings deduplicated, warnings collected)

use std::collections::{HashMap, HashSet};

use lasso::Spur;
use rayon::prelude::*;
use rue_builtins::{BuiltinReturnType, BuiltinTypeDef};
use rue_error::{
    CompileError, CompileErrors, CompileResult, CompileWarning, ErrorKind,
    IntrinsicTypeMismatchError, MultiErrorResult, OptionExt, PreviewFeature, WarningKind,
};
use rue_rir::{InstData, InstRef, RirArgMode, RirCallArg, RirDirective, RirParamMode};
use rue_span::Span;

use super::context::{
    AnalysisContext, AnalysisResult, BuiltinMethodContext, ConstValue, FieldPath, ParamInfo,
    ReceiverInfo, StringReceiverStorage,
};
use super::{AnalyzedFunction, InferenceContext, Sema, SemaOutput};
// Note: FunctionAnalyzer types available for future parallel merging
#[allow(unused_imports)]
use crate::function_analyzer::{FunctionAnalyzerOutput, MergedFunctionOutput};
use crate::inference::{
    Constraint, ConstraintContext, ConstraintGenerator, InferType, ParamVarInfo, Unifier,
    UnifyResult,
};
use crate::inst::{Air, AirArgMode, AirCallArg, AirInst, AirInstData, AirPattern, AirRef};
use crate::scope::ScopedContext;
use crate::sema_context::SemaContext;
use crate::types::{EnumId, StructId, Type};

/// Try to evaluate an RIR expression as a compile-time constant.
///
/// This is a standalone function that can be used from both `Sema` methods
/// and parallel analysis code. It only requires a reference to the RIR.
///
/// Returns `Some(value)` if the expression can be fully evaluated at compile time,
/// or `None` if evaluation requires runtime information (e.g., variable values,
/// function calls) or would cause overflow/panic.
fn try_evaluate_const_in_rir(rir: &rue_rir::Rir, inst_ref: InstRef) -> Option<ConstValue> {
    let inst = rir.get(inst_ref);
    match &inst.data {
        // Integer literals
        InstData::IntConst(value) => i64::try_from(*value).ok().map(ConstValue::Integer),

        // Boolean literals
        InstData::BoolConst(value) => Some(ConstValue::Bool(*value)),

        // Unary negation: -expr
        InstData::Neg { operand } => {
            match try_evaluate_const_in_rir(rir, *operand)? {
                ConstValue::Integer(n) => n.checked_neg().map(ConstValue::Integer),
                ConstValue::Bool(_) => None, // Can't negate a boolean
            }
        }

        // Logical NOT: !expr
        InstData::Not { operand } => {
            match try_evaluate_const_in_rir(rir, *operand)? {
                ConstValue::Bool(b) => Some(ConstValue::Bool(!b)),
                ConstValue::Integer(_) => None, // Can't logical-NOT an integer
            }
        }

        // Binary arithmetic operations
        InstData::Add { lhs, rhs } => {
            let l = try_evaluate_const_in_rir(rir, *lhs)?.as_integer()?;
            let r = try_evaluate_const_in_rir(rir, *rhs)?.as_integer()?;
            l.checked_add(r).map(ConstValue::Integer)
        }
        InstData::Sub { lhs, rhs } => {
            let l = try_evaluate_const_in_rir(rir, *lhs)?.as_integer()?;
            let r = try_evaluate_const_in_rir(rir, *rhs)?.as_integer()?;
            l.checked_sub(r).map(ConstValue::Integer)
        }
        InstData::Mul { lhs, rhs } => {
            let l = try_evaluate_const_in_rir(rir, *lhs)?.as_integer()?;
            let r = try_evaluate_const_in_rir(rir, *rhs)?.as_integer()?;
            l.checked_mul(r).map(ConstValue::Integer)
        }
        InstData::Div { lhs, rhs } => {
            let l = try_evaluate_const_in_rir(rir, *lhs)?.as_integer()?;
            let r = try_evaluate_const_in_rir(rir, *rhs)?.as_integer()?;
            if r == 0 {
                None // Division by zero - defer to runtime
            } else {
                l.checked_div(r).map(ConstValue::Integer)
            }
        }
        InstData::Mod { lhs, rhs } => {
            let l = try_evaluate_const_in_rir(rir, *lhs)?.as_integer()?;
            let r = try_evaluate_const_in_rir(rir, *rhs)?.as_integer()?;
            if r == 0 {
                None // Modulo by zero - defer to runtime
            } else {
                l.checked_rem(r).map(ConstValue::Integer)
            }
        }

        // Comparison operations
        InstData::Eq { lhs, rhs } => {
            let l = try_evaluate_const_in_rir(rir, *lhs)?;
            let r = try_evaluate_const_in_rir(rir, *rhs)?;
            match (l, r) {
                (ConstValue::Integer(a), ConstValue::Integer(b)) => Some(ConstValue::Bool(a == b)),
                (ConstValue::Bool(a), ConstValue::Bool(b)) => Some(ConstValue::Bool(a == b)),
                _ => None, // Mixed types
            }
        }
        InstData::Ne { lhs, rhs } => {
            let l = try_evaluate_const_in_rir(rir, *lhs)?;
            let r = try_evaluate_const_in_rir(rir, *rhs)?;
            match (l, r) {
                (ConstValue::Integer(a), ConstValue::Integer(b)) => Some(ConstValue::Bool(a != b)),
                (ConstValue::Bool(a), ConstValue::Bool(b)) => Some(ConstValue::Bool(a != b)),
                _ => None,
            }
        }
        InstData::Lt { lhs, rhs } => {
            let l = try_evaluate_const_in_rir(rir, *lhs)?.as_integer()?;
            let r = try_evaluate_const_in_rir(rir, *rhs)?.as_integer()?;
            Some(ConstValue::Bool(l < r))
        }
        InstData::Gt { lhs, rhs } => {
            let l = try_evaluate_const_in_rir(rir, *lhs)?.as_integer()?;
            let r = try_evaluate_const_in_rir(rir, *rhs)?.as_integer()?;
            Some(ConstValue::Bool(l > r))
        }
        InstData::Le { lhs, rhs } => {
            let l = try_evaluate_const_in_rir(rir, *lhs)?.as_integer()?;
            let r = try_evaluate_const_in_rir(rir, *rhs)?.as_integer()?;
            Some(ConstValue::Bool(l <= r))
        }
        InstData::Ge { lhs, rhs } => {
            let l = try_evaluate_const_in_rir(rir, *lhs)?.as_integer()?;
            let r = try_evaluate_const_in_rir(rir, *rhs)?.as_integer()?;
            Some(ConstValue::Bool(l >= r))
        }

        // Logical operations
        InstData::And { lhs, rhs } => {
            let l = try_evaluate_const_in_rir(rir, *lhs)?.as_bool()?;
            let r = try_evaluate_const_in_rir(rir, *rhs)?.as_bool()?;
            Some(ConstValue::Bool(l && r))
        }
        InstData::Or { lhs, rhs } => {
            let l = try_evaluate_const_in_rir(rir, *lhs)?.as_bool()?;
            let r = try_evaluate_const_in_rir(rir, *rhs)?.as_bool()?;
            Some(ConstValue::Bool(l || r))
        }

        // Bitwise operations
        InstData::BitAnd { lhs, rhs } => {
            let l = try_evaluate_const_in_rir(rir, *lhs)?.as_integer()?;
            let r = try_evaluate_const_in_rir(rir, *rhs)?.as_integer()?;
            Some(ConstValue::Integer(l & r))
        }
        InstData::BitOr { lhs, rhs } => {
            let l = try_evaluate_const_in_rir(rir, *lhs)?.as_integer()?;
            let r = try_evaluate_const_in_rir(rir, *rhs)?.as_integer()?;
            Some(ConstValue::Integer(l | r))
        }
        InstData::BitXor { lhs, rhs } => {
            let l = try_evaluate_const_in_rir(rir, *lhs)?.as_integer()?;
            let r = try_evaluate_const_in_rir(rir, *rhs)?.as_integer()?;
            Some(ConstValue::Integer(l ^ r))
        }
        InstData::Shl { lhs, rhs } => {
            let l = try_evaluate_const_in_rir(rir, *lhs)?.as_integer()?;
            let r = try_evaluate_const_in_rir(rir, *rhs)?.as_integer()?;
            // Only constant-fold small shift amounts to avoid type-width issues.
            if r < 0 || r >= 8 {
                return None;
            }
            Some(ConstValue::Integer(l << r))
        }
        InstData::Shr { lhs, rhs } => {
            let l = try_evaluate_const_in_rir(rir, *lhs)?.as_integer()?;
            let r = try_evaluate_const_in_rir(rir, *rhs)?.as_integer()?;
            // Only constant-fold small shift amounts to avoid type-width issues.
            if r < 0 || r >= 8 {
                return None;
            }
            Some(ConstValue::Integer(l >> r))
        }
        InstData::BitNot { operand } => {
            let n = try_evaluate_const_in_rir(rir, *operand)?.as_integer()?;
            Some(ConstValue::Integer(!n))
        }

        // Comptime blocks: evaluate the inner expression
        InstData::Comptime { expr } => try_evaluate_const_in_rir(rir, *expr),

        // Everything else requires runtime evaluation
        _ => None,
    }
}

/// A description of a function to analyze.
///
/// This is collected before parallel analysis so each function can be
/// processed independently without shared mutable state.
#[derive(Debug)]
enum FunctionJob {
    /// Regular function (not a method).
    Function {
        name: String,
        return_type: Spur,
        params_start: u32,
        params_len: u32,
        body: InstRef,
        span: Span,
    },
    /// Method from an impl block.
    Method {
        full_name: String,
        return_type: Spur,
        params_start: u32,
        params_len: u32,
        body: InstRef,
        span: Span,
        struct_type: Type,
        has_self: bool,
    },
    /// Destructor function.
    Destructor {
        full_name: String,
        body: InstRef,
        span: Span,
        struct_type: Type,
    },
}

/// Result of analyzing a single function.
type FunctionResult = Result<(AnalyzedFunction, Vec<CompileWarning>, Vec<String>), CompileError>;

/// Main entry point for analyzing all function bodies.
///
/// Called from Sema::analyze_all after declarations are collected.
/// Currently uses the sequential analysis path while the parallel infrastructure
/// is being completed.
///
/// # Parallel Analysis Infrastructure
///
/// The parallel analysis infrastructure is ready but not all instruction types
/// are implemented in `analyze_inst_with_context` yet. Once complete:
/// 1. Build `SemaContext` from `Sema`
/// 2. Collect function jobs with `collect_function_jobs`
/// 3. Process with `par_iter` using `analyze_function_job`
/// 4. Merge with `merge_function_results`
pub(crate) fn analyze_all_function_bodies(mut sema: Sema<'_>) -> MultiErrorResult<SemaOutput> {
    // Use sequential analysis path
    analyze_all_function_bodies_sequential(&mut sema)
}

/// Sequential analysis path (current implementation).
fn analyze_all_function_bodies_sequential(sema: &mut Sema<'_>) -> MultiErrorResult<SemaOutput> {
    // Build inference context once
    let infer_ctx = sema.build_inference_context();

    // Collect analyzed functions with their local strings.
    let mut functions_with_strings: Vec<(AnalyzedFunction, Vec<String>)> = Vec::new();
    let mut errors = CompileErrors::new();
    let mut all_warnings = Vec::new();

    // Collect method refs from impl blocks
    let mut method_refs: HashSet<InstRef> = HashSet::new();
    for (_, inst) in sema.rir.iter() {
        if let InstData::ImplDecl {
            methods_start,
            methods_len,
            ..
        } = &inst.data
        {
            let methods = sema.rir.get_inst_refs(*methods_start, *methods_len);
            for method_ref in methods {
                method_refs.insert(method_ref);
            }
        }
    }

    // Analyze regular functions
    for (inst_ref, inst) in sema.rir.iter() {
        if let InstData::FnDecl {
            name,
            params_start,
            params_len,
            return_type,
            body,
            has_self: _,
            ..
        } = &inst.data
        {
            if method_refs.contains(&inst_ref) {
                continue;
            }

            let fn_name = sema.interner.resolve(&*name).to_string();
            let params = sema.rir.get_params(*params_start, *params_len);

            match sema.analyze_single_function(
                &infer_ctx,
                &fn_name,
                *return_type,
                &params,
                *body,
                inst.span,
            ) {
                Ok((analyzed, warnings, local_strings)) => {
                    functions_with_strings.push((analyzed, local_strings));
                    all_warnings.extend(warnings);
                }
                Err(e) => errors.push(e),
            }
        }
    }

    // Analyze method bodies from impl blocks
    for (_, inst) in sema.rir.iter() {
        if let InstData::ImplDecl {
            type_name,
            methods_start,
            methods_len,
        } = &inst.data
        {
            let type_name_str = sema.interner.resolve(&*type_name).to_string();
            let struct_id = match sema.structs.get(type_name) {
                Some(id) => *id,
                None => {
                    errors.push(CompileError::new(
                        ErrorKind::InternalError(format!(
                            "impl block for undefined type '{}' survived validation",
                            type_name_str
                        )),
                        inst.span,
                    ));
                    continue;
                }
            };
            let struct_type = Type::Struct(struct_id);

            let methods = sema.rir.get_inst_refs(*methods_start, *methods_len);
            for method_ref in methods {
                let method_inst = sema.rir.get(method_ref);
                if let InstData::FnDecl {
                    name: method_name,
                    params_start,
                    params_len,
                    return_type,
                    body,
                    has_self,
                    ..
                } = &method_inst.data
                {
                    let method_name_str = sema.interner.resolve(&*method_name).to_string();
                    let params = sema.rir.get_params(*params_start, *params_len);

                    let full_name = if *has_self {
                        format!("{}.{}", type_name_str, method_name_str)
                    } else {
                        format!("{}::{}", type_name_str, method_name_str)
                    };

                    match sema.analyze_method_function(
                        &infer_ctx,
                        &full_name,
                        *return_type,
                        &params,
                        *body,
                        method_inst.span,
                        struct_type,
                        *has_self,
                    ) {
                        Ok((analyzed, warnings, local_strings)) => {
                            functions_with_strings.push((analyzed, local_strings));
                            all_warnings.extend(warnings);
                        }
                        Err(e) => errors.push(e),
                    }
                }
            }
        }
    }

    // Analyze destructor bodies
    for (_, inst) in sema.rir.iter() {
        if let InstData::DropFnDecl { type_name, body } = &inst.data {
            let type_name_str = sema.interner.resolve(&*type_name).to_string();
            let struct_id = match sema.structs.get(type_name) {
                Some(id) => *id,
                None => {
                    errors.push(CompileError::new(
                        ErrorKind::InternalError(format!(
                            "destructor for undefined type '{}' survived validation",
                            type_name_str
                        )),
                        inst.span,
                    ));
                    continue;
                }
            };
            let struct_type = Type::Struct(struct_id);
            let full_name = format!("{}.__drop", type_name_str);

            match sema.analyze_destructor_function(
                &infer_ctx,
                &full_name,
                *body,
                inst.span,
                struct_type,
            ) {
                Ok((analyzed, warnings, local_strings)) => {
                    functions_with_strings.push((analyzed, local_strings));
                    all_warnings.extend(warnings);
                }
                Err(e) => errors.push(e),
            }
        }
    }

    // Merge strings from all functions into a global table with deduplication.
    let mut global_string_table: HashMap<String, u32> = HashMap::new();
    let mut global_strings: Vec<String> = Vec::new();

    let mut functions: Vec<AnalyzedFunction> = Vec::new();
    for (mut analyzed, local_strings) in functions_with_strings {
        if !local_strings.is_empty() {
            let local_to_global: Vec<u32> = local_strings
                .into_iter()
                .map(|s| {
                    *global_string_table.entry(s.clone()).or_insert_with(|| {
                        let id = global_strings.len() as u32;
                        global_strings.push(s);
                        id
                    })
                })
                .collect();

            analyzed
                .air
                .remap_string_ids(|local_id| local_to_global[local_id as usize]);
        }
        functions.push(analyzed);
    }

    all_warnings.sort_by_key(|w| w.span().map(|s| s.start));

    errors.into_result_with(SemaOutput {
        functions,
        struct_defs: std::mem::take(&mut sema.struct_defs),
        enum_defs: std::mem::take(&mut sema.enum_defs),
        array_types: std::mem::take(&mut sema.array_type_defs),
        strings: global_strings,
        warnings: all_warnings,
        type_pool: sema.type_pool.clone(),
    })
}

/// Parallel analysis path (work in progress).
///
/// This will be enabled once all instruction types are implemented in
/// `analyze_inst_with_context`.
#[allow(dead_code)]
fn analyze_all_function_bodies_parallel(sema: Sema<'_>) -> MultiErrorResult<SemaOutput> {
    // Build SemaContext with thread-safe array registry for sharing across threads
    let ctx = sema.build_sema_context();

    // Collect all function jobs
    let jobs = collect_function_jobs(&ctx);

    // Analyze functions in parallel
    // Array types may be created during analysis via ctx.get_or_create_array_type()
    let results: Vec<FunctionResult> = jobs
        .into_par_iter()
        .map(|job| analyze_function_job(&ctx, job))
        .collect();

    // Extract array types from the thread-safe registry
    let array_type_defs = ctx.array_registry.into_defs();

    // Merge results
    merge_function_results(
        results,
        sema.struct_defs,
        sema.enum_defs,
        array_type_defs,
        sema.type_pool,
    )
}

/// Collect all functions to be analyzed from the RIR.
fn collect_function_jobs(ctx: &SemaContext<'_>) -> Vec<FunctionJob> {
    let mut jobs = Vec::new();

    // Collect method refs from impl blocks to skip them in the regular function pass
    let mut method_refs: HashSet<InstRef> = HashSet::new();
    for (_, inst) in ctx.rir.iter() {
        if let InstData::ImplDecl {
            methods_start,
            methods_len,
            ..
        } = &inst.data
        {
            let methods = ctx.rir.get_inst_refs(*methods_start, *methods_len);
            for method_ref in methods {
                method_refs.insert(method_ref);
            }
        }
    }

    // Collect regular functions
    for (inst_ref, inst) in ctx.rir.iter() {
        if let InstData::FnDecl {
            name,
            params_start,
            params_len,
            return_type,
            body,
            has_self: _,
            ..
        } = &inst.data
        {
            if method_refs.contains(&inst_ref) {
                continue;
            }

            let fn_name = ctx.interner.resolve(&*name).to_string();
            jobs.push(FunctionJob::Function {
                name: fn_name,
                return_type: *return_type,
                params_start: *params_start,
                params_len: *params_len,
                body: *body,
                span: inst.span,
            });
        }
    }

    // Collect methods from impl blocks
    for (_, inst) in ctx.rir.iter() {
        if let InstData::ImplDecl {
            type_name,
            methods_start,
            methods_len,
        } = &inst.data
        {
            let type_name_str = ctx.interner.resolve(&*type_name).to_string();
            let struct_id = match ctx.structs.get(type_name) {
                Some(id) => *id,
                None => continue, // Error will be caught elsewhere
            };
            let struct_type = Type::Struct(struct_id);

            let methods = ctx.rir.get_inst_refs(*methods_start, *methods_len);
            for method_ref in methods {
                let method_inst = ctx.rir.get(method_ref);
                if let InstData::FnDecl {
                    name: method_name,
                    params_start,
                    params_len,
                    return_type,
                    body,
                    has_self,
                    ..
                } = &method_inst.data
                {
                    let method_name_str = ctx.interner.resolve(&*method_name).to_string();
                    let full_name = if *has_self {
                        format!("{}.{}", type_name_str, method_name_str)
                    } else {
                        format!("{}::{}", type_name_str, method_name_str)
                    };

                    jobs.push(FunctionJob::Method {
                        full_name,
                        return_type: *return_type,
                        params_start: *params_start,
                        params_len: *params_len,
                        body: *body,
                        span: method_inst.span,
                        struct_type,
                        has_self: *has_self,
                    });
                }
            }
        }
    }

    // Collect destructors
    for (_, inst) in ctx.rir.iter() {
        if let InstData::DropFnDecl { type_name, body } = &inst.data {
            let type_name_str = ctx.interner.resolve(&*type_name).to_string();
            let struct_id = match ctx.structs.get(type_name) {
                Some(id) => *id,
                None => continue, // Error will be caught elsewhere
            };
            let struct_type = Type::Struct(struct_id);
            let full_name = format!("{}.__drop", type_name_str);

            jobs.push(FunctionJob::Destructor {
                full_name,
                body: *body,
                span: inst.span,
                struct_type,
            });
        }
    }

    jobs
}

/// Analyze a single function job using the shared context.
fn analyze_function_job(ctx: &SemaContext<'_>, job: FunctionJob) -> FunctionResult {
    match job {
        FunctionJob::Function {
            name,
            return_type,
            params_start,
            params_len,
            body,
            span,
        } => {
            let params = ctx.rir.get_params(params_start, params_len);
            analyze_regular_function(ctx, &name, return_type, &params, body, span)
        }
        FunctionJob::Method {
            full_name,
            return_type,
            params_start,
            params_len,
            body,
            span,
            struct_type,
            has_self,
        } => {
            let params = ctx.rir.get_params(params_start, params_len);
            analyze_method_function_parallel(
                ctx,
                &full_name,
                return_type,
                &params,
                body,
                span,
                struct_type,
                has_self,
            )
        }
        FunctionJob::Destructor {
            full_name,
            body,
            span,
            struct_type,
        } => analyze_destructor_function_parallel(ctx, &full_name, body, span, struct_type),
    }
}

/// Analyze a regular function using the shared context.
fn analyze_regular_function(
    ctx: &SemaContext<'_>,
    fn_name: &str,
    return_type: Spur,
    params: &[rue_rir::RirParam],
    body: InstRef,
    span: Span,
) -> FunctionResult {
    // Resolve return type
    let ret_type = resolve_type_from_ctx(ctx, return_type, span)?;

    // Resolve parameter types and modes
    let param_info: Vec<(Spur, Type, RirParamMode)> = params
        .iter()
        .map(|p| {
            let ty = resolve_type_from_ctx(ctx, p.ty, span)?;
            Ok((p.name, ty, p.mode))
        })
        .collect::<CompileResult<Vec<_>>>()?;

    analyze_function_with_context(ctx, fn_name, ret_type, &param_info, body)
}

/// Analyze a method function using the shared context.
fn analyze_method_function_parallel(
    ctx: &SemaContext<'_>,
    full_name: &str,
    return_type: Spur,
    params: &[rue_rir::RirParam],
    body: InstRef,
    span: Span,
    struct_type: Type,
    has_self: bool,
) -> FunctionResult {
    let ret_type = resolve_type_from_ctx(ctx, return_type, span)?;

    // Build parameter list, adding self as first parameter for methods
    let mut param_info: Vec<(Spur, Type, RirParamMode)> = Vec::new();

    if has_self {
        // Add self parameter (Normal mode - passed by value)
        let self_sym = ctx.interner.get_or_intern("self");
        param_info.push((self_sym, struct_type, RirParamMode::Normal));
    }

    // Add regular parameters with their modes
    for p in params.iter() {
        let ty = resolve_type_from_ctx(ctx, p.ty, span)?;
        param_info.push((p.name, ty, p.mode));
    }

    analyze_function_with_context(ctx, full_name, ret_type, &param_info, body)
}

/// Analyze a destructor function using the shared context.
fn analyze_destructor_function_parallel(
    ctx: &SemaContext<'_>,
    full_name: &str,
    body: InstRef,
    _span: Span,
    struct_type: Type,
) -> FunctionResult {
    // Destructors take self parameter and return unit
    let self_sym = ctx.interner.get_or_intern("self");
    let param_info: Vec<(Spur, Type, RirParamMode)> =
        vec![(self_sym, struct_type, RirParamMode::Normal)];

    analyze_function_with_context(ctx, full_name, Type::Unit, &param_info, body)
}

/// Resolve a type symbol using the shared context.
fn resolve_type_from_ctx(ctx: &SemaContext<'_>, type_sym: Spur, span: Span) -> CompileResult<Type> {
    let type_name = ctx.interner.resolve(&type_sym);

    // Check primitive types first
    match type_name {
        "i8" => return Ok(Type::I8),
        "i16" => return Ok(Type::I16),
        "i32" => return Ok(Type::I32),
        "i64" => return Ok(Type::I64),
        "u8" => return Ok(Type::U8),
        "u16" => return Ok(Type::U16),
        "u32" => return Ok(Type::U32),
        "u64" => return Ok(Type::U64),
        "bool" => return Ok(Type::Bool),
        "()" => return Ok(Type::Unit),
        "!" => return Ok(Type::Never),
        _ => {}
    }

    if let Some(struct_id) = ctx.get_struct(type_sym) {
        Ok(Type::Struct(struct_id))
    } else if let Some(enum_id) = ctx.get_enum(type_sym) {
        Ok(Type::Enum(enum_id))
    } else {
        // Check for array type syntax: [T; N]
        if let Some((element_type, length)) = crate::types::parse_array_type_syntax(type_name) {
            // Resolve the element type first
            let element_sym = ctx.interner.get_or_intern(&element_type);
            let element_ty = resolve_type_from_ctx(ctx, element_sym, span)?;
            // Get the array type (must exist from declaration gathering)
            if let Some(array_type_id) = ctx.get_array_type(element_ty, length) {
                Ok(Type::Array(array_type_id))
            } else {
                Err(CompileError::new(
                    ErrorKind::UnknownType(type_name.to_string()),
                    span,
                ))
            }
        } else {
            Err(CompileError::new(
                ErrorKind::UnknownType(type_name.to_string()),
                span,
            ))
        }
    }
}

/// Core function analysis using the shared immutable context.
///
/// This is called from the parallel analysis path and works with SemaContext
/// instead of the mutable Sema.
fn analyze_function_with_context(
    ctx: &SemaContext<'_>,
    fn_name: &str,
    return_type: Type,
    params: &[(Spur, Type, RirParamMode)],
    body: InstRef,
) -> FunctionResult {
    let mut air = Air::new(return_type);
    let mut param_map: HashMap<Spur, ParamInfo> = HashMap::new();
    let mut param_modes: Vec<bool> = Vec::new();

    // Add parameters to the param map, tracking ABI slot offsets.
    let mut next_abi_slot: u32 = 0;
    for (pname, ptype, mode) in params.iter() {
        param_map.insert(
            *pname,
            ParamInfo {
                abi_slot: next_abi_slot,
                ty: *ptype,
                mode: *mode,
            },
        );
        let is_by_ref = *mode != RirParamMode::Normal;
        let slot_count = if is_by_ref {
            1
        } else {
            ctx.abi_slot_count(*ptype)
        };
        for _ in 0..slot_count {
            param_modes.push(is_by_ref);
        }
        next_abi_slot += slot_count;
    }
    let num_param_slots = next_abi_slot;

    // Run Hindley-Milner type inference
    let resolved_types = run_type_inference_with_context(ctx, return_type, params, body)?;

    // Create analysis context with resolved types
    let mut analysis_ctx = AnalysisContext {
        locals: HashMap::new(),
        params: &param_map,
        next_slot: 0,
        loop_depth: 0,
        used_locals: HashSet::new(),
        return_type,
        scope_stack: Vec::new(),
        resolved_types: &resolved_types,
        moved_vars: HashMap::new(),
        warnings: Vec::new(),
        local_string_table: HashMap::new(),
        local_strings: Vec::new(),
    };

    // Analyze the body expression
    let body_result = analyze_inst_with_context(ctx, &mut air, body, &mut analysis_ctx)?;

    // Add implicit return only if body doesn't already diverge
    if body_result.ty != Type::Never {
        air.add_inst(AirInst {
            data: AirInstData::Ret(Some(body_result.air_ref)),
            ty: return_type,
            span: ctx.rir.get(body).span,
        });
    }

    Ok((
        AnalyzedFunction {
            name: fn_name.to_string(),
            air,
            num_locals: analysis_ctx.next_slot,
            num_param_slots,
            param_modes,
        },
        analysis_ctx.warnings,
        analysis_ctx.local_strings,
    ))
}

/// Run type inference using the shared context.
fn run_type_inference_with_context(
    ctx: &SemaContext<'_>,
    return_type: Type,
    params: &[(Spur, Type, RirParamMode)],
    body: InstRef,
) -> CompileResult<HashMap<InstRef, Type>> {
    // Create constraint generator using pre-computed inference context
    let mut cgen = ConstraintGenerator::new(
        ctx.rir,
        ctx.interner,
        &ctx.inference_ctx.func_sigs,
        &ctx.inference_ctx.struct_types,
        &ctx.inference_ctx.enum_types,
        &ctx.inference_ctx.method_sigs,
    );

    // Build parameter map for constraint context.
    // Convert Type to InferType so arrays are represented structurally.
    let param_vars: HashMap<Spur, ParamVarInfo> = params
        .iter()
        .map(|(name, ty, _mode)| {
            (
                *name,
                ParamVarInfo {
                    ty: ctx.type_to_infer_type(*ty),
                },
            )
        })
        .collect();

    // Create constraint context
    let mut cgen_ctx = ConstraintContext::new(&param_vars, return_type);

    // Phase 1: Generate constraints
    let body_info = cgen.generate(body, &mut cgen_ctx);

    // The function body's type must match the return type.
    cgen.add_constraint(Constraint::equal(
        body_info.ty,
        InferType::Concrete(return_type),
        body_info.span,
    ));

    // Consume the constraint generator to release borrows
    let (constraints, int_literal_vars, expr_types, type_var_count) = cgen.into_parts();

    // Phase 2: Solve constraints via unification
    let mut unifier = Unifier::with_capacity(type_var_count);
    let errors = unifier.solve_constraints(&constraints);

    // Convert unification errors to compile errors
    if let Some(err) = errors.first() {
        let error_kind = match &err.kind {
            UnifyResult::Ok => unreachable!("UnificationError should never contain Ok"),
            UnifyResult::TypeMismatch { expected, found } => ErrorKind::TypeMismatch {
                expected: expected.to_string(),
                found: found.to_string(),
            },
            UnifyResult::IntLiteralNonInteger { found } => ErrorKind::TypeMismatch {
                expected: "integer type".to_string(),
                found: found.name().to_string(),
            },
            UnifyResult::OccursCheck { var, ty } => ErrorKind::TypeMismatch {
                expected: "non-recursive type".to_string(),
                found: format!("{var} = {ty} (infinite type)"),
            },
            UnifyResult::NotSigned { ty } => ErrorKind::CannotNegateUnsigned(ty.name().to_string()),
            UnifyResult::NotInteger { ty } => ErrorKind::TypeMismatch {
                expected: "integer type".to_string(),
                found: ty.name().to_string(),
            },
            UnifyResult::NotUnsigned { ty } => ErrorKind::TypeMismatch {
                expected: "unsigned integer type".to_string(),
                found: ty.name().to_string(),
            },
            UnifyResult::ArrayLengthMismatch { expected, found } => {
                ErrorKind::ArrayLengthMismatch {
                    expected: *expected,
                    found: *found,
                }
            }
        };

        let mut compile_error = CompileError::new(error_kind, err.span);
        if matches!(err.kind, UnifyResult::NotSigned { .. }) {
            compile_error = compile_error.with_note("unsigned values cannot be negated");
        }
        return Err(compile_error);
    }

    // Default any unconstrained integer literals to i32
    unifier.default_int_literal_vars(&int_literal_vars);

    // Build the resolved types map, converting InferType to Type.
    // Note: Array types should already be created during declaration gathering.
    // If new array types appear in function bodies (e.g., array literals), they
    // won't be found and will result in Type::Error.
    let mut resolved_types = HashMap::new();
    for (inst_ref, infer_ty) in &expr_types {
        let resolved = unifier.resolve_infer_type(infer_ty);
        let concrete_ty = infer_type_to_type_standalone(&resolved, ctx);
        resolved_types.insert(*inst_ref, concrete_ty);
    }

    Ok(resolved_types)
}

/// Convert an InferType to a concrete Type using the context.
///
/// This function is thread-safe and can be called from parallel function analysis.
/// Array types are created on-demand via the thread-safe `ArrayTypeRegistry`.
fn infer_type_to_type_standalone(ty: &InferType, ctx: &SemaContext<'_>) -> Type {
    match ty {
        InferType::Concrete(t) => *t,
        InferType::Var(_) => Type::Error,
        InferType::IntLiteral => Type::I32,
        InferType::Array { element, length } => {
            let elem_ty = infer_type_to_type_standalone(element, ctx);
            if elem_ty == Type::Error {
                return Type::Error;
            }
            // Use get_or_create to handle inferred array types from literals
            let id = ctx.get_or_create_array_type(elem_ty, *length);
            Type::Array(id)
        }
    }
}

/// Analyze an RIR instruction using the shared context.
///
/// This is the parallel-compatible version of `Sema::analyze_inst`. It uses
/// `SemaContext` (immutable, shared) instead of `&mut Sema`.
fn analyze_inst_with_context(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    inst_ref: InstRef,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    let inst = ctx.rir.get(inst_ref);

    match &inst.data {
        InstData::IntConst(value) => {
            // Get the type from HM inference
            let ty = get_resolved_type_ctx(analysis_ctx, inst_ref, inst.span, "integer literal")?;

            // Check if the literal value fits in the target type's range
            if !ty.literal_fits(*value) {
                return Err(CompileError::new(
                    ErrorKind::LiteralOutOfRange {
                        value: *value,
                        ty: ty.name().to_string(),
                    },
                    inst.span,
                ));
            }

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Const(*value),
                ty,
                span: inst.span,
            });
            Ok(AnalysisResult::new(air_ref, ty))
        }

        InstData::BoolConst(value) => {
            let ty = Type::Bool;
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::BoolConst(*value),
                ty,
                span: inst.span,
            });
            Ok(AnalysisResult::new(air_ref, ty))
        }

        InstData::StringConst(symbol) => {
            let ty = ctx.builtin_string_type();
            let string_content = ctx.interner.resolve(&*symbol).to_string();
            let local_string_id = analysis_ctx.add_local_string(string_content);

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::StringConst(local_string_id),
                ty,
                span: inst.span,
            });
            Ok(AnalysisResult::new(air_ref, ty))
        }

        InstData::UnitConst => {
            let ty = Type::Unit;
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::UnitConst,
                ty,
                span: inst.span,
            });
            Ok(AnalysisResult::new(air_ref, ty))
        }

        InstData::Add { lhs, rhs } => analyze_binary_arith_ctx(
            ctx,
            air,
            *lhs,
            *rhs,
            AirInstData::Add,
            inst.span,
            analysis_ctx,
        ),

        InstData::Sub { lhs, rhs } => analyze_binary_arith_ctx(
            ctx,
            air,
            *lhs,
            *rhs,
            AirInstData::Sub,
            inst.span,
            analysis_ctx,
        ),

        InstData::Mul { lhs, rhs } => analyze_binary_arith_ctx(
            ctx,
            air,
            *lhs,
            *rhs,
            AirInstData::Mul,
            inst.span,
            analysis_ctx,
        ),

        InstData::Div { lhs, rhs } => analyze_binary_arith_ctx(
            ctx,
            air,
            *lhs,
            *rhs,
            AirInstData::Div,
            inst.span,
            analysis_ctx,
        ),

        InstData::Mod { lhs, rhs } => analyze_binary_arith_ctx(
            ctx,
            air,
            *lhs,
            *rhs,
            AirInstData::Mod,
            inst.span,
            analysis_ctx,
        ),

        InstData::Eq { lhs, rhs } => analyze_comparison_ctx(
            ctx,
            air,
            *lhs,
            *rhs,
            true,
            AirInstData::Eq,
            inst.span,
            analysis_ctx,
        ),

        InstData::Ne { lhs, rhs } => analyze_comparison_ctx(
            ctx,
            air,
            *lhs,
            *rhs,
            true,
            AirInstData::Ne,
            inst.span,
            analysis_ctx,
        ),

        InstData::Lt { lhs, rhs } => analyze_comparison_ctx(
            ctx,
            air,
            *lhs,
            *rhs,
            false,
            AirInstData::Lt,
            inst.span,
            analysis_ctx,
        ),

        InstData::Gt { lhs, rhs } => analyze_comparison_ctx(
            ctx,
            air,
            *lhs,
            *rhs,
            false,
            AirInstData::Gt,
            inst.span,
            analysis_ctx,
        ),

        InstData::Le { lhs, rhs } => analyze_comparison_ctx(
            ctx,
            air,
            *lhs,
            *rhs,
            false,
            AirInstData::Le,
            inst.span,
            analysis_ctx,
        ),

        InstData::Ge { lhs, rhs } => analyze_comparison_ctx(
            ctx,
            air,
            *lhs,
            *rhs,
            false,
            AirInstData::Ge,
            inst.span,
            analysis_ctx,
        ),

        InstData::And { lhs, rhs } => {
            let lhs_result = analyze_inst_with_context(ctx, air, *lhs, analysis_ctx)?;
            let rhs_result = analyze_inst_with_context(ctx, air, *rhs, analysis_ctx)?;

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::And(lhs_result.air_ref, rhs_result.air_ref),
                ty: Type::Bool,
                span: inst.span,
            });
            Ok(AnalysisResult::new(air_ref, Type::Bool))
        }

        InstData::Or { lhs, rhs } => {
            let lhs_result = analyze_inst_with_context(ctx, air, *lhs, analysis_ctx)?;
            let rhs_result = analyze_inst_with_context(ctx, air, *rhs, analysis_ctx)?;

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Or(lhs_result.air_ref, rhs_result.air_ref),
                ty: Type::Bool,
                span: inst.span,
            });
            Ok(AnalysisResult::new(air_ref, Type::Bool))
        }

        InstData::BitAnd { lhs, rhs } => analyze_binary_arith_ctx(
            ctx,
            air,
            *lhs,
            *rhs,
            AirInstData::BitAnd,
            inst.span,
            analysis_ctx,
        ),

        InstData::BitOr { lhs, rhs } => analyze_binary_arith_ctx(
            ctx,
            air,
            *lhs,
            *rhs,
            AirInstData::BitOr,
            inst.span,
            analysis_ctx,
        ),

        InstData::BitXor { lhs, rhs } => analyze_binary_arith_ctx(
            ctx,
            air,
            *lhs,
            *rhs,
            AirInstData::BitXor,
            inst.span,
            analysis_ctx,
        ),

        InstData::Shl { lhs, rhs } => analyze_binary_arith_ctx(
            ctx,
            air,
            *lhs,
            *rhs,
            AirInstData::Shl,
            inst.span,
            analysis_ctx,
        ),

        InstData::Shr { lhs, rhs } => analyze_binary_arith_ctx(
            ctx,
            air,
            *lhs,
            *rhs,
            AirInstData::Shr,
            inst.span,
            analysis_ctx,
        ),

        // Unary operations
        InstData::Neg { operand } => {
            // Get the resolved type from HM inference
            let ty = get_resolved_type_ctx(analysis_ctx, inst_ref, inst.span, "negation operator")?;

            // Check if trying to negate an unsigned type.
            if ty.is_unsigned() {
                return Err(CompileError::new(
                    ErrorKind::CannotNegateUnsigned(ty.name().to_string()),
                    inst.span,
                )
                .with_note("unsigned values cannot be negated"));
            }

            // Special case: negating a literal that equals |MIN| for signed types.
            let operand_inst = ctx.rir.get(*operand);
            if let InstData::IntConst(value) = &operand_inst.data {
                // Check if this value, when negated, fits in the target signed type
                if ty.negated_literal_fits(*value) && !ty.literal_fits(*value) {
                    // This is the MIN value case - store the MIN value directly.
                    let neg_value = match ty {
                        Type::I8 => (i8::MIN as i64) as u64,
                        Type::I16 => (i16::MIN as i64) as u64,
                        Type::I32 => (i32::MIN as i64) as u64,
                        Type::I64 => i64::MIN as u64,
                        _ => unreachable!(),
                    };
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Const(neg_value),
                        ty,
                        span: inst.span,
                    });
                    return Ok(AnalysisResult::new(air_ref, ty));
                }
            }

            let operand_result = analyze_inst_with_context(ctx, air, *operand, analysis_ctx)?;

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Neg(operand_result.air_ref),
                ty,
                span: inst.span,
            });
            Ok(AnalysisResult::new(air_ref, ty))
        }

        InstData::Not { operand } => {
            let operand_result = analyze_inst_with_context(ctx, air, *operand, analysis_ctx)?;

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Not(operand_result.air_ref),
                ty: Type::Bool,
                span: inst.span,
            });
            Ok(AnalysisResult::new(air_ref, Type::Bool))
        }

        InstData::BitNot { operand } => {
            // Get the resolved type from HM inference
            let ty =
                get_resolved_type_ctx(analysis_ctx, inst_ref, inst.span, "bitwise NOT operator")?;

            // Bitwise NOT operates on integer types only
            if !ty.is_integer() && !ty.is_error() && !ty.is_never() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "integer type".to_string(),
                        found: ty.name().to_string(),
                    },
                    inst.span,
                ));
            }

            let operand_result = analyze_inst_with_context(ctx, air, *operand, analysis_ctx)?;

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::BitNot(operand_result.air_ref),
                ty,
                span: inst.span,
            });
            Ok(AnalysisResult::new(air_ref, ty))
        }

        // Control flow: Break and Continue
        InstData::Break => {
            // Validate that we're inside a loop
            if analysis_ctx.loop_depth == 0 {
                return Err(CompileError::new(ErrorKind::BreakOutsideLoop, inst.span));
            }

            // Break has the never type - it diverges
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Break,
                ty: Type::Never,
                span: inst.span,
            });
            Ok(AnalysisResult::new(air_ref, Type::Never))
        }

        InstData::Continue => {
            // Validate that we're inside a loop
            if analysis_ctx.loop_depth == 0 {
                return Err(CompileError::new(ErrorKind::ContinueOutsideLoop, inst.span));
            }

            // Continue has the never type - it diverges
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Continue,
                ty: Type::Never,
                span: inst.span,
            });
            Ok(AnalysisResult::new(air_ref, Type::Never))
        }

        // Return statement
        InstData::Ret(inner) => {
            analyze_return_ctx(ctx, air, inner.as_ref().copied(), inst.span, analysis_ctx)
        }

        // Block expression
        InstData::Block { extra_start, len } => {
            analyze_block_ctx(ctx, air, *extra_start, *len, inst.span, analysis_ctx)
        }

        // Variable operations
        InstData::VarRef { name } => analyze_var_ref_ctx(ctx, air, *name, inst.span, analysis_ctx),

        InstData::ParamRef { name, .. } => {
            analyze_param_ref_ctx(ctx, air, *name, inst.span, analysis_ctx)
        }

        InstData::Alloc {
            directives_start,
            directives_len,
            name,
            is_mut,
            ty: _,
            init,
        } => analyze_alloc_ctx(
            ctx,
            air,
            *directives_start,
            *directives_len,
            *name,
            *is_mut,
            *init,
            inst.span,
            analysis_ctx,
        ),

        InstData::Assign { name, value } => {
            analyze_assign_ctx(ctx, air, *name, *value, inst.span, analysis_ctx)
        }

        // Control flow: Branch
        InstData::Branch {
            cond,
            then_block,
            else_block,
        } => analyze_branch_ctx(
            ctx,
            air,
            *cond,
            *then_block,
            *else_block,
            inst.span,
            analysis_ctx,
        ),

        // Control flow: Loops
        InstData::Loop { cond, body } => {
            analyze_while_loop_ctx(ctx, air, *cond, *body, inst.span, analysis_ctx)
        }

        InstData::InfiniteLoop { body } => {
            analyze_infinite_loop_ctx(ctx, air, *body, inst.span, analysis_ctx)
        }

        // Match expression
        InstData::Match {
            scrutinee,
            arms_start,
            arms_len,
        } => analyze_match_ctx(
            ctx,
            air,
            *scrutinee,
            *arms_start,
            *arms_len,
            inst.span,
            analysis_ctx,
        ),

        // Struct operations
        InstData::StructDecl { .. } => {
            // Struct declarations are handled at the top level
            Err(CompileError::new(
                ErrorKind::InternalError(
                    "StructDecl should not appear in expression context".to_string(),
                ),
                inst.span,
            ))
        }

        InstData::StructInit {
            type_name,
            fields_start,
            fields_len,
        } => analyze_struct_init_ctx(
            ctx,
            air,
            *type_name,
            *fields_start,
            *fields_len,
            inst.span,
            analysis_ctx,
        ),

        InstData::FieldGet { base, field } => {
            analyze_field_get_ctx(ctx, air, inst_ref, *base, *field, inst.span, analysis_ctx)
        }

        InstData::FieldSet { base, field, value } => {
            analyze_field_set_ctx(ctx, air, *base, *field, *value, inst.span, analysis_ctx)
        }

        // Array operations
        InstData::ArrayInit {
            elems_start,
            elems_len,
        } => analyze_array_init_ctx(
            ctx,
            air,
            inst_ref,
            *elems_start,
            *elems_len,
            inst.span,
            analysis_ctx,
        ),

        InstData::IndexGet { base, index } => {
            analyze_index_get_ctx(ctx, air, *base, *index, inst.span, analysis_ctx)
        }

        InstData::IndexSet { base, index, value } => {
            analyze_index_set_ctx(ctx, air, *base, *index, *value, inst.span, analysis_ctx)
        }

        // Enum operations
        InstData::EnumDecl { .. } => {
            // Enum declarations are processed during collection phase
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::UnitConst,
                ty: Type::Unit,
                span: inst.span,
            });
            Ok(AnalysisResult::new(air_ref, Type::Unit))
        }

        InstData::EnumVariant { type_name, variant } => {
            analyze_enum_variant_ctx(ctx, air, *type_name, *variant, inst.span)
        }

        // Call operations
        InstData::Call {
            name,
            args_start,
            args_len,
        } => analyze_call_ctx(
            ctx,
            air,
            *name,
            *args_start,
            *args_len,
            inst.span,
            analysis_ctx,
        ),

        InstData::MethodCall {
            receiver,
            method,
            args_start,
            args_len,
        } => analyze_method_call_ctx(
            ctx,
            air,
            *receiver,
            *method,
            *args_start,
            *args_len,
            inst.span,
            analysis_ctx,
        ),

        InstData::AssocFnCall {
            type_name,
            function,
            args_start,
            args_len,
        } => analyze_assoc_fn_call_ctx(
            ctx,
            air,
            *type_name,
            *function,
            *args_start,
            *args_len,
            inst.span,
            analysis_ctx,
        ),

        // Intrinsic operations
        InstData::Intrinsic {
            name,
            args_start,
            args_len,
        } => analyze_intrinsic_ctx(
            ctx,
            air,
            inst_ref,
            *name,
            *args_start,
            *args_len,
            inst.span,
            analysis_ctx,
        ),

        InstData::TypeIntrinsic { name, type_arg } => {
            analyze_type_intrinsic_ctx(ctx, air, *name, *type_arg, inst.span)
        }

        // Declaration no-ops (produce Unit in expression context)
        InstData::ImplDecl { .. } | InstData::DropFnDecl { .. } => {
            // These are processed during collection phase, just return Unit
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::UnitConst,
                ty: Type::Unit,
                span: inst.span,
            });
            Ok(AnalysisResult::new(air_ref, Type::Unit))
        }

        InstData::FnDecl { .. } => {
            // Function declarations are errors in expression context
            Err(CompileError::new(
                ErrorKind::InternalError(
                    "FnDecl should not appear in expression context".to_string(),
                ),
                inst.span,
            ))
        }

        InstData::Comptime { expr } => {
            // Gate the comptime feature
            if !ctx.preview_features.contains(&PreviewFeature::Comptime) {
                return Err(CompileError::new(
                    ErrorKind::PreviewFeatureRequired {
                        feature: PreviewFeature::Comptime,
                        what: "comptime blocks".to_string(),
                    },
                    inst.span,
                )
                .with_help(format!(
                    "use `--preview {}` to enable this feature ({})",
                    PreviewFeature::Comptime.name(),
                    PreviewFeature::Comptime.adr()
                )));
            }

            // Try to evaluate the inner expression at compile time
            match try_evaluate_const_in_rir(ctx.rir, *expr) {
                Some(ConstValue::Integer(value)) => {
                    // Get the expected type from HM inference
                    let ty =
                        get_resolved_type_ctx(analysis_ctx, inst_ref, inst.span, "comptime block")?;

                    // Check if the value fits in the target type
                    if value < 0 {
                        // Can't represent negative values as u64 directly
                        // For now, we only support non-negative integer values
                        return Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: "negative values not yet supported in comptime".to_string(),
                            },
                            inst.span,
                        ));
                    }

                    let unsigned_value = value as u64;
                    if !ty.literal_fits(unsigned_value) {
                        return Err(CompileError::new(
                            ErrorKind::LiteralOutOfRange {
                                value: unsigned_value,
                                ty: ty.name().to_string(),
                            },
                            inst.span,
                        ));
                    }

                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::Const(unsigned_value),
                        ty,
                        span: inst.span,
                    });
                    Ok(AnalysisResult::new(air_ref, ty))
                }
                Some(ConstValue::Bool(value)) => {
                    let ty = Type::Bool;
                    let air_ref = air.add_inst(AirInst {
                        data: AirInstData::BoolConst(value),
                        ty,
                        span: inst.span,
                    });
                    Ok(AnalysisResult::new(air_ref, ty))
                }
                None => {
                    // The expression couldn't be evaluated at compile time
                    Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason:
                                "expression contains values that cannot be known at compile time"
                                    .to_string(),
                        },
                        inst.span,
                    ))
                }
            }
        }
    }
}

/// Get resolved type from the analysis context (parallel version).
fn get_resolved_type_ctx(
    ctx: &AnalysisContext,
    inst_ref: InstRef,
    span: Span,
    what: &str,
) -> CompileResult<Type> {
    ctx.resolved_types.get(&inst_ref).copied().ok_or_else(|| {
        CompileError::new(
            ErrorKind::InternalError(format!("no resolved type for {} at {:?}", what, inst_ref)),
            span,
        )
    })
}

/// Analyze binary arithmetic operation (parallel version).
fn analyze_binary_arith_ctx<F>(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    lhs: InstRef,
    rhs: InstRef,
    make_inst: F,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult>
where
    F: FnOnce(AirRef, AirRef) -> AirInstData,
{
    let lhs_result = analyze_inst_with_context(ctx, air, lhs, analysis_ctx)?;
    let rhs_result = analyze_inst_with_context(ctx, air, rhs, analysis_ctx)?;
    let ty = lhs_result.ty;

    let air_ref = air.add_inst(AirInst {
        data: make_inst(lhs_result.air_ref, rhs_result.air_ref),
        ty,
        span,
    });
    Ok(AnalysisResult::new(air_ref, ty))
}

/// Analyze comparison operation (parallel version).
fn analyze_comparison_ctx<F>(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    lhs: InstRef,
    rhs: InstRef,
    _allows_bool: bool,
    make_inst: F,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult>
where
    F: FnOnce(AirRef, AirRef) -> AirInstData,
{
    let lhs_result = analyze_inst_with_context(ctx, air, lhs, analysis_ctx)?;
    let rhs_result = analyze_inst_with_context(ctx, air, rhs, analysis_ctx)?;

    let air_ref = air.add_inst(AirInst {
        data: make_inst(lhs_result.air_ref, rhs_result.air_ref),
        ty: Type::Bool,
        span,
    });
    Ok(AnalysisResult::new(air_ref, Type::Bool))
}

/// Merge results from parallel function analysis.
fn merge_function_results(
    results: Vec<FunctionResult>,
    struct_defs: Vec<crate::types::StructDef>,
    enum_defs: Vec<crate::types::EnumDef>,
    array_type_defs: Vec<crate::types::ArrayTypeDef>,
    type_pool: crate::intern_pool::TypeInternPool,
) -> MultiErrorResult<SemaOutput> {
    let mut errors = CompileErrors::new();
    let mut functions_with_strings: Vec<(AnalyzedFunction, Vec<String>)> = Vec::new();
    let mut all_warnings = Vec::new();

    // Collect successes and errors
    for result in results {
        match result {
            Ok((analyzed, warnings, local_strings)) => {
                functions_with_strings.push((analyzed, local_strings));
                all_warnings.extend(warnings);
            }
            Err(e) => errors.push(e),
        }
    }

    // Merge strings from all functions into a global table with deduplication
    let mut global_string_table: HashMap<String, u32> = HashMap::new();
    let mut global_strings: Vec<String> = Vec::new();

    let mut functions: Vec<AnalyzedFunction> = Vec::new();
    for (mut analyzed, local_strings) in functions_with_strings {
        if !local_strings.is_empty() {
            let local_to_global: Vec<u32> = local_strings
                .into_iter()
                .map(|s| {
                    *global_string_table.entry(s.clone()).or_insert_with(|| {
                        let id = global_strings.len() as u32;
                        global_strings.push(s);
                        id
                    })
                })
                .collect();

            analyzed
                .air
                .remap_string_ids(|local_id| local_to_global[local_id as usize]);
        }
        functions.push(analyzed);
    }

    all_warnings.sort_by_key(|w| w.span().map(|s| s.start));

    errors.into_result_with(SemaOutput {
        functions,
        struct_defs,
        enum_defs,
        array_types: array_type_defs,
        strings: global_strings,
        warnings: all_warnings,
        type_pool,
    })
}

// ============================================================================
// Helper functions for parallel analysis (using SemaContext)
// ============================================================================

/// Analyze a return statement using the shared context.
fn analyze_return_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    inner: Option<InstRef>,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    let inner_air_ref = if let Some(inner) = inner {
        // Explicit return with value
        let inner_result = analyze_inst_with_context(ctx, air, inner, analysis_ctx)?;
        let inner_ty = inner_result.ty;

        // Type check: returned value must match function's return type.
        if !analysis_ctx.return_type.is_error()
            && !inner_ty.is_error()
            && !inner_ty.can_coerce_to(&analysis_ctx.return_type)
        {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: analysis_ctx.return_type.name().to_string(),
                    found: inner_ty.name().to_string(),
                },
                span,
            ));
        }
        Some(inner_result.air_ref)
    } else {
        // `return;` without expression - only valid for unit-returning functions
        if analysis_ctx.return_type != Type::Unit && !analysis_ctx.return_type.is_error() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: analysis_ctx.return_type.name().to_string(),
                    found: "()".to_string(),
                },
                span,
            ));
        }
        None
    };

    let air_ref = air.add_inst(AirInst {
        data: AirInstData::Ret(inner_air_ref),
        ty: Type::Never, // Return expressions have Never type
        span,
    });
    Ok(AnalysisResult::new(air_ref, Type::Never))
}

/// Analyze a block expression using the shared context.
fn analyze_block_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    extra_start: u32,
    len: u32,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    // Get the instruction refs from extra data
    let inst_refs = ctx.rir.get_extra(extra_start, len);

    // Push a new scope for this block.
    analysis_ctx.push_scope();

    // Process all instructions in the block
    let mut statements = Vec::new();
    let mut last_result: Option<AnalysisResult> = None;
    let num_insts = inst_refs.len();
    for (i, &raw_ref) in inst_refs.iter().enumerate() {
        let inst_ref = InstRef::from_raw(raw_ref);
        let is_last = i == num_insts - 1;
        let result = analyze_inst_with_context(ctx, air, inst_ref, analysis_ctx)?;

        if is_last {
            last_result = Some(result);
        } else {
            statements.push(result.air_ref);
        }
    }

    // Check for unconsumed linear values before popping scope
    check_unconsumed_linear_values_ctx(ctx, analysis_ctx)?;

    // Check for unused variables before popping scope
    check_unused_locals_in_current_scope_ctx(ctx, analysis_ctx);

    // Pop scope to remove block-scoped variables.
    analysis_ctx.pop_scope();

    // Handle empty blocks - they evaluate to Unit
    let last = match last_result {
        Some(result) => result,
        None => {
            // Empty block: create a UnitConst
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::UnitConst,
                ty: Type::Unit,
                span,
            });
            AnalysisResult::new(air_ref, Type::Unit)
        }
    };

    // Only create a Block instruction if there are statements;
    // otherwise just return the value directly (optimization)
    if statements.is_empty() {
        Ok(last)
    } else {
        let ty = last.ty;
        let stmt_u32s: Vec<u32> = statements.iter().map(|r| r.as_u32()).collect();
        let stmts_start = air.add_extra(&stmt_u32s);
        let stmts_len = statements.len() as u32;
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Block {
                stmts_start,
                stmts_len,
                value: last.air_ref,
            },
            ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, ty))
    }
}

/// Check for unconsumed linear values at scope exit.
fn check_unconsumed_linear_values_ctx(
    ctx: &SemaContext<'_>,
    analysis_ctx: &AnalysisContext,
) -> CompileResult<()> {
    // Check locals in the current scope
    if let Some(scope_entries) = analysis_ctx.scope_stack.last() {
        for (symbol, _) in scope_entries {
            if let Some(local) = analysis_ctx.locals.get(symbol) {
                let ty = local.ty;
                // Check if this is a linear type
                if ctx.is_type_linear(ty) {
                    // Check if it's been consumed (moved)
                    let is_consumed = analysis_ctx
                        .moved_vars
                        .get(symbol)
                        .map(|state| state.full_move.is_some())
                        .unwrap_or(false);

                    if !is_consumed {
                        let name = ctx.interner.resolve(symbol);
                        return Err(CompileError::new(
                            ErrorKind::LinearValueNotConsumed(name.to_string()),
                            local.span,
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Check for unused variables in the current scope.
fn check_unused_locals_in_current_scope_ctx(
    ctx: &SemaContext<'_>,
    analysis_ctx: &mut AnalysisContext,
) {
    if let Some(scope_entries) = analysis_ctx.scope_stack.last() {
        for (symbol, old_value) in scope_entries {
            // Only check variables that were newly introduced (not shadowed)
            if old_value.is_none() {
                if let Some(local) = analysis_ctx.locals.get(symbol) {
                    // Skip if @allow(unused_variable) was applied
                    if local.allow_unused {
                        continue;
                    }
                    // Check if the variable was used
                    if !analysis_ctx.used_locals.contains(symbol) {
                        let name = ctx.interner.resolve(symbol);
                        // Don't warn about underscore-prefixed names
                        if !name.starts_with('_') {
                            analysis_ctx.warnings.push(CompileWarning::new(
                                WarningKind::UnusedVariable(name.to_string()),
                                local.span,
                            ));
                        }
                    }
                }
            }
        }
    }
}

/// Analyze a variable reference using the shared context.
fn analyze_var_ref_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    name: Spur,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    // First check if it's a parameter
    if let Some(param_info) = analysis_ctx.params.get(&name) {
        let ty = param_info.ty;
        let name_str = ctx.interner.resolve(&name);

        // Check if this parameter has been moved
        if let Some(move_state) = analysis_ctx.moved_vars.get(&name) {
            if let Some(moved_span) = move_state.is_any_part_moved() {
                return Err(
                    CompileError::new(ErrorKind::UseAfterMove(name_str.to_string()), span)
                        .with_label("value moved here", moved_span),
                );
            }
        }

        // Handle move semantics based on parameter mode
        if !ctx.is_type_copy(ty) {
            match param_info.mode {
                RirParamMode::Normal => {
                    analysis_ctx
                        .moved_vars
                        .entry(name)
                        .or_default()
                        .mark_path_moved(&[], span);
                }
                RirParamMode::Inout => {
                    analysis_ctx
                        .moved_vars
                        .entry(name)
                        .or_default()
                        .mark_path_moved(&[], span);
                }
                RirParamMode::Borrow => {
                    let name_str = ctx.interner.resolve(&name);
                    return Err(CompileError::new(
                        ErrorKind::MoveOutOfBorrow {
                            variable: name_str.to_string(),
                        },
                        span,
                    ));
                }
            }
        }

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Param {
                index: param_info.abi_slot,
            },
            ty,
            span,
        });
        return Ok(AnalysisResult::new(air_ref, ty));
    }

    // Look up the variable in locals
    let name_str = ctx.interner.resolve(&name);
    let local = analysis_ctx
        .locals
        .get(&name)
        .ok_or_compile_error(ErrorKind::UndefinedVariable(name_str.to_string()), span)?;

    let ty = local.ty;
    let slot = local.slot;

    // Check if this variable has been moved
    if let Some(move_state) = analysis_ctx.moved_vars.get(&name) {
        if let Some(moved_span) = move_state.is_any_part_moved() {
            return Err(
                CompileError::new(ErrorKind::UseAfterMove(name_str.to_string()), span)
                    .with_label("value moved here", moved_span),
            );
        }
    }

    // If type is not Copy, mark as moved
    if !ctx.is_type_copy(ty) {
        analysis_ctx
            .moved_vars
            .entry(name)
            .or_default()
            .mark_path_moved(&[], span);
    }

    // Mark variable as used
    analysis_ctx.used_locals.insert(name);

    // Load the variable
    let air_ref = air.add_inst(AirInst {
        data: AirInstData::Load { slot },
        ty,
        span,
    });
    Ok(AnalysisResult::new(air_ref, ty))
}

/// Analyze a parameter reference using the shared context.
fn analyze_param_ref_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    name: Spur,
    span: Span,
    analysis_ctx: &AnalysisContext,
) -> CompileResult<AnalysisResult> {
    let name_str = ctx.interner.resolve(&name);
    let param_info = analysis_ctx
        .params
        .get(&name)
        .ok_or_compile_error(ErrorKind::UndefinedVariable(name_str.to_string()), span)?;

    let ty = param_info.ty;

    let air_ref = air.add_inst(AirInst {
        data: AirInstData::Param {
            index: param_info.abi_slot,
        },
        ty,
        span,
    });
    Ok(AnalysisResult::new(air_ref, ty))
}

/// Check if a directive is an @allow directive with the specified name.
fn has_allow_directive_ctx(
    ctx: &SemaContext<'_>,
    directives: &[RirDirective],
    directive_name: &str,
) -> bool {
    for directive in directives {
        let name = ctx.interner.resolve(&directive.name);
        if name == "allow" && !directive.args.is_empty() {
            let arg_name = ctx.interner.resolve(&directive.args[0]);
            if arg_name == directive_name {
                return true;
            }
        }
    }
    false
}

/// Analyze a local variable allocation using the shared context.
fn analyze_alloc_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    directives_start: u32,
    directives_len: u32,
    name: Option<Spur>,
    is_mut: bool,
    init: InstRef,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    use super::context::LocalVar;

    // Analyze the initializer
    let init_result = analyze_inst_with_context(ctx, air, init, analysis_ctx)?;
    let var_type = init_result.ty;

    // If name is None, this is a wildcard pattern `_` that discards the value
    let Some(name) = name else {
        return Ok(AnalysisResult::new(init_result.air_ref, Type::Unit));
    };

    // Check if @allow(unused_variable) directive is present
    let directives = ctx.rir.get_directives(directives_start, directives_len);
    let allow_unused = has_allow_directive_ctx(ctx, &directives, "unused_variable");

    // Allocate slots
    let slot = analysis_ctx.next_slot;
    let num_slots = ctx.abi_slot_count(var_type);
    analysis_ctx.next_slot += num_slots;

    // Register the variable
    analysis_ctx.insert_local(
        name,
        LocalVar {
            slot,
            ty: var_type,
            is_mut,
            span,
            allow_unused,
        },
    );

    // Emit StorageLive to mark the slot as live
    let storage_live_ref = air.add_inst(AirInst {
        data: AirInstData::StorageLive { slot },
        ty: var_type,
        span,
    });

    // Emit the alloc instruction
    let alloc_ref = air.add_inst(AirInst {
        data: AirInstData::Alloc {
            slot,
            init: init_result.air_ref,
        },
        ty: Type::Unit,
        span,
    });

    // Return a block containing both StorageLive and Alloc
    let stmts_start = air.add_extra(&[storage_live_ref.as_u32()]);
    let block_ref = air.add_inst(AirInst {
        data: AirInstData::Block {
            stmts_start,
            stmts_len: 1,
            value: alloc_ref,
        },
        ty: Type::Unit,
        span,
    });
    Ok(AnalysisResult::new(block_ref, Type::Unit))
}

/// Analyze an assignment using the shared context.
fn analyze_assign_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    name: Spur,
    value: InstRef,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    let name_str = ctx.interner.resolve(&name);

    // First check if it's a parameter (for inout params)
    if let Some(param_info) = analysis_ctx.params.get(&name) {
        // Check parameter mode - only inout can be assigned to
        match param_info.mode {
            RirParamMode::Normal => {
                return Err(CompileError::new(
                    ErrorKind::AssignToImmutable(name_str.to_string()),
                    span,
                )
                .with_help(format!(
                    "consider making parameter `{}` inout: `inout {}: {}`",
                    name_str,
                    name_str,
                    param_info.ty.name()
                )));
            }
            RirParamMode::Inout => {
                // Inout parameters can be assigned to
            }
            RirParamMode::Borrow => {
                return Err(CompileError::new(
                    ErrorKind::MutateBorrowedValue {
                        variable: name_str.to_string(),
                    },
                    span,
                ));
            }
        }

        let abi_slot = param_info.abi_slot;

        // Analyze the value
        let value_result = analyze_inst_with_context(ctx, air, value, analysis_ctx)?;

        // Assignment to a parameter resets its move state
        analysis_ctx.moved_vars.remove(&name);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::ParamStore {
                param_slot: abi_slot,
                value: value_result.air_ref,
            },
            ty: Type::Unit,
            span,
        });
        return Ok(AnalysisResult::new(air_ref, Type::Unit));
    }

    // Look up local variable
    let local = analysis_ctx
        .locals
        .get(&name)
        .ok_or_compile_error(ErrorKind::UndefinedVariable(name_str.to_string()), span)?;

    // Check mutability
    if !local.is_mut {
        return Err(
            CompileError::new(ErrorKind::AssignToImmutable(name_str.to_string()), span)
                .with_label("variable declared as immutable here", local.span)
                .with_help(format!(
                    "consider making `{}` mutable: `let mut {}`",
                    name_str, name_str
                )),
        );
    }

    let slot = local.slot;

    // Analyze the value
    let value_result = analyze_inst_with_context(ctx, air, value, analysis_ctx)?;

    // Assignment to a mutable variable resets its move state.
    analysis_ctx.moved_vars.remove(&name);

    // Emit store instruction
    let air_ref = air.add_inst(AirInst {
        data: AirInstData::Store {
            slot,
            value: value_result.air_ref,
        },
        ty: Type::Unit,
        span,
    });
    Ok(AnalysisResult::new(air_ref, Type::Unit))
}

/// Analyze a branch (if-else) expression using the shared context.
fn analyze_branch_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    cond: InstRef,
    then_block: InstRef,
    else_block: Option<InstRef>,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    // Condition must be bool
    let cond_result = analyze_inst_with_context(ctx, air, cond, analysis_ctx)?;

    if let Some(else_b) = else_block {
        // Save move state before entering branches.
        let saved_moves = analysis_ctx.moved_vars.clone();

        // Analyze then branch with its own scope
        analysis_ctx.push_scope();
        let then_result = analyze_inst_with_context(ctx, air, then_block, analysis_ctx)?;
        let then_type = then_result.ty;
        let then_span = ctx.rir.get(then_block).span;
        analysis_ctx.pop_scope();

        // Capture then-branch's move state
        let then_moves = analysis_ctx.moved_vars.clone();

        // Restore to saved state before analyzing else branch
        analysis_ctx.moved_vars = saved_moves;

        // Analyze else branch with its own scope
        analysis_ctx.push_scope();
        let else_result = analyze_inst_with_context(ctx, air, else_b, analysis_ctx)?;
        let else_type = else_result.ty;
        let else_span = ctx.rir.get(else_b).span;
        analysis_ctx.pop_scope();

        // Capture else-branch's move state
        let else_moves = analysis_ctx.moved_vars.clone();

        // Merge move states from both branches.
        analysis_ctx.merge_branch_moves(
            then_moves,
            else_moves,
            then_type.is_never(),
            else_type.is_never(),
        );

        // Compute the unified result type using never type coercion
        let result_type = match (then_type.is_never(), else_type.is_never()) {
            (true, true) => Type::Never,
            (true, false) => else_type,
            (false, true) => then_type,
            (false, false) => {
                // Neither diverges - types must match exactly
                if then_type != else_type && !then_type.is_error() && !else_type.is_error() {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: then_type.name().to_string(),
                            found: else_type.name().to_string(),
                        },
                        else_span,
                    )
                    .with_label(format!("this is of type `{}`", then_type.name()), then_span)
                    .with_note("if and else branches must have compatible types"));
                }
                then_type
            }
        };

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Branch {
                cond: cond_result.air_ref,
                then_value: then_result.air_ref,
                else_value: Some(else_result.air_ref),
            },
            ty: result_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_type))
    } else {
        // No else branch - result is Unit
        // The then branch must have unit type (spec 4.6:5)

        // Save move state before entering then-branch.
        let saved_moves = analysis_ctx.moved_vars.clone();

        analysis_ctx.push_scope();
        let then_result = analyze_inst_with_context(ctx, air, then_block, analysis_ctx)?;
        analysis_ctx.pop_scope();

        // Check that the then branch has unit type (or Never/Error)
        let then_type = then_result.ty;
        if then_type != Type::Unit && !then_type.is_never() && !then_type.is_error() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "()".to_string(),
                    found: then_type.name().to_string(),
                },
                ctx.rir.get(then_block).span,
            )
            .with_help(
                "if expressions without else must have unit type; \
                 consider adding an else branch or making the body return ()",
            ));
        }

        // Capture then-branch's move state
        let then_moves = analysis_ctx.moved_vars.clone();

        // For if-without-else:
        if then_type.is_never() {
            // Then-branch diverges - code after if only runs if cond was false
            analysis_ctx.moved_vars = saved_moves;
        } else {
            // Then-branch doesn't diverge - merge moves (union semantics).
            analysis_ctx.merge_branch_moves(
                then_moves,
                saved_moves,
                false, // then doesn't diverge
                false, // "else" (empty) doesn't diverge
            );
        }

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Branch {
                cond: cond_result.air_ref,
                then_value: then_result.air_ref,
                else_value: None,
            },
            ty: Type::Unit,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Unit))
    }
}

/// Analyze a while loop using the shared context.
fn analyze_while_loop_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    cond: InstRef,
    body: InstRef,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    // While loop: condition must be bool, result is Unit
    let cond_result = analyze_inst_with_context(ctx, air, cond, analysis_ctx)?;

    // Analyze body with its own scope
    analysis_ctx.push_scope();
    analysis_ctx.loop_depth += 1;
    let body_result = analyze_inst_with_context(ctx, air, body, analysis_ctx)?;
    analysis_ctx.loop_depth -= 1;
    analysis_ctx.pop_scope();

    let air_ref = air.add_inst(AirInst {
        data: AirInstData::Loop {
            cond: cond_result.air_ref,
            body: body_result.air_ref,
        },
        ty: Type::Unit,
        span,
    });
    Ok(AnalysisResult::new(air_ref, Type::Unit))
}

/// Analyze an infinite loop using the shared context.
fn analyze_infinite_loop_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    body: InstRef,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    // Infinite loop: `loop { body }` - always produces Never type

    analysis_ctx.push_scope();
    analysis_ctx.loop_depth += 1;
    let body_result = analyze_inst_with_context(ctx, air, body, analysis_ctx)?;
    analysis_ctx.loop_depth -= 1;
    analysis_ctx.pop_scope();

    let air_ref = air.add_inst(AirInst {
        data: AirInstData::InfiniteLoop {
            body: body_result.air_ref,
        },
        ty: Type::Never,
        span,
    });
    Ok(AnalysisResult::new(air_ref, Type::Never))
}

/// Analyze a match expression using the shared context.
fn analyze_match_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    scrutinee: InstRef,
    arms_start: u32,
    arms_len: u32,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    use rue_rir::RirPattern;

    // Analyze the scrutinee to determine its type
    let scrutinee_result = analyze_inst_with_context(ctx, air, scrutinee, analysis_ctx)?;
    let scrutinee_type = scrutinee_result.ty;

    // Validate that we can match on this type (integers, booleans, and enums)
    if !scrutinee_type.is_integer() && scrutinee_type != Type::Bool && !scrutinee_type.is_enum() {
        return Err(CompileError::new(
            ErrorKind::InvalidMatchType(scrutinee_type.name().to_string()),
            span,
        ));
    }

    let arms = ctx.rir.get_match_arms(arms_start, arms_len);
    // Check for empty match
    if arms.is_empty() {
        return Err(CompileError::new(ErrorKind::EmptyMatch, span));
    }

    // Track patterns for exhaustiveness checking and duplicate detection
    let mut wildcard_span: Option<Span> = None;
    let mut bool_true_span: Option<Span> = None;
    let mut bool_false_span: Option<Span> = None;
    let mut seen_ints: HashMap<i64, Span> = HashMap::new();
    let mut covered_variants: HashSet<u32> = HashSet::new();
    let mut seen_variants: HashMap<u32, Span> = HashMap::new();
    let mut pattern_enum_id: Option<EnumId> = None;

    // Analyze each arm (each arm gets its own scope)
    let mut air_arms = Vec::new();
    let mut result_type: Option<Type> = None;

    for (pattern, body) in arms.iter() {
        let pattern_span = pattern.span();

        // If we've seen a wildcard, everything after is unreachable
        if let Some(first_wildcard_span) = wildcard_span {
            let pat_str = match pattern {
                RirPattern::Wildcard(_) => "_".to_string(),
                RirPattern::Int(n, _) => n.to_string(),
                RirPattern::Bool(b, _) => b.to_string(),
                RirPattern::Path {
                    type_name, variant, ..
                } => {
                    format!(
                        "{}::{}",
                        ctx.interner.resolve(&*type_name),
                        ctx.interner.resolve(&*variant)
                    )
                }
            };
            analysis_ctx.warnings.push(
                CompileWarning::new(WarningKind::UnreachablePattern(pat_str), pattern_span)
                    .with_label("previous wildcard pattern here", first_wildcard_span)
                    .with_note(
                        "this pattern will never be matched because the wildcard pattern above matches everything",
                    ),
            );
        }

        // Validate pattern against scrutinee type and check for duplicates
        match pattern {
            RirPattern::Wildcard(_) => {
                if wildcard_span.is_none() {
                    wildcard_span = Some(pattern_span);
                }
            }
            RirPattern::Int(n, _) => {
                if !scrutinee_type.is_integer() {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: scrutinee_type.name().to_string(),
                            found: "integer".to_string(),
                        },
                        pattern_span,
                    ));
                }
                // Check for duplicate integer pattern
                if let Some(first_span) = seen_ints.get(n) {
                    if wildcard_span.is_none() {
                        analysis_ctx.warnings.push(
                            CompileWarning::new(
                                WarningKind::UnreachablePattern(n.to_string()),
                                pattern_span,
                            )
                            .with_label("first occurrence of this pattern", *first_span)
                            .with_note(
                                "this pattern will never be matched because an earlier arm already matches the same value",
                            ),
                        );
                    }
                } else {
                    seen_ints.insert(*n, pattern_span);
                }
            }
            RirPattern::Bool(b, _) => {
                if scrutinee_type != Type::Bool {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: scrutinee_type.name().to_string(),
                            found: "bool".to_string(),
                        },
                        pattern_span,
                    ));
                }
                // Check for duplicate boolean pattern
                let (first_span_opt, is_true) = if *b {
                    (&mut bool_true_span, true)
                } else {
                    (&mut bool_false_span, false)
                };
                if let Some(first_span) = *first_span_opt {
                    if wildcard_span.is_none() {
                        analysis_ctx.warnings.push(
                            CompileWarning::new(
                                WarningKind::UnreachablePattern(is_true.to_string()),
                                pattern_span,
                            )
                            .with_label("first occurrence of this pattern", first_span)
                            .with_note(
                                "this pattern will never be matched because an earlier arm already matches the same value",
                            ),
                        );
                    }
                } else {
                    *first_span_opt = Some(pattern_span);
                }
            }
            RirPattern::Path {
                type_name, variant, ..
            } => {
                // Look up the enum type
                let enum_id = ctx.get_enum(*type_name).ok_or_compile_error(
                    ErrorKind::UnknownEnumType(ctx.interner.resolve(&*type_name).to_string()),
                    pattern_span,
                )?;
                let enum_def = ctx.get_enum_def(enum_id);

                // Check that scrutinee type matches the pattern's enum type
                if scrutinee_type != Type::Enum(enum_id) {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: scrutinee_type.name().to_string(),
                            found: enum_def.name.clone(),
                        },
                        pattern_span,
                    ));
                }

                // Find the variant index
                let variant_name = ctx.interner.resolve(&*variant);
                let variant_index = enum_def.find_variant(variant_name).ok_or_compile_error(
                    ErrorKind::UnknownVariant {
                        enum_name: enum_def.name.clone(),
                        variant_name: variant_name.to_string(),
                    },
                    pattern_span,
                )?;

                // Check for duplicate variant
                if let Some(first_span) = seen_variants.get(&(variant_index as u32)) {
                    if wildcard_span.is_none() {
                        analysis_ctx.warnings.push(
                            CompileWarning::new(
                                WarningKind::UnreachablePattern(format!(
                                    "{}::{}",
                                    enum_def.name, variant_name
                                )),
                                pattern_span,
                            )
                            .with_label("first occurrence of this pattern", *first_span)
                            .with_note(
                                "this pattern will never be matched because an earlier arm already matches the same value",
                            ),
                        );
                    }
                } else {
                    seen_variants.insert(variant_index as u32, pattern_span);
                }

                covered_variants.insert(variant_index as u32);
                pattern_enum_id = Some(enum_id);
            }
        }

        // Each arm gets its own scope
        analysis_ctx.push_scope();

        // Analyze arm body
        let body_result = analyze_inst_with_context(ctx, air, *body, analysis_ctx)?;
        let body_type = body_result.ty;

        analysis_ctx.pop_scope();

        // Update result type (handle Never type coercion)
        result_type = Some(match result_type {
            None => body_type,
            Some(prev) => {
                if prev.is_never() {
                    body_type
                } else if body_type.is_never() {
                    prev
                } else if prev != body_type && !prev.is_error() && !body_type.is_error() {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: prev.name().to_string(),
                            found: body_type.name().to_string(),
                        },
                        ctx.rir.get(*body).span,
                    ));
                } else {
                    prev
                }
            }
        });

        // Convert pattern to AIR pattern
        let air_pattern = match pattern {
            RirPattern::Wildcard(_) => AirPattern::Wildcard,
            RirPattern::Int(n, _) => AirPattern::Int(*n),
            RirPattern::Bool(b, _) => AirPattern::Bool(*b),
            RirPattern::Path {
                type_name, variant, ..
            } => {
                let type_name_str = ctx.interner.resolve(&*type_name).to_string();
                let enum_id = ctx.get_enum(*type_name).ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::InternalError(format!(
                            "enum type '{}' not found during pattern conversion",
                            type_name_str
                        )),
                        pattern_span,
                    )
                })?;
                let enum_def = ctx.get_enum_def(enum_id);
                let variant_name = ctx.interner.resolve(&*variant);
                let variant_index = enum_def.find_variant(variant_name).ok_or_else(|| {
                    CompileError::new(
                        ErrorKind::InternalError(format!(
                            "enum variant '{}::{}' not found during pattern conversion",
                            type_name_str, variant_name
                        )),
                        pattern_span,
                    )
                })?;
                AirPattern::EnumVariant {
                    enum_id,
                    variant_index: variant_index as u32,
                }
            }
        };

        air_arms.push((air_pattern, body_result.air_ref));
    }

    // Exhaustiveness checking
    let has_wildcard = wildcard_span.is_some();
    let bool_true_covered = bool_true_span.is_some();
    let bool_false_covered = bool_false_span.is_some();
    let is_exhaustive = if scrutinee_type == Type::Bool {
        has_wildcard || (bool_true_covered && bool_false_covered)
    } else if let Some(enum_id) = pattern_enum_id {
        let enum_def = ctx.get_enum_def(enum_id);
        has_wildcard || covered_variants.len() == enum_def.variant_count()
    } else {
        // For integers, must have wildcard
        has_wildcard
    };

    if !is_exhaustive {
        return Err(CompileError::new(ErrorKind::NonExhaustiveMatch, span));
    }

    let final_type = result_type.unwrap_or(Type::Unit);

    // Encode match arms into extra array
    let arms_len = air_arms.len() as u32;
    let mut extra_data = Vec::new();
    for (pattern, body) in &air_arms {
        pattern.encode(*body, &mut extra_data);
    }
    let arms_start = air.add_extra(&extra_data);

    let air_ref = air.add_inst(AirInst {
        data: AirInstData::Match {
            scrutinee: scrutinee_result.air_ref,
            arms_start,
            arms_len,
        },
        ty: final_type,
        span,
    });
    Ok(AnalysisResult::new(air_ref, final_type))
}

/// Analyze a struct initialization using the shared context.
fn analyze_struct_init_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    type_name: Spur,
    fields_start: u32,
    fields_len: u32,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    use rue_error::MissingFieldsError;

    let field_inits = ctx.rir.get_field_inits(fields_start, fields_len);
    // Look up the struct type
    let type_name_str = ctx.interner.resolve(&type_name);
    let struct_id = ctx
        .get_struct(type_name)
        .ok_or_compile_error(ErrorKind::UnknownType(type_name_str.to_string()), span)?;

    let struct_def = ctx.get_struct_def(struct_id);
    let struct_type = Type::Struct(struct_id);

    // Build a map from field name to struct field index
    let field_index_map: std::collections::HashMap<&str, usize> = struct_def
        .fields
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.as_str(), i))
        .collect();

    // Check for unknown or duplicate fields
    let mut seen_fields = std::collections::HashSet::new();
    for (init_field_name, _) in field_inits.iter() {
        let init_name = ctx.interner.resolve(&*init_field_name);

        if !field_index_map.contains_key(init_name) {
            return Err(CompileError::new(
                ErrorKind::UnknownField {
                    struct_name: struct_def.name.clone(),
                    field_name: init_name.to_string(),
                },
                span,
            ));
        }

        if !seen_fields.insert(init_name) {
            return Err(CompileError::new(
                ErrorKind::DuplicateField {
                    struct_name: struct_def.name.clone(),
                    field_name: init_name.to_string(),
                },
                span,
            ));
        }
    }

    // Check that all fields are provided
    if field_inits.len() != struct_def.fields.len() {
        let missing_fields: Vec<String> = struct_def
            .fields
            .iter()
            .filter(|f| !seen_fields.contains(f.name.as_str()))
            .map(|f| f.name.clone())
            .collect();
        return Err(CompileError::new(
            ErrorKind::MissingFields(Box::new(MissingFieldsError {
                struct_name: struct_def.name.clone(),
                missing_fields,
            })),
            span,
        ));
    }

    // Analyze field values in SOURCE ORDER (left-to-right as written)
    let mut analyzed_fields: Vec<Option<AirRef>> = vec![None; struct_def.fields.len()];
    let mut source_order: Vec<usize> = Vec::with_capacity(field_inits.len());

    for (init_field_name, field_value) in field_inits.iter() {
        let init_name = ctx.interner.resolve(&*init_field_name);
        let field_idx = field_index_map[init_name];

        let field_result = analyze_inst_with_context(ctx, air, *field_value, analysis_ctx)?;
        analyzed_fields[field_idx] = Some(field_result.air_ref);
        source_order.push(field_idx);
    }

    // Collect field refs in DECLARATION ORDER
    let field_refs: Vec<AirRef> = analyzed_fields
        .into_iter()
        .map(|opt| opt.expect("all fields should be initialized"))
        .collect();

    // Encode into extra array
    let fields_len = field_refs.len() as u32;
    let field_u32s: Vec<u32> = field_refs.iter().map(|r| r.as_u32()).collect();
    let fields_start = air.add_extra(&field_u32s);
    let source_order_u32s: Vec<u32> = source_order.iter().map(|&i| i as u32).collect();
    let source_order_start = air.add_extra(&source_order_u32s);

    let air_ref = air.add_inst(AirInst {
        data: AirInstData::StructInit {
            struct_id,
            fields_start,
            fields_len,
            source_order_start,
        },
        ty: struct_type,
        span,
    });
    Ok(AnalysisResult::new(air_ref, struct_type))
}

/// Analyze instruction for projection (field access chains).
/// This differs from analyze_inst_with_context in that it does NOT mark
/// the accessed value as moved - it only checks move state.
fn analyze_inst_for_projection_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    inst_ref: InstRef,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    let inst = ctx.rir.get(inst_ref);

    match &inst.data {
        InstData::VarRef { name } => {
            // First check if it's a parameter
            if let Some(param_info) = analysis_ctx.params.get(name) {
                let ty = param_info.ty;
                let name_str = ctx.interner.resolve(name);

                // Check if this parameter has been moved
                if let Some(move_state) = analysis_ctx.moved_vars.get(name) {
                    if let Some(moved_span) = move_state.is_any_part_moved() {
                        return Err(CompileError::new(
                            ErrorKind::UseAfterMove(name_str.to_string()),
                            inst.span,
                        )
                        .with_label("value moved here", moved_span));
                    }
                }

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Param {
                        index: param_info.abi_slot,
                    },
                    ty,
                    span: inst.span,
                });
                return Ok(AnalysisResult::new(air_ref, ty));
            }

            // Look up the variable in locals
            let name_str = ctx.interner.resolve(name);
            let local = analysis_ctx.locals.get(name).ok_or_compile_error(
                ErrorKind::UndefinedVariable(name_str.to_string()),
                inst.span,
            )?;

            let ty = local.ty;
            let slot = local.slot;

            // Check if this variable has been moved
            if let Some(move_state) = analysis_ctx.moved_vars.get(name) {
                if let Some(moved_span) = move_state.is_any_part_moved() {
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(name_str.to_string()),
                        inst.span,
                    )
                    .with_label("value moved here", moved_span));
                }
            }

            // Mark variable as used
            analysis_ctx.used_locals.insert(*name);

            // Load the variable - but don't mark as moved (projection context)
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Load { slot },
                ty,
                span: inst.span,
            });
            Ok(AnalysisResult::new(air_ref, ty))
        }
        // For field access on projections, recurse into the base
        InstData::FieldGet { base, field } => {
            let base_result = analyze_inst_for_projection_ctx(ctx, air, *base, analysis_ctx)?;
            let base_type = base_result.ty;

            let struct_id = match base_type {
                Type::Struct(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::FieldAccessOnNonStruct {
                            found: base_type.name().to_string(),
                        },
                        inst.span,
                    ));
                }
            };

            let struct_def = ctx.get_struct_def(struct_id);
            let field_name_str = ctx.interner.resolve(field).to_string();

            let (field_index, struct_field) =
                struct_def.find_field(&field_name_str).ok_or_compile_error(
                    ErrorKind::UnknownField {
                        struct_name: struct_def.name.clone(),
                        field_name: field_name_str,
                    },
                    inst.span,
                )?;

            let field_type = struct_field.ty;

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::FieldGet {
                    base: base_result.air_ref,
                    struct_id,
                    field_index: field_index as u32,
                },
                ty: field_type,
                span: inst.span,
            });
            Ok(AnalysisResult::new(air_ref, field_type))
        }
        // For index access on projections
        InstData::IndexGet { base, index } => {
            let base_result = analyze_inst_for_projection_ctx(ctx, air, *base, analysis_ctx)?;
            let base_type = base_result.ty;

            let index_result = analyze_inst_with_context(ctx, air, *index, analysis_ctx)?;

            // Verify base is an array
            let (array_type_id, elem_type, _array_len) = match base_type {
                Type::Array(type_id) => {
                    let array_def = ctx.get_array_type_def(type_id);
                    (type_id, array_def.element_type, array_def.length)
                }
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::IndexOnNonArray {
                            found: base_type.name().to_string(),
                        },
                        inst.span,
                    ));
                }
            };

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::IndexGet {
                    base: base_result.air_ref,
                    array_type: base_type,
                    index: index_result.air_ref,
                },
                ty: elem_type,
                span: inst.span,
            });
            Ok(AnalysisResult::new(air_ref, elem_type))
        }
        // For other instructions, just call the regular analysis
        _ => analyze_inst_with_context(ctx, air, inst_ref, analysis_ctx),
    }
}

/// Extract the root variable from a field access chain.
fn extract_root_variable_ctx(ctx: &SemaContext<'_>, inst_ref: InstRef) -> Option<Spur> {
    let inst = ctx.rir.get(inst_ref);
    match &inst.data {
        InstData::VarRef { name } => Some(*name),
        InstData::ParamRef { name, .. } => Some(*name),
        InstData::FieldGet { base, .. } => extract_root_variable_ctx(ctx, *base),
        InstData::IndexGet { base, .. } => extract_root_variable_ctx(ctx, *base),
        _ => None,
    }
}

/// Extract field path from a field access chain.
fn extract_field_path_ctx(ctx: &SemaContext<'_>, inst_ref: InstRef) -> Option<(Spur, FieldPath)> {
    let inst = ctx.rir.get(inst_ref);
    match &inst.data {
        InstData::VarRef { name } => Some((*name, Vec::new())),
        InstData::ParamRef { name, .. } => Some((*name, Vec::new())),
        InstData::FieldGet { base, field } => {
            let (root, mut path) = extract_field_path_ctx(ctx, *base)?;
            path.push(*field);
            Some((root, path))
        }
        _ => None,
    }
}

/// Analyze a field access using the shared context.
fn analyze_field_get_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    inst_ref: InstRef,
    base: InstRef,
    field: Spur,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    // Field access is a projection
    let base_result = analyze_inst_for_projection_ctx(ctx, air, base, analysis_ctx)?;
    let base_type = base_result.ty;

    let struct_id = match base_type {
        Type::Struct(id) => id,
        _ => {
            return Err(CompileError::new(
                ErrorKind::FieldAccessOnNonStruct {
                    found: base_type.name().to_string(),
                },
                span,
            ));
        }
    };

    let struct_def = ctx.get_struct_def(struct_id);
    let is_linear = struct_def.is_linear;
    let field_name_str = ctx.interner.resolve(&field).to_string();

    let (field_index, struct_field) = struct_def.find_field(&field_name_str).ok_or_compile_error(
        ErrorKind::UnknownField {
            struct_name: struct_def.name.clone(),
            field_name: field_name_str.clone(),
        },
        span,
    )?;

    let field_type = struct_field.ty;

    // For linear types, field access consumes the entire struct.
    if is_linear {
        if let Some(root_var) = extract_root_variable_ctx(ctx, inst_ref) {
            analysis_ctx
                .moved_vars
                .entry(root_var)
                .or_default()
                .mark_path_moved(&[], span);
        }
    }
    // For non-linear types, check if accessing a non-Copy field
    else if !ctx.is_type_copy(field_type) {
        if let Some((root_var, field_path)) = extract_field_path_ctx(ctx, inst_ref) {
            // Check if this field path is already moved
            if let Some(state) = analysis_ctx.moved_vars.get(&root_var) {
                if let Some(moved_span) = state.is_path_moved(&field_path) {
                    let root_name = ctx.interner.resolve(&root_var);
                    let path_str = if field_path.is_empty() {
                        root_name.to_string()
                    } else {
                        let field_names: Vec<_> = field_path
                            .iter()
                            .map(|s| ctx.interner.resolve(s).to_string())
                            .collect();
                        format!("{}.{}", root_name, field_names.join("."))
                    };
                    return Err(CompileError::new(ErrorKind::UseAfterMove(path_str), span)
                        .with_label("value moved here", moved_span));
                }
            }

            // Mark this field path as moved
            analysis_ctx
                .moved_vars
                .entry(root_var)
                .or_default()
                .mark_path_moved(&field_path, span);
        }
    }

    let air_ref = air.add_inst(AirInst {
        data: AirInstData::FieldGet {
            base: base_result.air_ref,
            struct_id,
            field_index: field_index as u32,
        },
        ty: field_type,
        span,
    });
    Ok(AnalysisResult::new(air_ref, field_type))
}

/// Analyze a field assignment using the shared context.
fn analyze_field_set_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    base: InstRef,
    field: Spur,
    value: InstRef,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    // For field assignment, we need to walk up the chain of field accesses
    // to find the root variable. We accumulate the slot offset as we go.

    // Walk up to find the root variable, collecting field symbols
    let mut current_base = base;
    let mut field_symbols: Vec<Spur> = Vec::new();

    // Result is either (Local, slot, type, is_mut, name) or (Param, abi_slot, type, mode, name)
    enum RootKind {
        Local { slot: u32, is_mut: bool },
        Param { abi_slot: u32, mode: RirParamMode },
    }

    let (var_name, root_kind, root_type, _root_symbol) = loop {
        let current_inst = ctx.rir.get(current_base);
        match &current_inst.data {
            InstData::VarRef { name } => {
                let name_str = ctx.interner.resolve(&*name);

                // Check if this variable has been moved
                if let Some(move_state) = analysis_ctx.moved_vars.get(name) {
                    if let Some(moved_span) = move_state.is_any_part_moved() {
                        return Err(CompileError::new(
                            ErrorKind::UseAfterMove(name_str.to_string()),
                            span,
                        )
                        .with_label("value moved here", moved_span));
                    }
                }

                // First check if it's a parameter
                if let Some(param_info) = analysis_ctx.params.get(name) {
                    break (
                        name_str.to_string(),
                        RootKind::Param {
                            abi_slot: param_info.abi_slot,
                            mode: param_info.mode,
                        },
                        param_info.ty,
                        *name,
                    );
                }

                // Then check locals
                let local = analysis_ctx.locals.get(name).ok_or_compile_error(
                    ErrorKind::UndefinedVariable(name_str.to_string()),
                    span,
                )?;

                break (
                    name_str.to_string(),
                    RootKind::Local {
                        slot: local.slot,
                        is_mut: local.is_mut,
                    },
                    local.ty,
                    *name,
                );
            }
            InstData::ParamRef { name, .. } => {
                let name_str = ctx.interner.resolve(&*name);
                let param_info = analysis_ctx.params.get(name).ok_or_compile_error(
                    ErrorKind::UndefinedVariable(name_str.to_string()),
                    span,
                )?;

                // Check if this parameter has been moved
                if let Some(move_state) = analysis_ctx.moved_vars.get(name) {
                    if let Some(moved_span) = move_state.is_any_part_moved() {
                        return Err(CompileError::new(
                            ErrorKind::UseAfterMove(name_str.to_string()),
                            span,
                        )
                        .with_label("value moved here", moved_span));
                    }
                }

                break (
                    name_str.to_string(),
                    RootKind::Param {
                        abi_slot: param_info.abi_slot,
                        mode: param_info.mode,
                    },
                    param_info.ty,
                    *name,
                );
            }
            InstData::FieldGet {
                base: inner_base,
                field: inner_field,
            } => {
                field_symbols.push(*inner_field);
                current_base = *inner_base;
            }
            _ => {
                return Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span));
            }
        }
    };

    // Check mutability based on root kind
    let root_slot = match root_kind {
        RootKind::Local { slot, is_mut } => {
            if !is_mut {
                return Err(CompileError::new(
                    ErrorKind::AssignToImmutable(var_name),
                    span,
                ));
            }
            slot
        }
        RootKind::Param { abi_slot, mode } => {
            match mode {
                RirParamMode::Normal => {
                    return Err(CompileError::new(
                        ErrorKind::AssignToImmutable(var_name.clone()),
                        span,
                    )
                    .with_help(format!(
                        "consider making parameter `{}` inout: `inout {}: {}`",
                        var_name,
                        var_name,
                        root_type.name()
                    )));
                }
                RirParamMode::Inout => {
                    // Inout parameters can be mutated
                }
                RirParamMode::Borrow => {
                    return Err(CompileError::new(
                        ErrorKind::MutateBorrowedValue { variable: var_name },
                        span,
                    ));
                }
            }
            abi_slot
        }
    };

    // Walk through the field chain to compute the slot offset
    let mut current_type = root_type;
    let mut slot_offset: u32 = 0;

    for field_sym in field_symbols.iter().rev() {
        let struct_id = match current_type {
            Type::Struct(id) => id,
            _ => {
                return Err(CompileError::new(
                    ErrorKind::FieldAccessOnNonStruct {
                        found: current_type.name().to_string(),
                    },
                    span,
                ));
            }
        };

        let struct_def = ctx.get_struct_def(struct_id);
        let field_name_str = ctx.interner.resolve(&*field_sym).to_string();

        let (field_index, struct_field) =
            struct_def.find_field(&field_name_str).ok_or_compile_error(
                ErrorKind::UnknownField {
                    struct_name: struct_def.name.clone(),
                    field_name: field_name_str.clone(),
                },
                span,
            )?;

        slot_offset += ctx.field_slot_offset(struct_id, field_index);
        current_type = struct_field.ty;
    }

    // Now handle the final field being assigned
    let struct_id = match current_type {
        Type::Struct(id) => id,
        _ => {
            return Err(CompileError::new(
                ErrorKind::FieldAccessOnNonStruct {
                    found: current_type.name().to_string(),
                },
                span,
            ));
        }
    };

    let struct_def = ctx.get_struct_def(struct_id);
    let field_name_str = ctx.interner.resolve(&field).to_string();

    let (field_index, _struct_field) = struct_def.find_field(&field_name_str).ok_or_compile_error(
        ErrorKind::UnknownField {
            struct_name: struct_def.name.clone(),
            field_name: field_name_str.clone(),
        },
        span,
    )?;

    // Analyze the value
    let value_result = analyze_inst_with_context(ctx, air, value, analysis_ctx)?;

    // Emit the appropriate instruction based on whether root is a local or param
    let air_ref = match root_kind {
        RootKind::Local { slot, .. } => {
            let base_slot = slot + slot_offset;
            air.add_inst(AirInst {
                data: AirInstData::FieldSet {
                    slot: base_slot,
                    struct_id,
                    field_index: field_index as u32,
                    value: value_result.air_ref,
                },
                ty: Type::Unit,
                span,
            })
        }
        RootKind::Param { abi_slot, .. } => air.add_inst(AirInst {
            data: AirInstData::ParamFieldSet {
                param_slot: abi_slot,
                inner_offset: slot_offset,
                struct_id,
                field_index: field_index as u32,
                value: value_result.air_ref,
            },
            ty: Type::Unit,
            span,
        }),
    };
    Ok(AnalysisResult::new(air_ref, Type::Unit))
}

/// Analyze an array initialization using the shared context.
fn analyze_array_init_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    inst_ref: InstRef,
    elems_start: u32,
    elems_len: u32,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    let elem_refs = ctx.rir.get_inst_refs(elems_start, elems_len);

    // Get the array type from HM inference
    let array_type = get_resolved_type_ctx(analysis_ctx, inst_ref, span, "array literal")?;

    let (array_type_id, _elem_type, expected_len) = match array_type {
        Type::Array(type_id) => {
            let array_def = ctx.get_array_type_def(type_id);
            (type_id, array_def.element_type, array_def.length)
        }
        _ => {
            return Err(CompileError::new(
                ErrorKind::InternalError(format!(
                    "Array literal inferred as non-array type: {}",
                    array_type.name()
                )),
                span,
            ));
        }
    };

    // Verify length matches
    if elem_refs.len() as u64 != expected_len {
        return Err(CompileError::new(
            ErrorKind::ArrayLengthMismatch {
                expected: expected_len,
                found: elem_refs.len() as u64,
            },
            span,
        ));
    }

    // Analyze elements
    let mut air_elems = Vec::with_capacity(elem_refs.len());
    for elem_ref in elem_refs {
        let elem_result = analyze_inst_with_context(ctx, air, elem_ref, analysis_ctx)?;
        air_elems.push(elem_result.air_ref);
    }

    // Encode into extra array
    let elems_len = air_elems.len() as u32;
    let elem_u32s: Vec<u32> = air_elems.iter().map(|r| r.as_u32()).collect();
    let elems_start = air.add_extra(&elem_u32s);

    let air_ref = air.add_inst(AirInst {
        data: AirInstData::ArrayInit {
            elems_start,
            elems_len,
        },
        ty: array_type,
        span,
    });
    Ok(AnalysisResult::new(air_ref, array_type))
}

/// Try to extract a constant integer value from an index expression.
fn try_get_const_index_ctx(ctx: &SemaContext<'_>, index: InstRef) -> Option<i64> {
    let inst = ctx.rir.get(index);
    match &inst.data {
        InstData::IntConst(value) => Some(*value as i64),
        InstData::Neg { operand } => {
            let inner = ctx.rir.get(*operand);
            if let InstData::IntConst(value) = &inner.data {
                Some(-(*value as i64))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Analyze an array index read using the shared context.
fn analyze_index_get_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    base: InstRef,
    index: InstRef,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    // Analyze base and index
    let base_result = analyze_inst_for_projection_ctx(ctx, air, base, analysis_ctx)?;
    let base_type = base_result.ty;

    let index_result = analyze_inst_with_context(ctx, air, index, analysis_ctx)?;

    // Verify base is an array
    let (array_type_id, elem_type, array_len) = match base_type {
        Type::Array(type_id) => {
            let array_def = ctx.get_array_type_def(type_id);
            (type_id, array_def.element_type, array_def.length)
        }
        _ => {
            return Err(CompileError::new(
                ErrorKind::IndexOnNonArray {
                    found: base_type.name().to_string(),
                },
                span,
            ));
        }
    };

    // Check for constant out-of-bounds index
    if let Some(const_idx) = try_get_const_index_ctx(ctx, index) {
        if const_idx < 0 || const_idx as u64 >= array_len {
            return Err(CompileError::new(
                ErrorKind::IndexOutOfBounds {
                    index: const_idx,
                    length: array_len,
                },
                ctx.rir.get(index).span,
            ));
        }
    }

    // Prevent moving non-Copy elements out of arrays.
    if !ctx.is_type_copy(elem_type) {
        return Err(CompileError::new(
            ErrorKind::MoveOutOfIndex {
                element_type: elem_type.name().to_string(),
            },
            span,
        )
        .with_help("use explicit methods like swap() or take() to remove elements"));
    }

    let air_ref = air.add_inst(AirInst {
        data: AirInstData::IndexGet {
            base: base_result.air_ref,
            array_type: base_type,
            index: index_result.air_ref,
        },
        ty: elem_type,
        span,
    });
    Ok(AnalysisResult::new(air_ref, elem_type))
}

/// Analyze an array index write using the shared context.
fn analyze_index_set_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    base: InstRef,
    index: InstRef,
    value: InstRef,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    let base_inst = ctx.rir.get(base);

    enum IndexSetRootKind {
        Local { slot: u32, is_mut: bool },
        Param { abi_slot: u32, mode: RirParamMode },
    }

    let (var_name, root_kind, base_type) = match &base_inst.data {
        InstData::VarRef { name } => {
            let name_str = ctx.interner.resolve(&*name);

            // Check if this variable has been moved
            if let Some(move_state) = analysis_ctx.moved_vars.get(name) {
                if let Some(moved_span) = move_state.is_any_part_moved() {
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(name_str.to_string()),
                        span,
                    )
                    .with_label("value moved here", moved_span));
                }
            }

            // First check if it's a parameter
            if let Some(param_info) = analysis_ctx.params.get(name) {
                (
                    name_str.to_string(),
                    IndexSetRootKind::Param {
                        abi_slot: param_info.abi_slot,
                        mode: param_info.mode,
                    },
                    param_info.ty,
                )
            } else {
                // Then check locals
                let local = analysis_ctx.locals.get(name).ok_or_compile_error(
                    ErrorKind::UndefinedVariable(name_str.to_string()),
                    span,
                )?;

                (
                    name_str.to_string(),
                    IndexSetRootKind::Local {
                        slot: local.slot,
                        is_mut: local.is_mut,
                    },
                    local.ty,
                )
            }
        }
        InstData::ParamRef { name, .. } => {
            let name_str = ctx.interner.resolve(&*name);
            let param_info = analysis_ctx
                .params
                .get(name)
                .ok_or_compile_error(ErrorKind::UndefinedVariable(name_str.to_string()), span)?;

            // Check if this parameter has been moved
            if let Some(move_state) = analysis_ctx.moved_vars.get(name) {
                if let Some(moved_span) = move_state.is_any_part_moved() {
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(name_str.to_string()),
                        span,
                    )
                    .with_label("value moved here", moved_span));
                }
            }

            (
                name_str.to_string(),
                IndexSetRootKind::Param {
                    abi_slot: param_info.abi_slot,
                    mode: param_info.mode,
                },
                param_info.ty,
            )
        }
        _ => {
            return Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span));
        }
    };

    // Check mutability based on root kind
    let (is_inout_param, slot) = match root_kind {
        IndexSetRootKind::Local { slot, is_mut } => {
            if !is_mut {
                return Err(CompileError::new(
                    ErrorKind::AssignToImmutable(var_name),
                    span,
                ));
            }
            (false, slot)
        }
        IndexSetRootKind::Param { abi_slot, mode } => {
            let is_inout = match mode {
                RirParamMode::Normal => false,
                RirParamMode::Inout => true,
                RirParamMode::Borrow => {
                    return Err(CompileError::new(
                        ErrorKind::MutateBorrowedValue { variable: var_name },
                        span,
                    ));
                }
            };
            if !is_inout {
                return Err(CompileError::new(
                    ErrorKind::AssignToImmutable(var_name.clone()),
                    span,
                )
                .with_help(format!(
                    "consider making parameter `{}` inout: `inout {}: {}`",
                    var_name,
                    var_name,
                    base_type.name()
                )));
            }
            (true, abi_slot)
        }
    };

    // Verify base is an array and get its element type
    let (array_type_id, _elem_type, array_len) = match base_type {
        Type::Array(type_id) => {
            let array_def = ctx.get_array_type_def(type_id);
            (type_id, array_def.element_type, array_def.length)
        }
        _ => {
            return Err(CompileError::new(
                ErrorKind::IndexOnNonArray {
                    found: base_type.name().to_string(),
                },
                span,
            ));
        }
    };

    // Analyze the index expression
    let index_result = analyze_inst_with_context(ctx, air, index, analysis_ctx)?;

    // Check for constant out-of-bounds index
    if let Some(const_idx) = try_get_const_index_ctx(ctx, index) {
        if const_idx < 0 || const_idx as u64 >= array_len {
            return Err(CompileError::new(
                ErrorKind::IndexOutOfBounds {
                    index: const_idx,
                    length: array_len,
                },
                ctx.rir.get(index).span,
            ));
        }
    }

    // Analyze the value expression
    let value_result = analyze_inst_with_context(ctx, air, value, analysis_ctx)?;

    // Emit the appropriate instruction
    let air_ref = if is_inout_param {
        air.add_inst(AirInst {
            data: AirInstData::ParamIndexSet {
                param_slot: slot,
                array_type: base_type,
                index: index_result.air_ref,
                value: value_result.air_ref,
            },
            ty: Type::Unit,
            span,
        })
    } else {
        air.add_inst(AirInst {
            data: AirInstData::IndexSet {
                slot,
                array_type: base_type,
                index: index_result.air_ref,
                value: value_result.air_ref,
            },
            ty: Type::Unit,
            span,
        })
    };
    Ok(AnalysisResult::new(air_ref, Type::Unit))
}

/// Analyze an enum variant using the shared context.
fn analyze_enum_variant_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    type_name: Spur,
    variant: Spur,
    span: Span,
) -> CompileResult<AnalysisResult> {
    // Look up the enum type
    let enum_id = ctx.get_enum(type_name).ok_or_compile_error(
        ErrorKind::UnknownEnumType(ctx.interner.resolve(&type_name).to_string()),
        span,
    )?;
    let enum_def = ctx.get_enum_def(enum_id);

    // Find the variant index
    let variant_name = ctx.interner.resolve(&variant);
    let variant_index = enum_def.find_variant(variant_name).ok_or_compile_error(
        ErrorKind::UnknownVariant {
            enum_name: enum_def.name.clone(),
            variant_name: variant_name.to_string(),
        },
        span,
    )?;

    let ty = Type::Enum(enum_id);

    let air_ref = air.add_inst(AirInst {
        data: AirInstData::EnumVariant {
            enum_id,
            variant_index: variant_index as u32,
        },
        ty,
        span,
    });
    Ok(AnalysisResult::new(air_ref, ty))
}

/// Analyze a function call using the shared context.
fn analyze_call_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    name: Spur,
    args_start: u32,
    args_len: u32,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    // Look up the function
    let fn_name_str = ctx.interner.resolve(&name).to_string();
    let fn_info = ctx
        .get_function(name)
        .ok_or_compile_error(ErrorKind::UndefinedFunction(fn_name_str.clone()), span)?;

    let args = ctx.rir.get_call_args(args_start, args_len);
    // Check argument count
    if args.len() != fn_info.param_types.len() {
        let expected = fn_info.param_types.len();
        let found = args.len();
        return Err(CompileError::new(
            ErrorKind::WrongArgumentCount { expected, found },
            span,
        ));
    }

    // Check for exclusive access violation
    check_exclusive_access_ctx(ctx, &args, span)?;

    // Check that call-site argument modes match function parameter modes
    for (i, (arg, expected_mode)) in args.iter().zip(fn_info.param_modes.iter()).enumerate() {
        match expected_mode {
            RirParamMode::Inout => {
                if arg.mode != RirArgMode::Inout {
                    return Err(CompileError::new(
                        ErrorKind::InoutKeywordMissing,
                        ctx.rir.get(args[i].value).span,
                    ));
                }
            }
            RirParamMode::Borrow => {
                if arg.mode != RirArgMode::Borrow {
                    return Err(CompileError::new(
                        ErrorKind::BorrowKeywordMissing,
                        ctx.rir.get(args[i].value).span,
                    ));
                }
            }
            RirParamMode::Normal => {
                // Normal params accept any mode
            }
        }
    }

    // Check that comptime parameters receive compile-time constant values
    let has_comptime_params = fn_info.param_comptime.iter().any(|&c| c);
    if has_comptime_params {
        // Gate behind comptime preview feature
        if !ctx.preview_features.contains(&PreviewFeature::Comptime) {
            return Err(CompileError::new(
                ErrorKind::PreviewFeatureRequired {
                    feature: PreviewFeature::Comptime,
                    what: "comptime parameters".to_string(),
                },
                span,
            )
            .with_help(format!(
                "use `--preview {}` to enable this feature ({})",
                PreviewFeature::Comptime.name(),
                PreviewFeature::Comptime.adr()
            )));
        }

        // Validate each comptime parameter receives a compile-time constant
        for (i, (&is_comptime, arg)) in fn_info.param_comptime.iter().zip(args.iter()).enumerate() {
            if is_comptime {
                // Try to evaluate the argument at compile time
                if try_evaluate_const_in_rir(ctx.rir, arg.value).is_none() {
                    let param_name = ctx.interner.resolve(&fn_info.param_names[i]).to_string();
                    return Err(CompileError::new(
                        ErrorKind::ComptimeArgNotConst {
                            param_name: param_name.clone(),
                        },
                        ctx.rir.get(arg.value).span,
                    )
                    .with_help(format!(
                        "parameter '{}' is declared as 'comptime' and requires a compile-time known value",
                        param_name
                    )));
                }
            }
        }
    }

    // Extract return_type before mutable borrow
    let return_type = fn_info.return_type;

    // Analyze arguments
    let air_args = analyze_call_args_ctx(ctx, air, &args, analysis_ctx)?;

    // Encode call args into extra array
    let args_len = air_args.len() as u32;
    let mut extra_data = Vec::with_capacity(air_args.len() * 2);
    for arg in &air_args {
        extra_data.push(arg.value.as_u32());
        extra_data.push(arg.mode.as_u32());
    }
    let args_start = air.add_extra(&extra_data);

    let air_ref = air.add_inst(AirInst {
        data: AirInstData::Call {
            name,
            args_start,
            args_len,
        },
        ty: return_type,
        span,
    });
    Ok(AnalysisResult::new(air_ref, return_type))
}

/// Check for exclusive access violations in call arguments.
fn check_exclusive_access_ctx(
    ctx: &SemaContext<'_>,
    args: &[RirCallArg],
    _span: Span,
) -> CompileResult<()> {
    let mut inout_vars: HashMap<Spur, Span> = HashMap::new();

    for arg in args {
        if arg.mode == RirArgMode::Inout {
            // Extract the variable name from the argument
            if let Some(var_name) = extract_arg_var_name_ctx(ctx, arg.value) {
                if let Some(first_span) = inout_vars.get(&var_name) {
                    let var_name_str = ctx.interner.resolve(&var_name);
                    return Err(CompileError::new(
                        ErrorKind::InoutExclusiveAccess {
                            variable: var_name_str.to_string(),
                        },
                        ctx.rir.get(arg.value).span,
                    )
                    .with_label("first inout borrow here", *first_span)
                    .with_note("a variable can only be passed as inout once per function call"));
                }
                inout_vars.insert(var_name, ctx.rir.get(arg.value).span);
            }
        }
    }
    Ok(())
}

/// Extract the variable name from an argument expression.
fn extract_arg_var_name_ctx(ctx: &SemaContext<'_>, inst_ref: InstRef) -> Option<Spur> {
    let inst = ctx.rir.get(inst_ref);
    match &inst.data {
        InstData::VarRef { name } => Some(*name),
        InstData::ParamRef { name, .. } => Some(*name),
        _ => None,
    }
}

/// Analyze call arguments using the shared context.
fn analyze_call_args_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    args: &[RirCallArg],
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<Vec<AirCallArg>> {
    let mut air_args = Vec::with_capacity(args.len());

    for arg in args {
        let arg_result = analyze_inst_with_context(ctx, air, arg.value, analysis_ctx)?;
        let air_mode = match arg.mode {
            RirArgMode::Normal => AirArgMode::Normal,
            RirArgMode::Inout => AirArgMode::Inout,
            RirArgMode::Borrow => AirArgMode::Borrow,
        };
        air_args.push(AirCallArg {
            value: arg_result.air_ref,
            mode: air_mode,
        });
    }

    Ok(air_args)
}

/// Analyze a method call using the shared context.
fn analyze_method_call_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    receiver: InstRef,
    method: Spur,
    args_start: u32,
    args_len: u32,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    // First analyze the receiver to get its type
    let receiver_result = analyze_inst_with_context(ctx, air, receiver, analysis_ctx)?;
    let receiver_type = receiver_result.ty;

    // Get the type name for method lookup
    let type_name = match receiver_type {
        Type::Struct(struct_id) => {
            let struct_def = ctx.get_struct_def(struct_id);
            ctx.interner.get_or_intern(&struct_def.name)
        }
        _ => {
            let method_name = ctx.interner.resolve(&method);
            return Err(CompileError::new(
                ErrorKind::MethodCallOnNonStruct {
                    method_name: method_name.to_string(),
                    found: receiver_type.name().to_string(),
                },
                span,
            ));
        }
    };

    // Check if this is a builtin type with builtin methods
    if let Type::Struct(struct_id) = receiver_type {
        if let Some(builtin_def) = ctx.get_builtin_type_def(struct_id) {
            let method_name = ctx.interner.resolve(&method);
            if let Some(builtin_method) = builtin_def.find_method(method_name) {
                // Handle builtin method call
                return analyze_builtin_method_ctx(
                    ctx,
                    air,
                    receiver,
                    receiver_result.air_ref,
                    receiver_type,
                    builtin_method,
                    args_start,
                    args_len,
                    span,
                    analysis_ctx,
                );
            }
        }
    }

    // Look up the method in user-defined methods
    let method_info = ctx.get_method(type_name, method).ok_or_compile_error(
        ErrorKind::UndefinedMethod {
            type_name: ctx.interner.resolve(&type_name).to_string(),
            method_name: ctx.interner.resolve(&method).to_string(),
        },
        span,
    )?;

    // Analyze arguments
    let args = ctx.rir.get_call_args(args_start, args_len);
    let air_args = analyze_call_args_ctx(ctx, air, &args, analysis_ctx)?;

    // Create the full name for the method
    let type_name_str = ctx.interner.resolve(&type_name);
    let method_name_str = ctx.interner.resolve(&method);
    let full_name = format!("{}::{}", type_name_str, method_name_str);
    let full_name_sym = ctx.interner.get_or_intern(&full_name);

    let return_type = method_info.return_type;

    // Encode call args with receiver
    let mut extra_data = Vec::with_capacity((air_args.len() + 1) * 2);
    // Add receiver as first argument
    extra_data.push(receiver_result.air_ref.as_u32());
    extra_data.push(AirArgMode::Normal.as_u32());
    // Add other arguments
    for arg in &air_args {
        extra_data.push(arg.value.as_u32());
        extra_data.push(arg.mode.as_u32());
    }
    let args_start = air.add_extra(&extra_data);
    let args_len = (air_args.len() + 1) as u32;

    let air_ref = air.add_inst(AirInst {
        data: AirInstData::Call {
            name: full_name_sym,
            args_start,
            args_len,
        },
        ty: return_type,
        span,
    });
    Ok(AnalysisResult::new(air_ref, return_type))
}

/// Analyze a builtin method call using the shared context.
fn analyze_builtin_method_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    receiver: InstRef,
    receiver_air_ref: AirRef,
    receiver_type: Type,
    builtin_method: &'static rue_builtins::BuiltinMethod,
    args_start: u32,
    args_len: u32,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    use rue_builtins::ReceiverMode;

    let args = ctx.rir.get_call_args(args_start, args_len);

    // Check argument count
    if args.len() != builtin_method.params.len() {
        return Err(CompileError::new(
            ErrorKind::WrongArgumentCount {
                expected: builtin_method.params.len(),
                found: args.len(),
            },
            span,
        ));
    }

    // Analyze arguments
    let air_args = analyze_call_args_ctx(ctx, air, &args, analysis_ctx)?;

    // Get the struct ID for builtin type
    let struct_id = match receiver_type {
        Type::Struct(id) => id,
        _ => unreachable!("builtin method called on non-struct type"),
    };

    // Resolve return type
    let return_type = resolve_builtin_return_type_ctx(ctx, builtin_method.return_ty, struct_id);

    // For mutation methods, we need to handle the storage update
    if builtin_method.receiver_mode == ReceiverMode::ByMutRef {
        // Find the storage location for the receiver
        let storage = get_receiver_storage_ctx(ctx, receiver, span, analysis_ctx)?;

        // Build the call instruction
        let full_name_sym = ctx.interner.get_or_intern(builtin_method.runtime_fn);

        // Encode call args with receiver
        let mut extra_data = Vec::with_capacity((air_args.len() + 1) * 2);
        extra_data.push(receiver_air_ref.as_u32());
        extra_data.push(AirArgMode::Normal.as_u32());
        for arg in &air_args {
            extra_data.push(arg.value.as_u32());
            extra_data.push(arg.mode.as_u32());
        }
        let call_args_start = air.add_extra(&extra_data);
        let call_args_len = (air_args.len() + 1) as u32;

        // Call the runtime function - it returns the new value
        let call_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: full_name_sym,
                args_start: call_args_start,
                args_len: call_args_len,
            },
            ty: receiver_type,
            span,
        });

        // Store the result back to the receiver location
        let store_ref = match storage {
            StringReceiverStorage::Local { slot } => air.add_inst(AirInst {
                data: AirInstData::Store {
                    slot,
                    value: call_ref,
                },
                ty: Type::Unit,
                span,
            }),
            StringReceiverStorage::Param { abi_slot } => air.add_inst(AirInst {
                data: AirInstData::ParamStore {
                    param_slot: abi_slot,
                    value: call_ref,
                },
                ty: Type::Unit,
                span,
            }),
        };

        // If return type is the receiver type, return the stored value
        // Otherwise return the call result (e.g., for pop() which returns the popped char)
        if return_type == receiver_type {
            // Return unit since mutation methods that return Self are for chaining
            Ok(AnalysisResult::new(store_ref, Type::Unit))
        } else {
            // Return the actual return value
            Ok(AnalysisResult::new(call_ref, return_type))
        }
    } else {
        // Non-mutation method - just call and return
        let full_name_sym = ctx.interner.get_or_intern(builtin_method.runtime_fn);

        let mut extra_data = Vec::with_capacity((air_args.len() + 1) * 2);
        extra_data.push(receiver_air_ref.as_u32());
        extra_data.push(AirArgMode::Normal.as_u32());
        for arg in &air_args {
            extra_data.push(arg.value.as_u32());
            extra_data.push(arg.mode.as_u32());
        }
        let call_args_start = air.add_extra(&extra_data);
        let call_args_len = (air_args.len() + 1) as u32;

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: full_name_sym,
                args_start: call_args_start,
                args_len: call_args_len,
            },
            ty: return_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, return_type))
    }
}

/// Get the storage location for a receiver expression.
fn get_receiver_storage_ctx(
    ctx: &SemaContext<'_>,
    receiver: InstRef,
    span: Span,
    analysis_ctx: &AnalysisContext,
) -> CompileResult<StringReceiverStorage> {
    let inst = ctx.rir.get(receiver);
    match &inst.data {
        InstData::VarRef { name } => {
            // Check if it's a parameter
            if let Some(param_info) = analysis_ctx.params.get(name) {
                // Check parameter mode
                match param_info.mode {
                    RirParamMode::Inout => {
                        return Ok(StringReceiverStorage::Param {
                            abi_slot: param_info.abi_slot,
                        });
                    }
                    RirParamMode::Borrow => {
                        let name_str = ctx.interner.resolve(name);
                        return Err(CompileError::new(
                            ErrorKind::MutateBorrowedValue {
                                variable: name_str.to_string(),
                            },
                            span,
                        ));
                    }
                    RirParamMode::Normal => {
                        let name_str = ctx.interner.resolve(name);
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(name_str.to_string()),
                            span,
                        ));
                    }
                }
            }
            // Check locals
            if let Some(local) = analysis_ctx.locals.get(name) {
                let name_str = ctx.interner.resolve(name);
                if !local.is_mut {
                    return Err(CompileError::new(
                        ErrorKind::AssignToImmutable(name_str.to_string()),
                        span,
                    ));
                }
                return Ok(StringReceiverStorage::Local { slot: local.slot });
            }
            let name_str = ctx.interner.resolve(name);
            Err(CompileError::new(
                ErrorKind::UndefinedVariable(name_str.to_string()),
                span,
            ))
        }
        InstData::ParamRef { name, .. } => {
            if let Some(param_info) = analysis_ctx.params.get(name) {
                Ok(StringReceiverStorage::Param {
                    abi_slot: param_info.abi_slot,
                })
            } else {
                let name_str = ctx.interner.resolve(name);
                Err(CompileError::new(
                    ErrorKind::UndefinedVariable(name_str.to_string()),
                    span,
                ))
            }
        }
        _ => Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span)),
    }
}

/// Resolve a builtin return type to a concrete Type.
fn resolve_builtin_return_type_ctx(
    ctx: &SemaContext<'_>,
    return_type: BuiltinReturnType,
    self_struct_id: StructId,
) -> Type {
    match return_type {
        BuiltinReturnType::Unit => Type::Unit,
        BuiltinReturnType::U64 => Type::U64,
        BuiltinReturnType::U8 => Type::U8,
        BuiltinReturnType::Bool => Type::Bool,
        BuiltinReturnType::SelfType => ctx.builtin_air_type(self_struct_id),
    }
}

/// Analyze an associated function call using the shared context.
fn analyze_assoc_fn_call_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    type_name: Spur,
    function: Spur,
    args_start: u32,
    args_len: u32,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    // Check if this is a builtin type with builtin associated functions
    if let Some(struct_id) = ctx.get_struct(type_name) {
        if let Some(builtin_def) = ctx.get_builtin_type_def(struct_id) {
            let fn_name = ctx.interner.resolve(&function);
            if let Some(assoc_fn) = builtin_def.find_associated_fn(fn_name) {
                // Handle builtin associated function
                return analyze_builtin_assoc_fn_ctx(
                    ctx,
                    air,
                    struct_id,
                    assoc_fn,
                    args_start,
                    args_len,
                    span,
                    analysis_ctx,
                );
            }
        }
    }

    // Look up user-defined associated function
    let method_info = ctx.get_method(type_name, function).ok_or_compile_error(
        ErrorKind::UndefinedMethod {
            type_name: ctx.interner.resolve(&type_name).to_string(),
            method_name: ctx.interner.resolve(&function).to_string(),
        },
        span,
    )?;

    // Analyze arguments
    let args = ctx.rir.get_call_args(args_start, args_len);
    let air_args = analyze_call_args_ctx(ctx, air, &args, analysis_ctx)?;

    // Create the full name
    let type_name_str = ctx.interner.resolve(&type_name);
    let fn_name_str = ctx.interner.resolve(&function);
    let full_name = format!("{}::{}", type_name_str, fn_name_str);
    let full_name_sym = ctx.interner.get_or_intern(&full_name);

    let return_type = method_info.return_type;

    // Encode call args
    let mut extra_data = Vec::with_capacity(air_args.len() * 2);
    for arg in &air_args {
        extra_data.push(arg.value.as_u32());
        extra_data.push(arg.mode.as_u32());
    }
    let call_args_start = air.add_extra(&extra_data);
    let call_args_len = air_args.len() as u32;

    let air_ref = air.add_inst(AirInst {
        data: AirInstData::Call {
            name: full_name_sym,
            args_start: call_args_start,
            args_len: call_args_len,
        },
        ty: return_type,
        span,
    });
    Ok(AnalysisResult::new(air_ref, return_type))
}

/// Analyze a builtin associated function call.
fn analyze_builtin_assoc_fn_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    struct_id: StructId,
    assoc_fn: &'static rue_builtins::BuiltinAssociatedFn,
    args_start: u32,
    args_len: u32,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    let args = ctx.rir.get_call_args(args_start, args_len);

    // Check argument count
    if args.len() != assoc_fn.params.len() {
        return Err(CompileError::new(
            ErrorKind::WrongArgumentCount {
                expected: assoc_fn.params.len(),
                found: args.len(),
            },
            span,
        ));
    }

    // Analyze arguments
    let air_args = analyze_call_args_ctx(ctx, air, &args, analysis_ctx)?;

    // Resolve return type
    let return_type = resolve_builtin_return_type_ctx(ctx, assoc_fn.return_ty, struct_id);

    // Build the call
    let full_name_sym = ctx.interner.get_or_intern(assoc_fn.runtime_fn);

    let mut extra_data = Vec::with_capacity(air_args.len() * 2);
    for arg in &air_args {
        extra_data.push(arg.value.as_u32());
        extra_data.push(arg.mode.as_u32());
    }
    let call_args_start = air.add_extra(&extra_data);
    let call_args_len = air_args.len() as u32;

    let air_ref = air.add_inst(AirInst {
        data: AirInstData::Call {
            name: full_name_sym,
            args_start: call_args_start,
            args_len: call_args_len,
        },
        ty: return_type,
        span,
    });
    Ok(AnalysisResult::new(air_ref, return_type))
}

/// Analyze an intrinsic call using the shared context.
fn analyze_intrinsic_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    inst_ref: InstRef,
    name: Spur,
    args_start: u32,
    args_len: u32,
    span: Span,
    analysis_ctx: &mut AnalysisContext,
) -> CompileResult<AnalysisResult> {
    // Intrinsic arguments are stored as plain InstRefs
    let arg_refs = ctx.rir.get_inst_refs(args_start, args_len);
    let args: Vec<RirCallArg> = arg_refs
        .into_iter()
        .map(|value| RirCallArg {
            value,
            mode: RirArgMode::Normal,
        })
        .collect();
    let known = &ctx.known;

    // Use pre-interned symbol comparison instead of string comparison
    if name == known.dbg {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "dbg".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        let arg_result = analyze_inst_with_context(ctx, air, args[0].value, analysis_ctx)?;
        let arg_type = arg_result.ty;

        // Validate type
        if !arg_type.is_integer()
            && arg_type != Type::Bool
            && !arg_type.is_struct()
            && !arg_type.is_enum()
            && !arg_type.is_array()
            && !arg_type.is_error()
            && !arg_type.is_never()
        {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "dbg".to_string(),
                    expected: "integer, bool, struct, enum, or array".to_string(),
                    found: arg_type.name().to_string(),
                })),
                span,
            ));
        }

        let air_args_start = air.add_extra(&[arg_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: known.dbg,
                args_start: air_args_start,
                args_len: 1,
            },
            ty: Type::Unit,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Unit))
    } else if name == known.cast {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "cast".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        // Get target type from HM inference
        let target_type = get_resolved_type_ctx(analysis_ctx, inst_ref, span, "@cast intrinsic")?;

        let arg_result = analyze_inst_with_context(ctx, air, args[0].value, analysis_ctx)?;
        let source_type = arg_result.ty;

        // Validate types
        if !source_type.is_integer() && !source_type.is_error() && !source_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "cast".to_string(),
                    expected: "integer type".to_string(),
                    found: source_type.name().to_string(),
                })),
                span,
            ));
        }
        if !target_type.is_integer() && !target_type.is_error() && !target_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "cast".to_string(),
                    expected: "integer target type".to_string(),
                    found: target_type.name().to_string(),
                })),
                span,
            ));
        }

        // Skip cast if types are the same
        if source_type == target_type || source_type.is_error() || source_type.is_never() {
            return Ok(arg_result);
        }

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::IntCast {
                value: arg_result.air_ref,
                from_ty: source_type,
            },
            ty: target_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, target_type))
    } else if name == known.panic {
        // @panic takes an optional string message
        if args.len() > 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "panic".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        if args.is_empty() {
            // Panic with no message
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::UnitConst,
                ty: Type::Never,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::Never));
        }

        // Analyze the message argument
        let arg_result = analyze_inst_with_context(ctx, air, args[0].value, analysis_ctx)?;

        let air_args_start = air.add_extra(&[arg_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: known.panic,
                args_start: air_args_start,
                args_len: 1,
            },
            ty: Type::Never,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Never))
    } else if name == known.assert {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "assert".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        let arg_result = analyze_inst_with_context(ctx, air, args[0].value, analysis_ctx)?;

        let air_args_start = air.add_extra(&[arg_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: known.assert,
                args_start: air_args_start,
                args_len: 1,
            },
            ty: Type::Unit,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Unit))
    } else {
        // Unknown intrinsic - resolve name for error message
        let intrinsic_name = ctx.interner.resolve(&name);
        Err(CompileError::new(
            ErrorKind::UnknownIntrinsic(intrinsic_name.to_string()),
            span,
        ))
    }
}

/// Analyze a type intrinsic (@size_of, @align_of) using the shared context.
fn analyze_type_intrinsic_ctx(
    ctx: &SemaContext<'_>,
    air: &mut Air,
    name: Spur,
    type_arg: Spur,
    span: Span,
) -> CompileResult<AnalysisResult> {
    let known = &ctx.known;
    let ty = resolve_type_from_ctx(ctx, type_arg, span)?;

    // Calculate the value based on which intrinsic (using symbol comparison)
    let value: u64 = if name == known.size_of {
        // Calculate size in bytes (slot count * 8)
        let slot_count = ctx.abi_slot_count(ty);
        (slot_count * 8) as u64
    } else if name == known.align_of {
        // Zero-sized types have 1-byte alignment, others have 8-byte
        let slot_count = ctx.abi_slot_count(ty);
        if slot_count == 0 { 1u64 } else { 8u64 }
    } else {
        let intrinsic_name = ctx.interner.resolve(&name);
        return Err(CompileError::new(
            ErrorKind::UnknownIntrinsic(intrinsic_name.to_string()),
            span,
        ));
    };

    let air_ref = air.add_inst(AirInst {
        data: AirInstData::Const(value),
        ty: Type::I32,
        span,
    });
    Ok(AnalysisResult::new(air_ref, Type::I32))
}

impl<'a> Sema<'a> {
    /// Check that a preview feature is enabled.
    ///
    /// This is used to gate experimental features behind the `--preview` flag.
    /// Returns an error with a helpful message if the feature is not enabled.
    ///
    /// # Arguments
    /// - `feature`: The preview feature to check
    /// - `what`: Human-readable description of what requires this feature
    /// - `span`: The source location where the feature is used
    ///
    /// # Returns
    /// - `Ok(())` if the feature is enabled
    /// - `Err(CompileError)` with a helpful message if not enabled
    pub(crate) fn require_preview(
        &self,
        feature: PreviewFeature,
        what: &str,
        span: Span,
    ) -> CompileResult<()> {
        if self.preview_features.contains(&feature) {
            Ok(())
        } else {
            Err(CompileError::new(
                ErrorKind::PreviewFeatureRequired {
                    feature,
                    what: what.to_string(),
                },
                span,
            )
            .with_help(format!(
                "use `--preview {}` to enable this feature ({})",
                feature.name(),
                feature.adr()
            )))
        }
    }

    fn analyze_single_function(
        &mut self,
        infer_ctx: &InferenceContext,
        fn_name: &str,
        return_type: Spur,
        params: &[rue_rir::RirParam],
        body: InstRef,
        span: Span,
    ) -> CompileResult<(AnalyzedFunction, Vec<CompileWarning>, Vec<String>)> {
        let ret_type = self.resolve_type(return_type, span)?;

        // Resolve parameter types and modes
        let param_info: Vec<(Spur, Type, RirParamMode)> = params
            .iter()
            .map(|p| {
                let ty = self.resolve_type(p.ty, span)?;
                Ok((p.name, ty, p.mode))
            })
            .collect::<CompileResult<Vec<_>>>()?;

        let (air, num_locals, num_param_slots, param_modes, warnings, local_strings) =
            self.analyze_function(infer_ctx, ret_type, &param_info, body)?;

        Ok((
            AnalyzedFunction {
                name: fn_name.to_string(),
                air,
                num_locals,
                num_param_slots,
                param_modes,
            },
            warnings,
            local_strings,
        ))
    }

    /// Analyze a method function from an impl block.
    ///
    /// The `infer_ctx` provides pre-computed type information for constraint generation.
    ///
    /// Returns the analyzed function, any warnings, and local strings collected during analysis.
    fn analyze_method_function(
        &mut self,
        infer_ctx: &InferenceContext,
        full_name: &str,
        return_type: Spur,
        params: &[rue_rir::RirParam],
        body: InstRef,
        span: Span,
        struct_type: Type,
        has_self: bool,
    ) -> CompileResult<(AnalyzedFunction, Vec<CompileWarning>, Vec<String>)> {
        let ret_type = self.resolve_type(return_type, span)?;

        // Build parameter list, adding self as first parameter for methods
        let mut param_info: Vec<(Spur, Type, RirParamMode)> = Vec::new();

        if has_self {
            // Add self parameter (Normal mode - passed by value)
            let self_sym = self.interner.get_or_intern("self");
            param_info.push((self_sym, struct_type, RirParamMode::Normal));
        }

        // Add regular parameters with their modes
        for p in params.iter() {
            let ty = self.resolve_type(p.ty, span)?;
            param_info.push((p.name, ty, p.mode));
        }

        let (air, num_locals, num_param_slots, param_modes, warnings, local_strings) =
            self.analyze_function(infer_ctx, ret_type, &param_info, body)?;

        Ok((
            AnalyzedFunction {
                name: full_name.to_string(),
                air,
                num_locals,
                num_param_slots,
                param_modes,
            },
            warnings,
            local_strings,
        ))
    }

    /// Analyze a destructor function.
    ///
    /// The `infer_ctx` provides pre-computed type information for constraint generation.
    ///
    /// Returns the analyzed function, any warnings, and local strings collected during analysis.
    fn analyze_destructor_function(
        &mut self,
        infer_ctx: &InferenceContext,
        full_name: &str,
        body: InstRef,
        _span: Span,
        struct_type: Type,
    ) -> CompileResult<(AnalyzedFunction, Vec<CompileWarning>, Vec<String>)> {
        // Destructors take self parameter and return unit
        let self_sym = self.interner.get_or_intern("self");
        let param_info: Vec<(Spur, Type, RirParamMode)> =
            vec![(self_sym, struct_type, RirParamMode::Normal)];

        let (air, num_locals, num_param_slots, param_modes, warnings, local_strings) =
            self.analyze_function(infer_ctx, Type::Unit, &param_info, body)?;

        Ok((
            AnalyzedFunction {
                name: full_name.to_string(),
                air,
                num_locals,
                num_param_slots,
                param_modes,
            },
            warnings,
            local_strings,
        ))
    }
    /// Analyze a single function, producing AIR.
    ///
    /// The `infer_ctx` provides pre-computed type information for constraint generation,
    /// avoiding the cost of rebuilding maps for each function.
    ///
    /// Returns (air, num_locals, num_param_slots, param_modes, warnings).
    /// Warnings are collected per-function to enable future parallel analysis.
    fn analyze_function(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode)], // (name, type, mode)
        body: InstRef,
    ) -> CompileResult<(Air, u32, u32, Vec<bool>, Vec<CompileWarning>, Vec<String>)> {
        let mut air = Air::new(return_type);
        let mut param_map: HashMap<Spur, ParamInfo> = HashMap::new();
        let mut param_modes: Vec<bool> = Vec::new();

        // Add parameters to the param map, tracking ABI slot offsets.
        // Each parameter starts at the next available ABI slot.
        // For struct parameters, the slot count is the number of fields.
        let mut next_abi_slot: u32 = 0;
        for (pname, ptype, mode) in params.iter() {
            param_map.insert(
                *pname,
                ParamInfo {
                    abi_slot: next_abi_slot,
                    ty: *ptype,
                    mode: *mode,
                },
            );
            // Both inout and borrow are passed by reference (as a pointer = 1 slot)
            let is_by_ref = *mode != RirParamMode::Normal;
            let slot_count = if is_by_ref {
                // By-ref parameters are always 1 slot (pointer)
                1
            } else {
                self.abi_slot_count(*ptype)
            };
            for _ in 0..slot_count {
                param_modes.push(is_by_ref);
            }
            next_abi_slot += slot_count;
        }
        let num_param_slots = next_abi_slot;

        // ======================================================================
        // Phase 1-2: Hindley-Milner Type Inference
        // ======================================================================
        // Run constraint generation and unification to determine types
        // for all expressions BEFORE emitting AIR.
        let resolved_types = self.run_type_inference(infer_ctx, return_type, params, body)?;

        // Create analysis context with resolved types
        let mut ctx = AnalysisContext {
            locals: HashMap::new(),
            params: &param_map,
            next_slot: 0,
            loop_depth: 0,
            used_locals: HashSet::new(),
            return_type,
            scope_stack: Vec::new(),
            resolved_types: &resolved_types,
            moved_vars: HashMap::new(),
            warnings: Vec::new(),
            local_string_table: HashMap::new(),
            local_strings: Vec::new(),
        };

        // ======================================================================
        // Phase 3: AIR Emission
        // ======================================================================
        // Analyze the body expression, emitting AIR with resolved types
        let body_result = self.analyze_inst(&mut air, body, &mut ctx)?;

        // Add implicit return only if body doesn't already diverge (e.g., explicit return)
        if body_result.ty != Type::Never {
            air.add_inst(AirInst {
                data: AirInstData::Ret(Some(body_result.air_ref)),
                ty: return_type,
                span: self.rir.get(body).span,
            });
        }

        Ok((
            air,
            ctx.next_slot,
            num_param_slots,
            param_modes,
            ctx.warnings,
            ctx.local_strings,
        ))
    }

    /// Run Hindley-Milner type inference on a function body.
    ///
    /// This is Phases 1-2 of the HM algorithm:
    /// 1. Generate constraints by walking the RIR
    /// 2. Solve constraints via unification
    ///
    /// The `infer_ctx` parameter provides pre-computed type information (function
    /// signatures, struct/enum types, method signatures) converted to InferType format.
    /// This avoids rebuilding these maps for each function, reducing O(n²) to O(n).
    ///
    /// Returns a map from RIR instruction refs to their resolved concrete types.
    fn run_type_inference(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode)],
        body: InstRef,
    ) -> CompileResult<HashMap<InstRef, Type>> {
        // Create constraint generator using pre-computed inference context
        let mut cgen = ConstraintGenerator::new(
            self.rir,
            self.interner,
            &infer_ctx.func_sigs,
            &infer_ctx.struct_types,
            &infer_ctx.enum_types,
            &infer_ctx.method_sigs,
        );

        // Build parameter map for constraint context.
        // Convert Type to InferType so arrays are represented structurally.
        let param_vars: HashMap<Spur, ParamVarInfo> = params
            .iter()
            .map(|(name, ty, _mode)| {
                (
                    *name,
                    ParamVarInfo {
                        ty: self.type_to_infer_type(*ty),
                    },
                )
            })
            .collect();

        // Create constraint context
        let mut cgen_ctx = ConstraintContext::new(&param_vars, return_type);

        // Phase 1: Generate constraints
        let body_info = cgen.generate(body, &mut cgen_ctx);

        // The function body's type must match the return type.
        // This handles implicit returns like `fn foo() -> i8 { 42 }`.
        cgen.add_constraint(Constraint::equal(
            body_info.ty,
            InferType::Concrete(return_type),
            body_info.span,
        ));

        // Consume the constraint generator to release borrows
        let (constraints, int_literal_vars, expr_types, type_var_count) = cgen.into_parts();

        // Phase 2: Solve constraints via unification
        // Pre-size the substitution for better performance on large functions
        let mut unifier = Unifier::with_capacity(type_var_count);
        let errors = unifier.solve_constraints(&constraints);

        // Convert unification errors to compile errors
        // For now, we collect the first error. In the future, we could
        // report multiple errors for better diagnostics.
        if let Some(err) = errors.first() {
            // Map each UnifyResult variant to the appropriate ErrorKind
            let error_kind = match &err.kind {
                UnifyResult::Ok => unreachable!("UnificationError should never contain Ok"),
                UnifyResult::TypeMismatch { expected, found } => ErrorKind::TypeMismatch {
                    expected: expected.to_string(),
                    found: found.to_string(),
                },
                UnifyResult::IntLiteralNonInteger { found } => ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: found.name().to_string(),
                },
                UnifyResult::OccursCheck { var, ty } => ErrorKind::TypeMismatch {
                    expected: "non-recursive type".to_string(),
                    found: format!("{var} = {ty} (infinite type)"),
                },
                UnifyResult::NotSigned { ty } => {
                    ErrorKind::CannotNegateUnsigned(ty.name().to_string())
                }
                UnifyResult::NotInteger { ty } => ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: ty.name().to_string(),
                },
                UnifyResult::NotUnsigned { ty } => ErrorKind::TypeMismatch {
                    expected: "unsigned integer type".to_string(),
                    found: ty.name().to_string(),
                },
                UnifyResult::ArrayLengthMismatch { expected, found } => {
                    ErrorKind::ArrayLengthMismatch {
                        expected: *expected,
                        found: *found,
                    }
                }
            };

            let mut compile_error = CompileError::new(error_kind, err.span);

            // Add note for unsigned negation errors
            if matches!(err.kind, UnifyResult::NotSigned { .. }) {
                compile_error = compile_error.with_note("unsigned values cannot be negated");
            }

            return Err(compile_error);
        }

        // Default any unconstrained integer literals to i32
        unifier.default_int_literal_vars(&int_literal_vars);

        // Pre-collect all array types from resolved InferTypes before converting them.
        // This ensures all array types are created before the conversion loop, which
        // enables parallelization of function analysis (mutation happens here, not in
        // infer_type_to_type).
        for (_, infer_ty) in &expr_types {
            let resolved = unifier.resolve_infer_type(infer_ty);
            self.pre_create_array_types_from_infer_type(&resolved);
        }

        // Build the resolved types map, converting InferType to Type.
        // Since we pre-created all array types above, infer_type_to_type only
        // performs lookups (no mutation).
        let mut resolved_types = HashMap::new();
        for (inst_ref, infer_ty) in &expr_types {
            let resolved = unifier.resolve_infer_type(infer_ty);
            let concrete_ty = self.infer_type_to_type(&resolved);
            resolved_types.insert(*inst_ref, concrete_ty);
        }

        Ok(resolved_types)
    }
    /// Analyze an RIR instruction for projection (field access).
    ///
    /// This is like `analyze_inst` but does NOT mark non-Copy values as moved.
    /// Used for field access where we're reading from a struct without consuming it.
    /// We still check that the variable hasn't already been moved (fully moved).
    /// Field-level move checking is done at the FieldGet level, not here.
    pub(crate) fn analyze_inst_for_projection(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        // For VarRef, we handle it specially: check for full moves but don't mark as moved
        if let InstData::VarRef { name } = &inst.data {
            // First check if it's a parameter
            if let Some(param_info) = ctx.params.get(name) {
                let ty = param_info.ty;

                // Check if this parameter has been fully moved
                // (Partial moves are checked at the FieldGet level)
                if let Some(move_state) = ctx.moved_vars.get(name) {
                    if let Some(moved_span) = move_state.full_move {
                        let name_str = self.interner.resolve(&*name);
                        return Err(CompileError::new(
                            ErrorKind::UseAfterMove(name_str.to_string()),
                            inst.span,
                        )
                        .with_label("value moved here", moved_span));
                    }
                }

                // NOTE: We do NOT mark as moved here - this is a projection

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::Param {
                        index: param_info.abi_slot,
                    },
                    ty,
                    span: inst.span,
                });
                return Ok(AnalysisResult::new(air_ref, ty));
            }

            // Look up the variable in locals
            let name_str = self.interner.resolve(&*name);
            let local = ctx.locals.get(name).ok_or_compile_error(
                ErrorKind::UndefinedVariable(name_str.to_string()),
                inst.span,
            )?;

            let ty = local.ty;
            let slot = local.slot;

            // Check if this variable has been fully moved
            // (Partial moves are checked at the FieldGet level)
            if let Some(move_state) = ctx.moved_vars.get(name) {
                if let Some(moved_span) = move_state.full_move {
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(name_str.to_string()),
                        inst.span,
                    )
                    .with_label("value moved here", moved_span));
                }
            }

            // NOTE: We do NOT mark as moved here - this is a projection

            // Mark variable as used
            ctx.used_locals.insert(*name);

            // Load the variable
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::Load { slot },
                ty,
                span: inst.span,
            });
            return Ok(AnalysisResult::new(air_ref, ty));
        }

        // For nested field access (e.g., a.b.c), recursively use projection mode
        if let InstData::FieldGet { base, field } = &inst.data {
            let base_result = self.analyze_inst_for_projection(air, *base, ctx)?;
            let base_type = base_result.ty;

            let struct_id = match base_type {
                Type::Struct(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::FieldAccessOnNonStruct {
                            found: base_type.name().to_string(),
                        },
                        inst.span,
                    ));
                }
            };

            let struct_def = self.type_pool.struct_def(struct_id);
            let field_name_str = self.interner.resolve(&*field).to_string();

            let (field_index, struct_field) =
                struct_def.find_field(&field_name_str).ok_or_compile_error(
                    ErrorKind::UnknownField {
                        struct_name: struct_def.name.clone(),
                        field_name: field_name_str.clone(),
                    },
                    inst.span,
                )?;

            let field_type = struct_field.ty;

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::FieldGet {
                    base: base_result.air_ref,
                    struct_id,
                    field_index: field_index as u32,
                },
                ty: field_type,
                span: inst.span,
            });
            return Ok(AnalysisResult::new(air_ref, field_type));
        }

        // For index access in projection mode (e.g., `arr[i].field`), we allow the
        // indexing without checking if the element type is Copy. This enables
        // accessing Copy fields of non-Copy array elements.
        if let InstData::IndexGet { base, index } = &inst.data {
            // Recursively analyze the base in projection mode
            let base_result = self.analyze_inst_for_projection(air, *base, ctx)?;
            let base_type = base_result.ty;

            let array_type_id = match base_type {
                Type::Array(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::IndexOnNonArray {
                            found: base_type.name().to_string(),
                        },
                        inst.span,
                    ));
                }
            };

            // Index must be an unsigned integer
            let index_result = self.analyze_inst(air, *index, ctx)?;
            if !index_result.ty.is_unsigned() && !index_result.ty.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "unsigned integer type".to_string(),
                        found: index_result.ty.name().to_string(),
                    },
                    self.rir.get(*index).span,
                ));
            }

            let array_def = &self.array_type_defs[array_type_id.0 as usize];
            let element_type = array_def.element_type;
            let array_length = array_def.length;

            // Compile-time bounds check for constant indices
            if let Some(const_index) = self.try_get_const_index(*index) {
                if const_index < 0 || const_index as u64 >= array_length {
                    return Err(CompileError::new(
                        ErrorKind::IndexOutOfBounds {
                            index: const_index,
                            length: array_length,
                        },
                        self.rir.get(*index).span,
                    ));
                }
            }

            // NOTE: We do NOT check if element_type is Copy here.
            // In projection mode, we allow accessing elements for further projection
            // (e.g., arr[i].field where field is Copy).

            let air_ref = air.add_inst(AirInst {
                data: AirInstData::IndexGet {
                    base: base_result.air_ref,
                    array_type: base_type,
                    index: index_result.air_ref,
                },
                ty: element_type,
                span: inst.span,
            });
            return Ok(AnalysisResult::new(air_ref, element_type));
        }

        // For other expressions, use the normal analyze_inst
        // (they will trigger move semantics as expected)
        self.analyze_inst(air, inst_ref, ctx)
    }

    /// Look up the resolved type for an instruction from HM inference.
    ///
    /// Returns an `InternalError` if the type was not resolved. This should
    /// never happen in normal operation, but provides a better error message
    /// than a panic if there's a bug in type inference.
    pub(crate) fn get_resolved_type(
        ctx: &AnalysisContext,
        inst_ref: InstRef,
        span: Span,
        context: &str,
    ) -> CompileResult<Type> {
        ctx.resolved_types.get(&inst_ref).copied().ok_or_else(|| {
            CompileError::new(
                ErrorKind::InternalError(format!(
                    "type inference did not resolve type for {} (instruction {:?})",
                    context, inst_ref
                )),
                span,
            )
        })
    }

    /// Analyze an RIR instruction, producing AIR instructions.
    ///
    /// Types are determined by Hindley-Milner inference (stored in `resolved_types`).
    /// Returns both the AIR reference and the synthesized type.
    /// Analyze a single RIR instruction and produce the corresponding AIR instruction.
    ///
    /// This method dispatches to category-specific methods in `analyze_ops.rs` for
    /// maintainability. Each category handles related instruction types together.
    ///
    /// # Categories
    ///
    /// - **Literals**: IntConst, BoolConst, StringConst, UnitConst
    /// - **Binary arithmetic**: Add, Sub, Mul, Div, Mod, BitAnd, BitOr, BitXor, Shl, Shr
    /// - **Comparison**: Eq, Ne, Lt, Gt, Le, Ge
    /// - **Logical**: And, Or
    /// - **Unary**: Neg, Not, BitNot
    /// - **Control flow**: Branch, Loop, InfiniteLoop, Match, Break, Continue, Ret, Block
    /// - **Variables**: Alloc, VarRef, ParamRef, Assign
    /// - **Structs**: StructDecl, StructInit, FieldGet, FieldSet
    /// - **Arrays**: ArrayInit, IndexGet, IndexSet
    /// - **Enums**: EnumDecl, EnumVariant
    /// - **Calls**: Call, MethodCall, AssocFnCall
    /// - **Intrinsics**: Intrinsic, TypeIntrinsic
    /// - **Declarations**: ImplDecl, DropFnDecl, FnDecl
    pub(crate) fn analyze_inst(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let inst = self.rir.get(inst_ref);

        match &inst.data {
            // Literals
            InstData::IntConst(_)
            | InstData::BoolConst(_)
            | InstData::StringConst(_)
            | InstData::UnitConst => self.analyze_literal(air, inst_ref, ctx),

            // Binary arithmetic operations
            InstData::Add { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Add, inst.span, ctx)
            }
            InstData::Sub { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Sub, inst.span, ctx)
            }
            InstData::Mul { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Mul, inst.span, ctx)
            }
            InstData::Div { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Div, inst.span, ctx)
            }
            InstData::Mod { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Mod, inst.span, ctx)
            }

            // Bitwise binary operations
            InstData::BitAnd { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::BitAnd, inst.span, ctx)
            }
            InstData::BitOr { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::BitOr, inst.span, ctx)
            }
            InstData::BitXor { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::BitXor, inst.span, ctx)
            }
            InstData::Shl { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Shl, inst.span, ctx)
            }
            InstData::Shr { lhs, rhs } => {
                self.analyze_binary_arith(air, *lhs, *rhs, AirInstData::Shr, inst.span, ctx)
            }

            // Comparison operations
            InstData::Eq { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, true, AirInstData::Eq, inst.span, ctx)
            }
            InstData::Ne { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, true, AirInstData::Ne, inst.span, ctx)
            }
            InstData::Lt { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Lt, inst.span, ctx)
            }
            InstData::Gt { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Gt, inst.span, ctx)
            }
            InstData::Le { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Le, inst.span, ctx)
            }
            InstData::Ge { lhs, rhs } => {
                self.analyze_comparison(air, *lhs, *rhs, false, AirInstData::Ge, inst.span, ctx)
            }

            // Logical operations
            InstData::And { .. } | InstData::Or { .. } => {
                self.analyze_logical_op(air, inst_ref, ctx)
            }

            // Unary operations
            InstData::Neg { .. } | InstData::Not { .. } | InstData::BitNot { .. } => {
                self.analyze_unary_op(air, inst_ref, ctx)
            }

            // Control flow
            InstData::Branch { .. }
            | InstData::Loop { .. }
            | InstData::InfiniteLoop { .. }
            | InstData::Match { .. }
            | InstData::Break
            | InstData::Continue
            | InstData::Ret(_)
            | InstData::Block { .. } => self.analyze_control_flow(air, inst_ref, ctx),

            // Variable operations
            InstData::Alloc { .. }
            | InstData::VarRef { .. }
            | InstData::ParamRef { .. }
            | InstData::Assign { .. } => self.analyze_variable_ops(air, inst_ref, ctx),

            // Struct operations
            InstData::StructDecl { .. }
            | InstData::StructInit { .. }
            | InstData::FieldGet { .. }
            | InstData::FieldSet { .. } => self.analyze_struct_ops(air, inst_ref, ctx),

            // Array operations
            InstData::ArrayInit { .. } | InstData::IndexGet { .. } | InstData::IndexSet { .. } => {
                self.analyze_array_ops(air, inst_ref, ctx)
            }

            // Enum operations
            InstData::EnumDecl { .. } | InstData::EnumVariant { .. } => {
                self.analyze_enum_ops(air, inst_ref, ctx)
            }

            // Call operations
            InstData::Call { .. } | InstData::MethodCall { .. } | InstData::AssocFnCall { .. } => {
                self.analyze_call_ops(air, inst_ref, ctx)
            }

            // Intrinsic operations
            InstData::Intrinsic { .. } | InstData::TypeIntrinsic { .. } => {
                self.analyze_intrinsic_ops(air, inst_ref, ctx)
            }

            // Declaration no-ops (produce Unit in expression context)
            InstData::ImplDecl { .. } | InstData::DropFnDecl { .. } | InstData::FnDecl { .. } => {
                self.analyze_decl_noop(air, inst_ref, ctx)
            }

            // Comptime block expression
            InstData::Comptime { expr } => {
                // Gate the comptime feature
                self.require_preview(PreviewFeature::Comptime, "comptime blocks", inst.span)?;

                // Try to evaluate the inner expression at compile time
                match self.try_evaluate_const(*expr) {
                    Some(ConstValue::Integer(value)) => {
                        // Get the expected type from resolved types
                        let ty =
                            Self::get_resolved_type(ctx, inst_ref, inst.span, "comptime block")?;

                        // Check if the value fits in the target type
                        if value < 0 {
                            return Err(CompileError::new(
                                ErrorKind::ComptimeEvaluationFailed {
                                    reason: "negative values not yet supported in comptime"
                                        .to_string(),
                                },
                                inst.span,
                            ));
                        }

                        let unsigned_value = value as u64;
                        if !ty.literal_fits(unsigned_value) {
                            return Err(CompileError::new(
                                ErrorKind::LiteralOutOfRange {
                                    value: unsigned_value,
                                    ty: ty.name().to_string(),
                                },
                                inst.span,
                            ));
                        }

                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::Const(unsigned_value),
                            ty,
                            span: inst.span,
                        });
                        Ok(AnalysisResult::new(air_ref, ty))
                    }
                    Some(ConstValue::Bool(value)) => {
                        let ty = Type::Bool;
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::BoolConst(value),
                            ty,
                            span: inst.span,
                        });
                        Ok(AnalysisResult::new(air_ref, ty))
                    }
                    None => Err(CompileError::new(
                        ErrorKind::ComptimeEvaluationFailed {
                            reason:
                                "expression contains values that cannot be known at compile time"
                                    .to_string(),
                        },
                        inst.span,
                    )),
                }
            }
        }
    }

    // ========================================================================
    // Implementation methods for complex operations
    // These are called by the category methods in analyze_ops.rs
    // ========================================================================

    /// Implementation for FieldSet - handles both local and parameter field assignment.
    pub(crate) fn analyze_field_set_impl(
        &mut self,
        air: &mut Air,
        base: InstRef,
        field: Spur,
        value: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // For field assignment, we need to walk up the chain of field accesses
        // to find the root variable. We accumulate the slot offset as we go.

        // Walk up to find the root variable, collecting field symbols
        let mut current_base = base;
        let mut field_symbols: Vec<Spur> = Vec::new();

        // Result is either (Local, slot, type, is_mut, name) or (Param, abi_slot, type, mode, name)
        enum RootKind {
            Local { slot: u32, is_mut: bool },
            Param { abi_slot: u32, mode: RirParamMode },
        }

        let (var_name, root_kind, root_type, root_symbol) = loop {
            let current_inst = self.rir.get(current_base);
            match &current_inst.data {
                InstData::VarRef { name } => {
                    let name_str = self.interner.resolve(&*name);

                    // Check if this variable has been moved
                    if let Some(move_state) = ctx.moved_vars.get(name) {
                        if let Some(moved_span) = move_state.is_any_part_moved() {
                            return Err(CompileError::new(
                                ErrorKind::UseAfterMove(name_str.to_string()),
                                span,
                            )
                            .with_label("value moved here", moved_span));
                        }
                    }

                    // First check if it's a parameter
                    if let Some(param_info) = ctx.params.get(name) {
                        break (
                            name_str.to_string(),
                            RootKind::Param {
                                abi_slot: param_info.abi_slot,
                                mode: param_info.mode,
                            },
                            param_info.ty,
                            *name,
                        );
                    }

                    // Then check locals
                    let local = ctx.locals.get(name).ok_or_compile_error(
                        ErrorKind::UndefinedVariable(name_str.to_string()),
                        span,
                    )?;

                    break (
                        name_str.to_string(),
                        RootKind::Local {
                            slot: local.slot,
                            is_mut: local.is_mut,
                        },
                        local.ty,
                        *name,
                    );
                }
                InstData::ParamRef { name, .. } => {
                    let name_str = self.interner.resolve(&*name);
                    let param_info = ctx.params.get(name).ok_or_compile_error(
                        ErrorKind::UndefinedVariable(name_str.to_string()),
                        span,
                    )?;

                    // Check if this parameter has been moved
                    if let Some(move_state) = ctx.moved_vars.get(name) {
                        if let Some(moved_span) = move_state.is_any_part_moved() {
                            return Err(CompileError::new(
                                ErrorKind::UseAfterMove(name_str.to_string()),
                                span,
                            )
                            .with_label("value moved here", moved_span));
                        }
                    }

                    break (
                        name_str.to_string(),
                        RootKind::Param {
                            abi_slot: param_info.abi_slot,
                            mode: param_info.mode,
                        },
                        param_info.ty,
                        *name,
                    );
                }
                InstData::FieldGet {
                    base: inner_base,
                    field: inner_field,
                } => {
                    field_symbols.push(*inner_field);
                    current_base = *inner_base;
                }
                _ => {
                    return Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span));
                }
            }
        };

        // Check mutability based on root kind
        let root_slot = match root_kind {
            RootKind::Local { slot, is_mut } => {
                if !is_mut {
                    return Err(CompileError::new(
                        ErrorKind::AssignToImmutable(var_name),
                        span,
                    ));
                }
                slot
            }
            RootKind::Param { abi_slot, mode } => {
                match mode {
                    RirParamMode::Normal => {
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(var_name.clone()),
                            span,
                        )
                        .with_help(format!(
                            "consider making parameter `{}` inout: `inout {}: {}`",
                            var_name,
                            var_name,
                            root_type.name()
                        )));
                    }
                    RirParamMode::Inout => {
                        // Inout parameters can be mutated
                    }
                    RirParamMode::Borrow => {
                        return Err(CompileError::new(
                            ErrorKind::MutateBorrowedValue { variable: var_name },
                            span,
                        ));
                    }
                }
                abi_slot
            }
        };

        // Suppress unused variable warning
        let _ = root_symbol;

        // Walk through the field chain to compute the slot offset
        let mut current_type = root_type;
        let mut slot_offset: u32 = 0;

        for field_sym in field_symbols.iter().rev() {
            let struct_id = match current_type {
                Type::Struct(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::FieldAccessOnNonStruct {
                            found: current_type.name().to_string(),
                        },
                        span,
                    ));
                }
            };

            let struct_def = self.type_pool.struct_def(struct_id);
            let field_name_str = self.interner.resolve(&*field_sym).to_string();

            let (field_index, struct_field) =
                struct_def.find_field(&field_name_str).ok_or_compile_error(
                    ErrorKind::UnknownField {
                        struct_name: struct_def.name.clone(),
                        field_name: field_name_str.clone(),
                    },
                    span,
                )?;

            slot_offset += self.field_slot_offset(struct_id, field_index);
            current_type = struct_field.ty;
        }

        // Now handle the final field being assigned
        let struct_id = match current_type {
            Type::Struct(id) => id,
            _ => {
                return Err(CompileError::new(
                    ErrorKind::FieldAccessOnNonStruct {
                        found: current_type.name().to_string(),
                    },
                    span,
                ));
            }
        };

        let struct_def = self.type_pool.struct_def(struct_id);
        let field_name_str = self.interner.resolve(&field).to_string();

        let (field_index, _struct_field) =
            struct_def.find_field(&field_name_str).ok_or_compile_error(
                ErrorKind::UnknownField {
                    struct_name: struct_def.name.clone(),
                    field_name: field_name_str.clone(),
                },
                span,
            )?;

        // Analyze the value
        let value_result = self.analyze_inst(air, value, ctx)?;

        // Emit the appropriate instruction based on whether root is a local or param
        let air_ref = match root_kind {
            RootKind::Local { slot, .. } => {
                let base_slot = slot + slot_offset;
                air.add_inst(AirInst {
                    data: AirInstData::FieldSet {
                        slot: base_slot,
                        struct_id,
                        field_index: field_index as u32,
                        value: value_result.air_ref,
                    },
                    ty: Type::Unit,
                    span,
                })
            }
            RootKind::Param { abi_slot, .. } => air.add_inst(AirInst {
                data: AirInstData::ParamFieldSet {
                    param_slot: abi_slot,
                    inner_offset: slot_offset,
                    struct_id,
                    field_index: field_index as u32,
                    value: value_result.air_ref,
                },
                ty: Type::Unit,
                span,
            }),
        };
        Ok(AnalysisResult::new(air_ref, Type::Unit))
    }

    /// Implementation for IndexSet - handles both local and parameter array index assignment.
    pub(crate) fn analyze_index_set_impl(
        &mut self,
        air: &mut Air,
        base: InstRef,
        index: InstRef,
        value: InstRef,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let base_inst = self.rir.get(base);

        enum IndexSetRootKind {
            Local { slot: u32, is_mut: bool },
            Param { abi_slot: u32, mode: RirParamMode },
        }

        let (var_name, root_kind, base_type) = match &base_inst.data {
            InstData::VarRef { name } => {
                let name_str = self.interner.resolve(&*name);

                // Check if this variable has been moved
                if let Some(move_state) = ctx.moved_vars.get(name) {
                    if let Some(moved_span) = move_state.is_any_part_moved() {
                        return Err(CompileError::new(
                            ErrorKind::UseAfterMove(name_str.to_string()),
                            span,
                        )
                        .with_label("value moved here", moved_span));
                    }
                }

                // First check if it's a parameter
                if let Some(param_info) = ctx.params.get(name) {
                    (
                        name_str.to_string(),
                        IndexSetRootKind::Param {
                            abi_slot: param_info.abi_slot,
                            mode: param_info.mode,
                        },
                        param_info.ty,
                    )
                } else {
                    // Then check locals
                    let local = ctx.locals.get(name).ok_or_compile_error(
                        ErrorKind::UndefinedVariable(name_str.to_string()),
                        span,
                    )?;

                    (
                        name_str.to_string(),
                        IndexSetRootKind::Local {
                            slot: local.slot,
                            is_mut: local.is_mut,
                        },
                        local.ty,
                    )
                }
            }
            InstData::ParamRef { name, .. } => {
                let name_str = self.interner.resolve(&*name);
                let param_info = ctx.params.get(name).ok_or_compile_error(
                    ErrorKind::UndefinedVariable(name_str.to_string()),
                    span,
                )?;

                // Check if this parameter has been moved
                if let Some(move_state) = ctx.moved_vars.get(name) {
                    if let Some(moved_span) = move_state.is_any_part_moved() {
                        return Err(CompileError::new(
                            ErrorKind::UseAfterMove(name_str.to_string()),
                            span,
                        )
                        .with_label("value moved here", moved_span));
                    }
                }

                (
                    name_str.to_string(),
                    IndexSetRootKind::Param {
                        abi_slot: param_info.abi_slot,
                        mode: param_info.mode,
                    },
                    param_info.ty,
                )
            }
            _ => {
                return Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span));
            }
        };

        // Check mutability based on root kind
        let (is_inout_param, slot) = match root_kind {
            IndexSetRootKind::Local { slot, is_mut } => {
                if !is_mut {
                    return Err(CompileError::new(
                        ErrorKind::AssignToImmutable(var_name),
                        span,
                    ));
                }
                (false, slot)
            }
            IndexSetRootKind::Param { abi_slot, mode } => {
                let is_inout = match mode {
                    RirParamMode::Normal => false,
                    RirParamMode::Inout => true,
                    RirParamMode::Borrow => {
                        return Err(CompileError::new(
                            ErrorKind::MutateBorrowedValue { variable: var_name },
                            span,
                        ));
                    }
                };
                (is_inout, abi_slot)
            }
        };

        let array_type_id = match base_type {
            Type::Array(id) => id,
            _ => {
                return Err(CompileError::new(
                    ErrorKind::IndexOnNonArray {
                        found: base_type.name().to_string(),
                    },
                    span,
                ));
            }
        };

        // Index must be an unsigned integer
        let index_result = self.analyze_inst(air, index, ctx)?;
        if !index_result.ty.is_unsigned() && !index_result.ty.is_error() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "unsigned integer type".to_string(),
                    found: index_result.ty.name().to_string(),
                },
                self.rir.get(index).span,
            ));
        }

        let array_def = &self.array_type_defs[array_type_id.0 as usize];
        let element_type = array_def.element_type;
        let array_length = array_def.length;

        // Compile-time bounds check for constant indices
        if let Some(const_index) = self.try_get_const_index(index) {
            if const_index < 0 || const_index as u64 >= array_length {
                return Err(CompileError::new(
                    ErrorKind::IndexOutOfBounds {
                        index: const_index,
                        length: array_length,
                    },
                    self.rir.get(index).span,
                ));
            }
        }

        // Analyze the value
        let value_result = self.analyze_inst(air, value, ctx)?;

        // Emit appropriate instruction
        let air_ref = if is_inout_param {
            air.add_inst(AirInst {
                data: AirInstData::ParamIndexSet {
                    param_slot: slot,
                    array_type: base_type,
                    index: index_result.air_ref,
                    value: value_result.air_ref,
                },
                ty: Type::Unit,
                span,
            })
        } else {
            air.add_inst(AirInst {
                data: AirInstData::IndexSet {
                    slot,
                    array_type: base_type,
                    index: index_result.air_ref,
                    value: value_result.air_ref,
                },
                ty: Type::Unit,
                span,
            })
        };
        Ok(AnalysisResult::new(air_ref, Type::Unit))
    }

    /// Implementation for MethodCall.
    pub(crate) fn analyze_method_call_impl(
        &mut self,
        air: &mut Air,
        receiver: InstRef,
        method: Spur,
        args_start: u32,
        args_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let args = self.rir.get_call_args(args_start, args_len);
        let receiver_var = self.extract_root_variable(receiver);
        let method_name_str = self.interner.resolve(&method).to_string();

        // Check if this is a builtin mutation method
        let is_builtin_mutation_method = self.is_builtin_mutation_method(&method_name_str);

        // Get storage location for mutation methods before analyzing receiver
        let receiver_storage = if is_builtin_mutation_method {
            self.get_string_receiver_storage(receiver, ctx, span)?
        } else {
            None
        };

        // Analyze the receiver expression
        let receiver_result = self.analyze_inst(air, receiver, ctx)?;
        let receiver_type = receiver_result.ty;

        // Check that receiver is a struct type
        let struct_id = match receiver_type {
            Type::Struct(id) => id,
            _ => {
                return Err(CompileError::new(
                    ErrorKind::MethodCallOnNonStruct {
                        found: receiver_type.name().to_string(),
                        method_name: method_name_str,
                    },
                    span,
                ));
            }
        };

        // Check if this is a builtin type and handle its methods
        if let Some(builtin_def) = self.get_builtin_type_def(struct_id) {
            let method_ctx = BuiltinMethodContext {
                struct_id,
                builtin_def,
                method_name: &method_name_str,
                span,
            };
            let receiver_info = ReceiverInfo {
                result: receiver_result,
                var: receiver_var,
                storage: receiver_storage,
            };
            return self.analyze_builtin_method(air, ctx, &method_ctx, receiver_info, &args);
        }

        // Look up the struct name by its ID
        let struct_def = self.type_pool.struct_def(struct_id);
        let struct_name_str = struct_def.name.clone();

        // Find the struct name symbol for method lookup
        let struct_name_sym = self.interner.get_or_intern(&struct_name_str);

        // Look up the method
        let method_key = (struct_name_sym, method);
        let method_info = self.methods.get(&method_key).ok_or_compile_error(
            ErrorKind::UndefinedMethod {
                type_name: struct_name_str.clone(),
                method_name: method_name_str.clone(),
            },
            span,
        )?;

        // Check that this is a method (has self), not an associated function
        if !method_info.has_self {
            return Err(CompileError::new(
                ErrorKind::AssocFnCalledAsMethod {
                    type_name: struct_name_str,
                    function_name: method_name_str,
                },
                span,
            ));
        }

        // Check argument count (method_info.param_types excludes self)
        if args.len() != method_info.param_types.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: method_info.param_types.len(),
                    found: args.len(),
                },
                span,
            ));
        }

        // Check for exclusive access violation
        self.check_exclusive_access(&args, span)?;

        // Clone data needed before mutable borrow
        let return_type = method_info.return_type;

        // Analyze arguments - receiver first, then remaining args
        let mut air_args = vec![AirCallArg {
            value: receiver_result.air_ref,
            mode: AirArgMode::Normal,
        }];
        air_args.extend(self.analyze_call_args(air, &args, ctx)?);

        // Generate a method call name: Type.method
        let call_name = format!("{}.{}", struct_name_str, method_name_str);
        let call_name_sym = self.interner.get_or_intern(&call_name);

        // Encode call args into extra array
        let args_len = air_args.len() as u32;
        let mut extra_data = Vec::with_capacity(air_args.len() * 2);
        for arg in &air_args {
            extra_data.push(arg.value.as_u32());
            extra_data.push(arg.mode.as_u32());
        }
        let args_start = air.add_extra(&extra_data);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: call_name_sym,
                args_start,
                args_len,
            },
            ty: return_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, return_type))
    }

    /// Implementation for AssocFnCall.
    pub(crate) fn analyze_assoc_fn_call_impl(
        &mut self,
        air: &mut Air,
        type_name: Spur,
        function: Spur,
        args_start: u32,
        args_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let args = self.rir.get_call_args(args_start, args_len);
        let type_name_str = self.interner.resolve(&type_name).to_string();
        let function_name_str = self.interner.resolve(&function).to_string();

        // Check that the type exists and is a struct
        let struct_id = *self
            .structs
            .get(&type_name)
            .ok_or_compile_error(ErrorKind::UnknownType(type_name_str.clone()), span)?;

        // Handle builtin type associated functions
        if let Some(builtin_def) = self.get_builtin_type_def(struct_id) {
            return self.analyze_builtin_assoc_fn(
                air,
                ctx,
                struct_id,
                builtin_def,
                &function_name_str,
                &args,
                span,
            );
        }

        // Look up the function
        let method_key = (type_name, function);
        let method_info = self.methods.get(&method_key).ok_or_compile_error(
            ErrorKind::UndefinedAssocFn {
                type_name: type_name_str.clone(),
                function_name: function_name_str.clone(),
            },
            span,
        )?;

        // Check that this is an associated function (no self), not a method
        if method_info.has_self {
            return Err(CompileError::new(
                ErrorKind::MethodCalledAsAssocFn {
                    type_name: type_name_str,
                    method_name: function_name_str,
                },
                span,
            ));
        }

        // Check argument count
        if args.len() != method_info.param_types.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: method_info.param_types.len(),
                    found: args.len(),
                },
                span,
            ));
        }

        // Check for exclusive access violation
        self.check_exclusive_access(&args, span)?;

        // Clone data needed before mutable borrow
        let return_type = method_info.return_type;

        // Analyze arguments
        let air_args = self.analyze_call_args(air, &args, ctx)?;

        // Generate a function call name: Type::function
        let call_name = format!("{}::{}", type_name_str, function_name_str);
        let call_name_sym = self.interner.get_or_intern(&call_name);

        // Encode call args into extra array
        let args_len = air_args.len() as u32;
        let mut extra_data = Vec::with_capacity(air_args.len() * 2);
        for arg in &air_args {
            extra_data.push(arg.value.as_u32());
            extra_data.push(arg.mode.as_u32());
        }
        let args_start = air.add_extra(&extra_data);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: call_name_sym,
                args_start,
                args_len,
            },
            ty: return_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, return_type))
    }

    /// Implementation for Intrinsic calls.
    pub(crate) fn analyze_intrinsic_impl(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        name: Spur,
        args_start: u32,
        args_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Intrinsic arguments are stored as plain InstRefs
        let arg_refs = self.rir.get_inst_refs(args_start, args_len);
        let args: Vec<RirCallArg> = arg_refs
            .into_iter()
            .map(|value| RirCallArg {
                value,
                mode: RirArgMode::Normal,
            })
            .collect();
        let known = &self.known;

        // Use pre-interned symbol comparison instead of string comparison
        if name == known.dbg {
            self.analyze_dbg_intrinsic(air, inst_ref, &args, span, ctx)
        } else if name == known.int_cast {
            self.analyze_intcast_intrinsic(air, inst_ref, &args, span, ctx)
        } else if name == known.test_preview_gate {
            self.analyze_test_preview_gate_intrinsic(air, &args, span)
        } else if name == known.read_line {
            self.analyze_read_line_intrinsic(air, name, &args, span)
        } else if let Some(intrinsic_name_str) = known.get_parse_intrinsic_name(name) {
            self.analyze_parse_intrinsic(air, name, intrinsic_name_str, &args, span, ctx)
        } else if name == known.cast {
            self.analyze_cast_intrinsic(air, inst_ref, &args, span, ctx)
        } else if name == known.panic {
            self.analyze_panic_intrinsic(air, &args, span, ctx)
        } else if name == known.assert {
            self.analyze_assert_intrinsic(air, &args, span, ctx)
        } else {
            // Unknown intrinsic - resolve name for error message
            let intrinsic_name_str = self.interner.resolve(&name);
            Err(CompileError::new(
                ErrorKind::UnknownIntrinsic(intrinsic_name_str.to_string()),
                span,
            ))
        }
    }

    // Helper methods for intrinsic analysis (delegated from analyze_intrinsic_impl)

    fn analyze_dbg_intrinsic(
        &mut self,
        air: &mut Air,
        _inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "dbg".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        let arg_result = self.analyze_inst(air, args[0].value, ctx)?;
        let arg_type = arg_result.ty;

        // Validate type
        if !arg_type.is_integer()
            && arg_type != Type::Bool
            && !arg_type.is_struct()
            && !arg_type.is_enum()
            && !arg_type.is_array()
            && !arg_type.is_error()
            && !arg_type.is_never()
        {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "dbg".to_string(),
                    expected: "integer, bool, struct, enum, or array".to_string(),
                    found: arg_type.name().to_string(),
                })),
                span,
            ));
        }

        let args_start = air.add_extra(&[arg_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: self.known.dbg,
                args_start,
                args_len: 1,
            },
            ty: Type::Unit,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Unit))
    }

    fn analyze_cast_intrinsic(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "cast".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        // Get target type from HM inference
        let target_type = Self::get_resolved_type(ctx, inst_ref, span, "@cast intrinsic")?;

        let arg_result = self.analyze_inst(air, args[0].value, ctx)?;
        let source_type = arg_result.ty;

        // Validate types
        if !source_type.is_integer() && !source_type.is_error() && !source_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "cast".to_string(),
                    expected: "integer type".to_string(),
                    found: source_type.name().to_string(),
                })),
                span,
            ));
        }
        if !target_type.is_integer() && !target_type.is_error() && !target_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "cast".to_string(),
                    expected: "integer target type".to_string(),
                    found: target_type.name().to_string(),
                })),
                span,
            ));
        }

        // Skip cast if types are the same
        if source_type == target_type || source_type.is_error() || source_type.is_never() {
            return Ok(arg_result);
        }

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::IntCast {
                value: arg_result.air_ref,
                from_ty: source_type,
            },
            ty: target_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, target_type))
    }

    fn analyze_panic_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // @panic takes an optional string message
        if args.len() > 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "panic".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        if args.is_empty() {
            // Panic with no message
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::UnitConst,
                ty: Type::Never,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::Never));
        }

        // Analyze the message argument
        let arg_result = self.analyze_inst(air, args[0].value, ctx)?;

        let args_start = air.add_extra(&[arg_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: self.known.panic,
                args_start,
                args_len: 1,
            },
            ty: Type::Never,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Never))
    }

    fn analyze_assert_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // @assert takes a bool condition and optional message
        if args.is_empty() || args.len() > 2 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "assert".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        let cond_result = self.analyze_inst(air, args[0].value, ctx)?;

        // Build args for AIR
        let mut extra_data = vec![cond_result.air_ref.as_u32()];
        if args.len() > 1 {
            let msg_result = self.analyze_inst(air, args[1].value, ctx)?;
            extra_data.push(msg_result.air_ref.as_u32());
        }

        let args_len = extra_data.len() as u32;
        let args_start = air.add_extra(&extra_data);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name: self.known.assert,
                args_start,
                args_len,
            },
            ty: Type::Unit,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Unit))
    }

    /// Analyze @intCast intrinsic.
    fn analyze_intcast_intrinsic(
        &mut self,
        air: &mut Air,
        inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        let intrinsic_name = "intCast";

        // @intCast expects exactly one argument
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: intrinsic_name.to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        // Analyze the argument
        let arg_result = self.analyze_inst(air, args[0].value, ctx)?;
        let from_ty = arg_result.ty;

        // Argument must be an integer type
        if !from_ty.is_integer() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: intrinsic_name.to_string(),
                    expected: "integer".to_string(),
                    found: from_ty.name().to_string(),
                })),
                span,
            ));
        }

        // Get the target type from HM inference
        let target_ty = match ctx.resolved_types.get(&inst_ref).copied() {
            Some(ty) if ty.is_integer() => ty,
            Some(Type::Error) => {
                // Error already reported during type inference
                return Err(CompileError::new(ErrorKind::TypeAnnotationRequired, span));
            }
            Some(ty) => {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: intrinsic_name.to_string(),
                        expected: "integer".to_string(),
                        found: ty.name().to_string(),
                    })),
                    span,
                ));
            }
            None => {
                // Type inference couldn't determine the target type
                return Err(CompileError::new(ErrorKind::TypeAnnotationRequired, span));
            }
        };

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::IntCast {
                value: arg_result.air_ref,
                from_ty,
            },
            ty: target_ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, target_ty))
    }

    /// Analyze @test_preview_gate intrinsic.
    fn analyze_test_preview_gate_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // @test_preview_gate() - no-op intrinsic gated by test_infra preview feature.
        self.require_preview(
            PreviewFeature::TestInfra,
            "@test_preview_gate() intrinsic",
            span,
        )?;

        // Takes no arguments
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "test_preview_gate".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        // No-op: just return a unit constant
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::UnitConst,
            ty: Type::Unit,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Unit))
    }

    /// Analyze @read_line intrinsic.
    fn analyze_read_line_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // @read_line() - reads a line from stdin and returns it as a String.
        // Takes no arguments, returns String.
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "read_line".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        // Get the String type
        let string_type = self.builtin_string_type();

        // Create the intrinsic instruction that returns String
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start: 0, // No args
                args_len: 0,
            },
            ty: string_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, string_type))
    }

    /// Analyze @parse_i32, @parse_i64, @parse_u32, @parse_u64 intrinsics.
    fn analyze_parse_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        intrinsic_name_str: &str,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Expects exactly one argument
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: intrinsic_name_str.to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        // Analyze the argument - String borrows are handled by
        // analyze_inst_for_projection to avoid consuming the String
        let arg_result = self.analyze_inst_for_projection(air, args[0].value, ctx)?;
        let arg_type = arg_result.ty;

        // Argument must be a String
        if !self.is_builtin_string(arg_type) {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: format!("@{}", intrinsic_name_str),
                    expected: "String".to_string(),
                    found: arg_type.name().to_string(),
                })),
                span,
            ));
        }

        // Determine the return type based on the intrinsic name
        let return_type = match intrinsic_name_str {
            "parse_i32" => Type::I32,
            "parse_i64" => Type::I64,
            "parse_u32" => Type::U32,
            "parse_u64" => Type::U64,
            _ => unreachable!(),
        };

        // Encode args into extra array
        let args_start = air.add_extra(&[arg_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 1,
            },
            ty: return_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, return_type))
    }

    // Note: The old analyze_inst body from here onwards is now handled by the
    // dispatcher above and the category methods in analyze_ops.rs

    // ========================================================================
    // Helper methods for analysis
    // ========================================================================

    /// Convert RIR argument mode to AIR argument mode.
    fn convert_arg_mode(mode: RirArgMode) -> AirArgMode {
        match mode {
            RirArgMode::Normal => AirArgMode::Normal,
            RirArgMode::Inout => AirArgMode::Inout,
            RirArgMode::Borrow => AirArgMode::Borrow,
        }
    }
    /// Analyze a binary arithmetic operator (+, -, *, /, %).
    ///
    /// Follows Rust's type inference rules:
    /// Types are determined by HM inference. Both operands must have the same type.
    fn analyze_binary_arith<F>(
        &mut self,
        air: &mut Air,
        lhs: InstRef,
        rhs: InstRef,
        make_data: F,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult>
    where
        F: FnOnce(AirRef, AirRef) -> AirInstData,
    {
        let lhs_result = self.analyze_inst(air, lhs, ctx)?;
        let rhs_result = self.analyze_inst(air, rhs, ctx)?;

        // Verify the type is integer (HM should have enforced this, but check anyway)
        if !lhs_result.ty.is_integer() && !lhs_result.ty.is_error() && !lhs_result.ty.is_never() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: lhs_result.ty.name().to_string(),
                },
                span,
            ));
        }

        let air_ref = air.add_inst(AirInst {
            data: make_data(lhs_result.air_ref, rhs_result.air_ref),
            ty: lhs_result.ty,
            span,
        });
        Ok(AnalysisResult::new(air_ref, lhs_result.ty))
    }

    /// Analyze a comparison operator.
    ///
    /// Types are determined by HM inference. Both operands must have the same type.
    ///
    /// For equality operators (`==`, `!=`), both integers and booleans are allowed.
    /// For ordering operators (`<`, `>`, `<=`, `>=`), only integers are allowed.
    fn analyze_comparison<F>(
        &mut self,
        air: &mut Air,
        lhs: InstRef,
        rhs: InstRef,
        allow_bool: bool,
        make_data: F,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult>
    where
        F: FnOnce(AirRef, AirRef) -> AirInstData,
    {
        // Check for chained comparisons (e.g., `a < b < c`)
        // Since the parser is left-associative, `a < b < c` parses as `(a < b) < c`,
        // so we only need to check if the LHS is a comparison.
        if self.is_comparison(lhs) {
            return Err(CompileError::new(ErrorKind::ChainedComparison, span)
                .with_help("use `&&` to combine comparisons: `a < b && b < c`"));
        }

        // Comparisons read values without consuming them (like projections).
        // This matches Rust's PartialEq trait which takes references.
        let lhs_result = self.analyze_inst_for_projection(air, lhs, ctx)?;
        let rhs_result = self.analyze_inst_for_projection(air, rhs, ctx)?;
        let lhs_type = lhs_result.ty;

        // Propagate Never/Error without additional type errors
        if lhs_type.is_never() || lhs_type.is_error() {
            let air_ref = air.add_inst(AirInst {
                data: make_data(lhs_result.air_ref, rhs_result.air_ref),
                ty: Type::Bool,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::Bool));
        }

        // Validate the type is appropriate for this comparison
        if allow_bool {
            // Equality operators (==, !=) work on integers, booleans, strings, unit, and structs
            // Note: String is now a struct, so is_struct() covers it
            if !lhs_type.is_integer()
                && lhs_type != Type::Bool
                && lhs_type != Type::Unit
                && !lhs_type.is_struct()
                && !self.is_builtin_string(lhs_type)
            {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "integer, bool, string, unit, or struct".to_string(),
                        found: lhs_type.name().to_string(),
                    },
                    self.rir.get(lhs).span,
                ));
            }
        } else if !lhs_type.is_integer() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "integer".to_string(),
                    found: lhs_type.name().to_string(),
                },
                self.rir.get(lhs).span,
            ));
        }

        let air_ref = air.add_inst(AirInst {
            data: make_data(lhs_result.air_ref, rhs_result.air_ref),
            ty: Type::Bool,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::Bool))
    }

    /// Try to evaluate an RIR expression as a compile-time constant.
    ///
    /// Returns `Some(value)` if the expression can be fully evaluated at compile time,
    /// or `None` if evaluation requires runtime information (e.g., variable values,
    /// function calls) or would cause overflow/panic.
    ///
    /// This is the foundation for compile-time bounds checking and can be extended
    /// for future `comptime` features.
    pub(crate) fn try_evaluate_const(&self, inst_ref: InstRef) -> Option<ConstValue> {
        let inst = self.rir.get(inst_ref);
        match &inst.data {
            // Integer literals
            InstData::IntConst(value) => i64::try_from(*value).ok().map(ConstValue::Integer),

            // Boolean literals
            InstData::BoolConst(value) => Some(ConstValue::Bool(*value)),

            // Unary negation: -expr
            InstData::Neg { operand } => {
                match self.try_evaluate_const(*operand)? {
                    ConstValue::Integer(n) => n.checked_neg().map(ConstValue::Integer),
                    ConstValue::Bool(_) => None, // Can't negate a boolean
                }
            }

            // Logical NOT: !expr
            InstData::Not { operand } => {
                match self.try_evaluate_const(*operand)? {
                    ConstValue::Bool(b) => Some(ConstValue::Bool(!b)),
                    ConstValue::Integer(_) => None, // Can't logical-NOT an integer
                }
            }

            // Binary arithmetic operations
            InstData::Add { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                l.checked_add(r).map(ConstValue::Integer)
            }
            InstData::Sub { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                l.checked_sub(r).map(ConstValue::Integer)
            }
            InstData::Mul { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                l.checked_mul(r).map(ConstValue::Integer)
            }
            InstData::Div { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                if r == 0 {
                    None // Division by zero - defer to runtime
                } else {
                    l.checked_div(r).map(ConstValue::Integer)
                }
            }
            InstData::Mod { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                if r == 0 {
                    None // Modulo by zero - defer to runtime
                } else {
                    l.checked_rem(r).map(ConstValue::Integer)
                }
            }

            // Comparison operations
            InstData::Eq { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?;
                let r = self.try_evaluate_const(*rhs)?;
                match (l, r) {
                    (ConstValue::Integer(a), ConstValue::Integer(b)) => {
                        Some(ConstValue::Bool(a == b))
                    }
                    (ConstValue::Bool(a), ConstValue::Bool(b)) => Some(ConstValue::Bool(a == b)),
                    _ => None, // Mixed types
                }
            }
            InstData::Ne { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?;
                let r = self.try_evaluate_const(*rhs)?;
                match (l, r) {
                    (ConstValue::Integer(a), ConstValue::Integer(b)) => {
                        Some(ConstValue::Bool(a != b))
                    }
                    (ConstValue::Bool(a), ConstValue::Bool(b)) => Some(ConstValue::Bool(a != b)),
                    _ => None,
                }
            }
            InstData::Lt { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Bool(l < r))
            }
            InstData::Gt { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Bool(l > r))
            }
            InstData::Le { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Bool(l <= r))
            }
            InstData::Ge { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Bool(l >= r))
            }

            // Logical operations
            InstData::And { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_bool()?;
                let r = self.try_evaluate_const(*rhs)?.as_bool()?;
                Some(ConstValue::Bool(l && r))
            }
            InstData::Or { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_bool()?;
                let r = self.try_evaluate_const(*rhs)?.as_bool()?;
                Some(ConstValue::Bool(l || r))
            }

            // Bitwise operations
            InstData::BitAnd { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Integer(l & r))
            }
            InstData::BitOr { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Integer(l | r))
            }
            InstData::BitXor { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                Some(ConstValue::Integer(l ^ r))
            }
            InstData::Shl { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                // Only constant-fold small shift amounts to avoid type-width issues.
                // For shifts >= 8, defer to runtime where hardware handles masking correctly.
                // This is conservative but safe - we don't know the operand type here.
                if r < 0 || r >= 8 {
                    return None;
                }
                Some(ConstValue::Integer(l << r))
            }
            InstData::Shr { lhs, rhs } => {
                let l = self.try_evaluate_const(*lhs)?.as_integer()?;
                let r = self.try_evaluate_const(*rhs)?.as_integer()?;
                // Only constant-fold small shift amounts to avoid type-width issues.
                // For shifts >= 8, defer to runtime where hardware handles masking correctly.
                if r < 0 || r >= 8 {
                    return None;
                }
                Some(ConstValue::Integer(l >> r))
            }
            InstData::BitNot { operand } => {
                let n = self.try_evaluate_const(*operand)?.as_integer()?;
                Some(ConstValue::Integer(!n))
            }

            // Comptime block: comptime { expr } is compile-time evaluable if its inner expr is
            InstData::Comptime { expr } => self.try_evaluate_const(*expr),

            // Everything else requires runtime evaluation
            _ => None,
        }
    }

    /// Try to extract a constant integer value from an RIR index expression.
    ///
    /// This is used for compile-time bounds checking. Returns `Some(value)` if
    /// the index can be evaluated to an integer constant at compile time.
    pub(crate) fn try_get_const_index(&self, inst_ref: InstRef) -> Option<i64> {
        self.try_evaluate_const(inst_ref)?.as_integer()
    }

    /// Check if an RIR instruction is an integer literal.
    ///
    /// This is used for bidirectional type inference to detect when the LHS
    /// of a binary operator is a literal that can adopt its type from the RHS.
    fn is_integer_literal(&self, inst_ref: InstRef) -> bool {
        matches!(self.rir.get(inst_ref).data, InstData::IntConst(_))
    }

    /// Check if an RIR instruction is a comparison operation.
    ///
    /// This is used to detect chained comparisons (e.g., `a < b < c`) which are
    /// not allowed in Rue.
    fn is_comparison(&self, inst_ref: InstRef) -> bool {
        matches!(
            self.rir.get(inst_ref).data,
            InstData::Lt { .. }
                | InstData::Gt { .. }
                | InstData::Le { .. }
                | InstData::Ge { .. }
                | InstData::Eq { .. }
                | InstData::Ne { .. }
        )
    }

    /// Analyze a builtin type associated function call.
    ///
    /// Dispatches to the appropriate runtime function based on the builtin registry.
    fn analyze_builtin_assoc_fn(
        &mut self,
        air: &mut Air,
        ctx: &mut AnalysisContext,
        struct_id: StructId,
        builtin_def: &'static BuiltinTypeDef,
        function_name: &str,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        use rue_builtins::{BuiltinParamType, BuiltinReturnType};

        // Look up the associated function in the registry
        let assoc_fn = builtin_def
            .find_associated_fn(function_name)
            .ok_or_else(|| {
                CompileError::new(
                    ErrorKind::UndefinedAssocFn {
                        type_name: builtin_def.name.to_string(),
                        function_name: function_name.to_string(),
                    },
                    span,
                )
            })?;

        // Check argument count
        if args.len() != assoc_fn.params.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: assoc_fn.params.len(),
                    found: args.len(),
                },
                span,
            ));
        }

        // Analyze arguments and check types
        let mut air_args: Vec<(AirRef, AirArgMode)> = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            let arg_result = self.analyze_inst(air, arg.value, ctx)?;

            // Get expected type from param
            let expected_ty = match assoc_fn.params[i].ty {
                BuiltinParamType::U64 => Type::U64,
                BuiltinParamType::U8 => Type::U8,
                BuiltinParamType::Bool => Type::Bool,
                BuiltinParamType::SelfType => Type::Struct(struct_id),
            };

            // Type check
            if arg_result.ty != expected_ty && !arg_result.ty.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: expected_ty.name().to_string(),
                        found: arg_result.ty.name().to_string(),
                    },
                    span,
                ));
            }

            air_args.push((arg_result.air_ref, AirArgMode::Normal));
        }

        // Determine return type
        // Use builtin_air_type for SelfType to get correct AIR output type
        let return_ty = match assoc_fn.return_ty {
            BuiltinReturnType::Unit => Type::Unit,
            BuiltinReturnType::U64 => Type::U64,
            BuiltinReturnType::U8 => Type::U8,
            BuiltinReturnType::Bool => Type::Bool,
            BuiltinReturnType::SelfType => self.builtin_air_type(struct_id),
        };

        // Generate runtime function call
        let call_name = self.interner.get_or_intern(assoc_fn.runtime_fn);

        // Encode args into extra array
        let mut extra_data: Vec<u32> = Vec::with_capacity(air_args.len() * 2);
        for (air_ref, mode) in &air_args {
            extra_data.push(air_ref.as_u32());
            extra_data.push(mode.as_u32());
        }
        let args_start = air.add_extra(&extra_data);

        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: call_name,
                args_start,
                args_len: air_args.len() as u32,
            },
            ty: return_ty,
            span,
        });

        Ok(AnalysisResult::new(air_ref, return_ty))
    }

    /// Analyze a builtin type method call.
    ///
    /// Dispatches to the appropriate runtime function based on the builtin registry.
    /// Handles borrow semantics (for query methods) and mutation semantics (for
    /// methods that modify the receiver).
    fn analyze_builtin_method(
        &mut self,
        air: &mut Air,
        ctx: &mut AnalysisContext,
        method_ctx: &BuiltinMethodContext<'_>,
        receiver: ReceiverInfo,
        args: &[RirCallArg],
    ) -> CompileResult<AnalysisResult> {
        use rue_builtins::{BuiltinParamType, BuiltinReturnType, ReceiverMode};

        // Look up the method in the registry
        let method = method_ctx
            .builtin_def
            .find_method(method_ctx.method_name)
            .ok_or_else(|| {
                CompileError::new(
                    ErrorKind::UndefinedMethod {
                        type_name: method_ctx.builtin_def.name.to_string(),
                        method_name: method_ctx.method_name.to_string(),
                    },
                    method_ctx.span,
                )
            })?;

        // Handle receiver mode (borrow vs mutation vs consume)
        match method.receiver_mode {
            ReceiverMode::ByRef => {
                // Borrow semantics - "unmove" the variable since it's not consumed
                if let Some(var_symbol) = receiver.var {
                    ctx.moved_vars.remove(&var_symbol);
                }
            }
            ReceiverMode::ByMutRef => {
                // Mutation semantics - variable remains valid after
                if let Some(var_symbol) = receiver.var {
                    ctx.moved_vars.remove(&var_symbol);
                }
            }
            ReceiverMode::ByValue => {
                // Consume semantics - variable is moved (already handled by analyze_inst)
            }
        }

        // Check argument count
        if args.len() != method.params.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: method.params.len(),
                    found: args.len(),
                },
                method_ctx.span,
            ));
        }

        // Analyze arguments and check types
        let mut air_args: Vec<(AirRef, AirArgMode)> = Vec::with_capacity(args.len() + 1);

        // Add receiver as first argument
        air_args.push((receiver.result.air_ref, AirArgMode::Normal));

        // Analyze and add other arguments
        for (i, arg) in args.iter().enumerate() {
            let arg_result = self.analyze_inst(air, arg.value, ctx)?;

            // Get expected type from param
            let expected_ty = match method.params[i].ty {
                BuiltinParamType::U64 => Type::U64,
                BuiltinParamType::U8 => Type::U8,
                BuiltinParamType::Bool => Type::Bool,
                BuiltinParamType::SelfType => Type::Struct(method_ctx.struct_id),
            };

            // Type check
            if arg_result.ty != expected_ty
                && !arg_result.ty.is_error()
                && !(self.is_builtin_string(arg_result.ty)
                    && matches!(method.params[i].ty, BuiltinParamType::SelfType))
            {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: expected_ty.name().to_string(),
                        found: arg_result.ty.name().to_string(),
                    },
                    method_ctx.span,
                ));
            }

            air_args.push((arg_result.air_ref, AirArgMode::Normal));
        }

        // Determine return type
        // Use builtin_air_type for SelfType to get correct AIR output type
        let return_ty = match method.return_ty {
            BuiltinReturnType::Unit => Type::Unit,
            BuiltinReturnType::U64 => Type::U64,
            BuiltinReturnType::U8 => Type::U8,
            BuiltinReturnType::Bool => Type::Bool,
            BuiltinReturnType::SelfType => self.builtin_air_type(method_ctx.struct_id),
        };

        // Generate runtime function call
        let call_name = self.interner.get_or_intern(method.runtime_fn);

        // Encode args into extra array
        let mut extra_data: Vec<u32> = Vec::with_capacity(air_args.len() * 2);
        for (air_ref, mode) in &air_args {
            extra_data.push(air_ref.as_u32());
            extra_data.push(mode.as_u32());
        }
        let args_start = air.add_extra(&extra_data);

        let call_ref = air.add_inst(AirInst {
            data: AirInstData::Call {
                name: call_name,
                args_start,
                args_len: air_args.len() as u32,
            },
            ty: return_ty,
            span: method_ctx.span,
        });

        // For mutation methods, store the result back to the receiver
        if method.receiver_mode == ReceiverMode::ByMutRef {
            let storage = receiver.storage.ok_or_else(|| {
                CompileError::new(ErrorKind::InvalidAssignmentTarget, method_ctx.span)
            })?;
            return self.store_string_result(air, call_ref, storage, method_ctx.span);
        }

        Ok(AnalysisResult::new(call_ref, return_ty))
    }

    /// Get the storage location for a String receiver in a mutation method call.
    ///
    /// For mutation methods like `push_str`, `push`, `clear`, `reserve`, we need
    /// to know where to store the updated String after the runtime function returns.
    ///
    /// Returns `Some(storage)` if the receiver is a mutable local or inout parameter.
    /// Returns an error if the receiver is:
    /// - An immutable binding (`let` instead of `var`)
    /// - A borrow parameter (can't mutate borrowed values)
    /// - Not an lvalue (e.g., a function call result)
    fn get_string_receiver_storage(
        &self,
        receiver_ref: InstRef,
        ctx: &AnalysisContext,
        span: Span,
    ) -> CompileResult<Option<StringReceiverStorage>> {
        let receiver_inst = self.rir.get(receiver_ref);

        match &receiver_inst.data {
            InstData::VarRef { name } => {
                // Check if this is a parameter
                if let Some(param_info) = ctx.params.get(name) {
                    // Check parameter mode
                    match param_info.mode {
                        RirParamMode::Inout => {
                            return Ok(Some(StringReceiverStorage::Param {
                                abi_slot: param_info.abi_slot,
                            }));
                        }
                        RirParamMode::Borrow => {
                            let name_str = self.interner.resolve(&*name);
                            return Err(CompileError::new(
                                ErrorKind::MutateBorrowedValue {
                                    variable: name_str.to_string(),
                                },
                                span,
                            ));
                        }
                        RirParamMode::Normal => {
                            // Normal parameters can be mutated if declared as `var`
                            // For now, we don't allow mutation of normal params
                            let name_str = self.interner.resolve(&*name);
                            return Err(CompileError::new(
                                ErrorKind::AssignToImmutable(name_str.to_string()),
                                span,
                            ));
                        }
                    }
                }

                // Check if it's a local variable
                if let Some(local) = ctx.locals.get(name) {
                    if !local.is_mut {
                        let name_str = self.interner.resolve(&*name);
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(name_str.to_string()),
                            span,
                        ));
                    }
                    return Ok(Some(StringReceiverStorage::Local { slot: local.slot }));
                }

                // Variable not found
                let name_str = self.interner.resolve(&*name);
                Err(CompileError::new(
                    ErrorKind::UndefinedVariable(name_str.to_string()),
                    span,
                ))
            }

            // For other receiver types (field access, function calls, etc.),
            // we don't support mutation for now
            _ => Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span)),
        }
    }

    /// Store the result of a String mutation method back to the receiver's storage.
    ///
    /// Returns a Unit-typed result since mutation methods don't return a value.
    fn store_string_result(
        &self,
        air: &mut Air,
        call_ref: AirRef,
        storage: StringReceiverStorage,
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        let store_ref = match storage {
            StringReceiverStorage::Local { slot } => air.add_inst(AirInst {
                data: AirInstData::Store {
                    slot,
                    value: call_ref,
                },
                ty: Type::Unit,
                span,
            }),
            StringReceiverStorage::Param { abi_slot } => air.add_inst(AirInst {
                data: AirInstData::ParamStore {
                    param_slot: abi_slot,
                    value: call_ref,
                },
                ty: Type::Unit,
                span,
            }),
        };

        Ok(AnalysisResult::new(store_ref, Type::Unit))
    }

    /// Check if directives contain @allow for a specific warning name.
    pub(crate) fn has_allow_directive(
        &self,
        directives: &[RirDirective],
        warning_name: &str,
    ) -> bool {
        let allow_sym = self.interner.get("allow");
        let warning_sym = self.interner.get(warning_name);

        for directive in directives {
            if Some(directive.name) == allow_sym {
                for arg in &directive.args {
                    if Some(*arg) == warning_sym {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Check for unused local variables in the current scope (before popping it).
    /// Uses the scope stack to determine which variables were added in the current scope.
    pub(crate) fn check_unused_locals_in_current_scope(&self, ctx: &mut AnalysisContext) {
        // Get the current scope entries (variables added in this scope)
        let Some(current_scope) = ctx.scope_stack.last() else {
            return;
        };

        for (symbol, _old_value) in current_scope {
            // Skip if variable was used
            if ctx.used_locals.contains(symbol) {
                continue;
            }

            // Get the local var info (it should still be in ctx.locals before pop)
            let Some(local) = ctx.locals.get(symbol) else {
                continue;
            };

            // Get variable name
            let name = self.interner.resolve(&*symbol);

            // Skip variables starting with underscore (convention for intentionally unused)
            if name.starts_with('_') {
                continue;
            }

            // Skip if @allow(unused_variable) was applied
            if local.allow_unused {
                continue;
            }

            // Emit warning with help suggestion (to ctx.warnings for parallel safety)
            ctx.warnings.push(
                CompileWarning::new(WarningKind::UnusedVariable(name.to_string()), local.span)
                    .with_help(format!(
                        "if this is intentional, prefix it with an underscore: `_{}`",
                        name
                    )),
            );
        }
    }

    /// Check for unconsumed linear values in the current scope (before popping it).
    /// Linear values MUST be consumed (moved) - it's an error to let them drop implicitly.
    /// Returns an error if any linear value was not consumed.
    pub(crate) fn check_unconsumed_linear_values(
        &self,
        ctx: &AnalysisContext,
    ) -> CompileResult<()> {
        // Get the current scope entries (variables added in this scope)
        let Some(current_scope) = ctx.scope_stack.last() else {
            return Ok(());
        };

        for (symbol, _old_value) in current_scope {
            // Get the local var info (it should still be in ctx.locals before pop)
            let Some(local) = ctx.locals.get(symbol) else {
                continue;
            };

            // Only check linear types
            if !self.is_type_linear(local.ty) {
                continue;
            }

            // Check if this variable was moved (consumed)
            let was_consumed = ctx
                .moved_vars
                .get(symbol)
                .is_some_and(|state| state.full_move.is_some());

            if !was_consumed {
                let name = self.interner.resolve(&*symbol);
                return Err(CompileError::new(
                    ErrorKind::LinearValueNotConsumed(name.to_string()),
                    local.span,
                ));
            }
        }

        Ok(())
    }

    /// Extract the root variable symbol from an expression, if it refers to a variable.
    ///
    /// For inout arguments, we need to track which variable is being passed to detect
    /// when the same variable is passed to multiple inout parameters.
    ///
    /// Returns Some(symbol) for:
    /// - VarRef { name } -> the variable symbol
    /// - ParamRef { name, .. } -> the parameter symbol
    /// - FieldGet { base, .. } -> recursively extract from base
    /// - IndexGet { base, .. } -> recursively extract from base
    ///
    /// Returns None for expressions that don't refer to a variable (literals, calls, etc.)
    pub(crate) fn extract_root_variable(&self, inst_ref: InstRef) -> Option<Spur> {
        let inst = self.rir.get(inst_ref);
        match &inst.data {
            InstData::VarRef { name } => Some(*name),
            InstData::ParamRef { name, .. } => Some(*name),
            InstData::FieldGet { base, .. } => self.extract_root_variable(*base),
            InstData::IndexGet { base, .. } => self.extract_root_variable(*base),
            _ => None,
        }
    }

    /// Extract the root variable symbol and field path from an expression.
    ///
    /// For expressions like `s.a.b`, returns (sym("s"), [sym("a"), sym("b")]).
    /// For `s`, returns (sym("s"), []).
    ///
    /// Returns None for expressions that don't refer to a variable (literals, calls, etc.)
    pub(crate) fn extract_field_path(&self, inst_ref: InstRef) -> Option<(Spur, FieldPath)> {
        let mut path = Vec::new();
        let root = self.extract_field_path_inner(inst_ref, &mut path)?;
        // Path is built in reverse order, so reverse it
        path.reverse();
        Some((root, path))
    }

    /// Helper for extract_field_path that builds the path in reverse order.
    fn extract_field_path_inner(&self, inst_ref: InstRef, path: &mut FieldPath) -> Option<Spur> {
        let inst = self.rir.get(inst_ref);
        match &inst.data {
            InstData::VarRef { name } => Some(*name),
            InstData::ParamRef { name, .. } => Some(*name),
            InstData::FieldGet { base, field } => {
                path.push(*field);
                self.extract_field_path_inner(*base, path)
            }
            // For index expressions, we stop tracking the field path
            // (index-based moves are more complex and not addressed here)
            InstData::IndexGet { .. } => None,
            _ => None,
        }
    }

    /// Check exclusivity rules for inout and borrow parameters in a call.
    ///
    /// This enforces two rules:
    /// 1. Same variable cannot be passed to multiple inout parameters (prevents aliasing)
    /// 2. Same variable cannot be passed to both inout and borrow (law of exclusivity)
    ///
    /// The law of exclusivity: either one mutable (inout) access OR any number of
    /// immutable (borrow) accesses, never both simultaneously.
    pub(crate) fn check_exclusive_access(
        &self,
        args: &[RirCallArg],
        call_span: Span,
    ) -> CompileResult<()> {
        use std::collections::HashSet;
        let mut inout_vars: HashSet<Spur> = HashSet::new();
        let mut borrow_vars: HashSet<Spur> = HashSet::new();

        for arg in args {
            let maybe_var_symbol = self.extract_root_variable(arg.value);

            // Check that inout/borrow arguments are lvalues
            if arg.is_inout() && maybe_var_symbol.is_none() {
                return Err(CompileError::new(
                    ErrorKind::InoutNonLvalue,
                    self.rir.get(arg.value).span,
                ));
            }
            if arg.is_borrow() && maybe_var_symbol.is_none() {
                return Err(CompileError::new(
                    ErrorKind::BorrowNonLvalue,
                    self.rir.get(arg.value).span,
                ));
            }

            if let Some(var_symbol) = maybe_var_symbol {
                if arg.is_inout() {
                    // Check for duplicate inout access
                    if !inout_vars.insert(var_symbol) {
                        let var_name = self.interner.resolve(&var_symbol).to_string();
                        return Err(CompileError::new(
                            ErrorKind::InoutExclusiveAccess { variable: var_name },
                            call_span,
                        ));
                    }
                    // Check for borrow/inout conflict
                    if borrow_vars.contains(&var_symbol) {
                        let var_name = self.interner.resolve(&var_symbol).to_string();
                        return Err(CompileError::new(
                            ErrorKind::BorrowInoutConflict { variable: var_name },
                            call_span,
                        ));
                    }
                } else if arg.is_borrow() {
                    borrow_vars.insert(var_symbol);
                    // Check for borrow/inout conflict
                    if inout_vars.contains(&var_symbol) {
                        let var_name = self.interner.resolve(&var_symbol).to_string();
                        return Err(CompileError::new(
                            ErrorKind::BorrowInoutConflict { variable: var_name },
                            call_span,
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    /// Analyze a list of call arguments, handling inout unmove logic.
    ///
    /// For inout arguments, the variable is "unmoving" after analysis - this is because
    /// inout is a mutable borrow, not a move. The value stays valid after the call.
    pub(crate) fn analyze_call_args(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Vec<AirCallArg>> {
        let mut air_args = Vec::new();
        for arg in args.iter() {
            // For inout/borrow arguments, extract the variable name before analysis
            // so we can "unmove" it after - these are borrows, not moves
            let borrowed_var = if arg.is_inout() || arg.is_borrow() {
                self.extract_root_variable(arg.value)
            } else {
                None
            };

            let arg_result = self.analyze_inst(air, arg.value, ctx)?;

            // If this was an inout/borrow argument, the variable shouldn't be marked as moved
            // because these are borrows - the value stays valid after the call
            if let Some(var_symbol) = borrowed_var {
                ctx.moved_vars.remove(&var_symbol);
            }

            air_args.push(AirCallArg {
                value: arg_result.air_ref,
                mode: Self::convert_arg_mode(arg.mode),
            });
        }
        Ok(air_args)
    }
}
