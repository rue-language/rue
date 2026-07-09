//! Function body analysis and AIR generation.
//!
//! This module contains the core semantic analysis functionality:
//! - Function analysis (analyze_single_function, analyze_method_function, analyze_destructor_function)
//! - Hindley-Milner type inference (run_type_inference)
//! - RIR to AIR instruction lowering (analyze_inst)
//! - Helper functions for expression analysis
//!
//! Two drivers share these analysis functions: the eager path (single-file
//! compilation analyzes every function) and the lazy path (multi-file
//! compilation analyzes only reachable functions, per ADR-0026).

use std::collections::{HashMap, HashSet};

use lasso::{Key, Spur, ThreadedRodeo};
use rue_builtins::{BuiltinReturnType, BuiltinTypeDef};
use rue_error::{
    CompileError, CompileErrors, CompileResult, CompileWarning, ErrorKind,
    IntrinsicTypeMismatchError, MultiErrorResult, OptionExt, PreviewFeature, WarningKind,
};
use rue_rir::{InstData, InstRef, Rir, RirArgMode, RirCallArg, RirDirective, RirParamMode};
use rue_span::{FileId, Span};
use rue_target::{Arch, Os};

use super::context::{
    AnalysisContext, AnalysisResult, BuiltinMethodContext, ConstValue, ParamInfo, ReceiverInfo,
    StringReceiverStorage,
};
use super::{AnalyzedFunction, InferenceContext, MethodInfo, Sema, SemaOutput};
use crate::inference::{
    Constraint, ConstraintContext, ConstraintGenerator, InferType, ParamVarInfo, Unifier,
    UnifyResult,
};
use crate::inst::{
    Air, AirArgMode, AirCallArg, AirInst, AirInstData, AirPlaceBase, AirProjection, AirRef,
};
use crate::types::{ModuleId, StructField, StructId, Type, TypeKind};

/// Main entry point for analyzing all function bodies.
///
/// Called from Sema::analyze_all after declarations are collected.
/// Uses the lazy driver for import graphs and the eager driver for single-file
/// compilations.
pub(crate) fn analyze_all_function_bodies(mut sema: Sema<'_>) -> MultiErrorResult<SemaOutput> {
    // Use lazy analysis when imports are present (multi-file compilation)
    // This ensures only reachable code is analyzed, per ADR-0026
    let result = if sema.has_imports() {
        analyze_function_bodies_lazy(&mut sema)
    } else {
        // Use eager analysis for single-file compilation (backwards compatibility)
        analyze_all_function_bodies_sequential(&mut sema)
    };

    // Sema→CFG boundary invariant (RUE-153): a value may only carry the
    // `<error>` type as part of error recovery, i.e. when at least one
    // diagnostic has already been emitted. If analysis reports `Ok` (no
    // errors) yet some AIR value is still `<error>`-typed, an inference
    // variable decayed to `<error>` on a path that forgot to emit its
    // diagnostic (the RUE-149 class). That value would otherwise reach
    // codegen and hit an `unreachable!()`; convert it into an actionable
    // internal-error diagnostic (E9000) here instead.
    if let Ok(output) = &result {
        if let Some(err) = find_undiagnosed_error_type(output) {
            return Err(CompileErrors::from(err));
        }
    }

    result
}

/// Scan analyzed AIR for an `<error>`-typed value that survived analysis with
/// no diagnostic emitted (see the invariant in `analyze_all_function_bodies`).
///
/// Returns the first offending instruction as an internal-error `CompileError`
/// (E9000), or `None` when every value is well-typed. Only called on the
/// success (`Ok`) path, so any `<error>` found here is by definition
/// undiagnosed and indicates a compiler bug.
fn find_undiagnosed_error_type(output: &SemaOutput) -> Option<CompileError> {
    for func in &output.functions {
        if func.air.return_type().is_error() {
            return Some(CompileError::without_span(ErrorKind::InternalError(
                format!(
                    "function '{}' has an <error> return type but no diagnostic was \
                 emitted; an inference variable decayed to <error> without \
                 reporting an error (RUE-153)",
                    func.name
                ),
            )));
        }
        for (_air_ref, inst) in func.air.iter() {
            if inst.ty.is_error() {
                return Some(CompileError::new(
                    ErrorKind::InternalError(format!(
                        "an <error>-typed value reached the end of semantic \
                         analysis in function '{}' but no diagnostic was \
                         emitted; an inference variable decayed to <error> \
                         without reporting an error (RUE-153)",
                        func.name
                    )),
                    inst.span,
                ));
            }
        }
    }
    None
}

/// Sequential analysis path (current implementation).
fn analyze_all_function_bodies_sequential(sema: &mut Sema<'_>) -> MultiErrorResult<SemaOutput> {
    // Build inference context once
    let infer_ctx = sema.build_inference_context();

    // Collect analyzed functions with their local strings.
    let mut functions_with_strings: Vec<(AnalyzedFunction, Vec<String>)> = Vec::new();
    let mut errors = CompileErrors::new();
    let mut all_warnings = Vec::new();
    let mut referenced_functions = HashSet::new();

    // Collect method refs from struct declarations to skip them when analyzing regular functions
    let mut method_refs: HashSet<InstRef> = HashSet::new();
    for (_, inst) in sema.rir.iter() {
        match &inst.data {
            InstData::StructDecl {
                methods_start,
                methods_len,
                ..
            } => {
                let methods = sema.rir.get_inst_refs(*methods_start, *methods_len);
                for method_ref in methods {
                    method_refs.insert(method_ref);
                }
            }
            // Also collect methods from anonymous structs (inside comptime functions like Vec<T>)
            InstData::AnonStructType {
                methods_start,
                methods_len,
                ..
            } => {
                if *methods_len > 0 {
                    let methods = sema.rir.get_inst_refs(*methods_start, *methods_len);
                    for method_ref in methods {
                        method_refs.insert(method_ref);
                    }
                }
            }
            _ => {}
        }
    }

    // Analyze regular functions (skip generic functions - they're analyzed during specialization)
    for (inst_ref, inst) in sema.rir.iter() {
        if let InstData::FnDecl {
            name,
            params_start,
            params_len,
            return_type,
            body,
            has_self,
            ..
        } = &inst.data
        {
            if method_refs.contains(&inst_ref) {
                continue;
            }

            // Skip methods (has_self = true) - these are handled elsewhere:
            // - Named struct methods are collected below via StructDecl
            // - Anonymous struct methods are analyzed in the fixed-point loop later
            if *has_self {
                continue;
            }

            let function_key = sema
                .resolve_function_name_local(*name, inst.span.file_id)
                .unwrap_or(*name);

            // Skip FnDecls that are not in the functions table.
            // These are anonymous struct methods which are analyzed separately.
            if !sema.functions.contains_key(&function_key) {
                continue;
            }

            let Some(fn_info) = sema.functions.get(&function_key).copied() else {
                continue;
            };
            // Skip functions with comptime parameters - they are analyzed per specialization
            if fn_info.is_generic {
                continue;
            }

            let fn_name = sema.interner.resolve(&function_key).to_string();
            let params = sema.rir.get_params(*params_start, *params_len);

            match sema.analyze_single_function(
                &infer_ctx,
                &fn_name,
                *return_type,
                &params,
                *body,
                inst.span,
                fn_info.allow_unused_variable,
                fn_info.allow_unreachable_code,
            ) {
                Ok((analyzed, warnings, local_strings, mut ref_fns, _ref_meths)) => {
                    functions_with_strings.push((analyzed, local_strings));
                    all_warnings.extend(warnings);
                    ref_fns.remove(&function_key);
                    referenced_functions.extend(ref_fns);
                }
                Err(e) => errors.push(e),
            }
        }
    }

    // Analyze method bodies from struct declarations
    for (_, inst) in sema.rir.iter() {
        if let InstData::StructDecl {
            name: type_name,
            methods_start,
            methods_len,
            ..
        } = &inst.data
        {
            let type_name_str = sema.interner.resolve(&*type_name).to_string();
            let struct_id = match sema.structs.get(type_name) {
                Some(id) => *id,
                None => {
                    errors.push(CompileError::new(
                        ErrorKind::InternalError(format!(
                            "struct '{}' not found in struct map during method analysis",
                            type_name_str
                        )),
                        inst.span,
                    ));
                    continue;
                }
            };
            let struct_type = Type::new_struct(struct_id);

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
                    self_mode,
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
                        *self_mode,
                    ) {
                        Ok((analyzed, warnings, local_strings, ref_fns, _ref_meths)) => {
                            functions_with_strings.push((analyzed, local_strings));
                            all_warnings.extend(warnings);
                            referenced_functions.extend(ref_fns);
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
            let struct_type = Type::new_struct(struct_id);
            let full_name = format!("{}.__drop", type_name_str);

            match sema.analyze_destructor_function(
                &infer_ctx,
                &full_name,
                *body,
                inst.span,
                struct_type,
            ) {
                Ok((analyzed, warnings, local_strings, ref_fns, _ref_meths)) => {
                    functions_with_strings.push((analyzed, local_strings));
                    all_warnings.extend(warnings);
                    referenced_functions.extend(ref_fns);
                }
                Err(e) => errors.push(e),
            }
        }
    }

    // Analyze methods for anonymous structs.
    // These are registered during comptime evaluation of function bodies, so they
    // aren't in any named StructDecl. We use a fixed-point loop since analyzing one
    // method may create new anonymous struct types with their own methods.
    let mut analyzed_anon_methods: HashSet<(StructId, Spur)> = HashSet::new();
    // The reserved name a `drop fn(self)` destructor is carried under inside an
    // anonymous struct body (RUE-312); such a method is analyzed as a destructor.
    let drop_marker_sym = sema.interner.get_or_intern("__drop");
    loop {
        // Collect anonymous struct methods that haven't been analyzed yet
        let pending_anon_methods: Vec<(StructId, Spur, MethodInfo)> = sema
            .methods
            .iter()
            .filter_map(|((struct_id, method_name), method_info)| {
                // Check if this is an anonymous struct
                let struct_def = sema.type_pool.struct_def(*struct_id);
                if struct_def.name.starts_with("__anon_struct_")
                    && !analyzed_anon_methods.contains(&(*struct_id, *method_name))
                {
                    Some((*struct_id, *method_name, method_info.clone()))
                } else {
                    None
                }
            })
            .collect();

        if pending_anon_methods.is_empty() {
            break;
        }

        for (struct_id, method_name, method_info) in pending_anon_methods {
            analyzed_anon_methods.insert((struct_id, method_name));

            let struct_def = sema.type_pool.struct_def(struct_id);
            let type_name_str = struct_def.name.clone();
            let method_name_str = sema.interner.resolve(&method_name).to_string();

            let full_name = if method_info.has_self {
                format!("{}.{}", type_name_str, method_name_str)
            } else {
                format!("{}::{}", type_name_str, method_name_str)
            };

            // Build param_info from MethodInfo's ParamRange
            let param_names = sema.param_arena.names(method_info.params);
            let param_types = sema.param_arena.types(method_info.params);
            let param_modes = sema.param_arena.modes(method_info.params);
            let param_comptime = sema.param_arena.comptime(method_info.params);

            let mut param_info: Vec<(Spur, Type, RirParamMode, bool)> = Vec::new();

            if method_info.has_self {
                // Add self parameter in the receiver's declared mode
                // (by-value `self`, or by-ref `borrow`/`inout self`; RUE-15).
                let self_sym = sema.interner.get_or_intern("self");
                param_info.push((
                    self_sym,
                    method_info.struct_type,
                    method_info.self_mode,
                    false,
                ));
            }

            // Add regular parameters (convert from arena slices)
            for i in 0..param_names.len() {
                param_info.push((
                    param_names[i],
                    param_types[i],
                    param_modes[i],
                    param_comptime[i],
                ));
            }

            // Retrieve captured comptime values from struct-level storage
            // Clone the HashMap to avoid borrowing issues with mutable analyze_method_body call
            let struct_id = method_info
                .struct_type
                .as_struct()
                .expect("method must belong to struct");
            let captured_values = sema
                .anon_struct_captured_values
                .get(&struct_id)
                .cloned()
                .unwrap_or_else(HashMap::new);
            let enclosing_type_subst = sema
                .anon_struct_type_subst
                .get(&struct_id)
                .cloned()
                .unwrap_or_else(HashMap::new);

            // A `drop fn(self)` in an anonymous struct body is carried under the
            // reserved `__drop` method name (RUE-312). It must be analyzed as a
            // *destructor*, not an ordinary method: `self` is consumed and the
            // drop glue (not the destructor) drops the fields afterwards, so the
            // destructor's own param-drop list is cleared and a self-move out of
            // it is rejected — exactly like a named struct's `drop fn`. The
            // enclosing `-> type` constructor's params (`T`) are threaded through
            // to BOTH paths (RUE-313), so a generic destructor
            // (`@free(self.buf, self.cap)`) monomorphizes per instantiation.
            let is_destructor = method_name == drop_marker_sym;

            let analysis_result = if is_destructor {
                sema.analyze_anon_destructor_body(
                    &infer_ctx,
                    &param_info,
                    method_info.body,
                    method_info.struct_type,
                    &captured_values,
                    &full_name,
                    &enclosing_type_subst,
                )
            } else {
                sema.analyze_method_body(
                    &infer_ctx,
                    method_info.return_type,
                    &param_info,
                    method_info.body,
                    method_info.struct_type,
                    &captured_values,
                    &enclosing_type_subst,
                )
            };

            match analysis_result {
                Ok((
                    air,
                    num_locals,
                    num_param_slots,
                    param_modes_result,
                    warnings,
                    local_strings,
                    ref_fns,
                    _ref_meths,
                )) => {
                    let analyzed = AnalyzedFunction {
                        name: full_name,
                        air,
                        num_locals,
                        num_param_slots,
                        param_modes: param_modes_result,
                        allow_unreachable_code: false,
                    };
                    functions_with_strings.push((analyzed, local_strings));
                    all_warnings.extend(warnings);
                    referenced_functions.extend(ref_fns);
                }
                Err(e) => errors.push(e),
            }
        }
    }

    finalize_function_body_analysis(
        sema,
        &infer_ctx,
        functions_with_strings,
        all_warnings,
        &referenced_functions,
        errors,
    )
}

fn finalize_function_body_analysis(
    sema: &mut Sema<'_>,
    infer_ctx: &InferenceContext,
    functions_with_strings: Vec<(AnalyzedFunction, Vec<String>)>,
    mut all_warnings: Vec<CompileWarning>,
    unused_function_roots: &HashSet<Spur>,
    mut errors: CompileErrors,
) -> MultiErrorResult<SemaOutput> {
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

    let mut referenced_for_unused_warnings = collect_static_function_references(sema);
    referenced_for_unused_warnings.extend(unused_function_roots.iter().copied());
    add_unused_function_warnings(sema, &referenced_for_unused_warnings, &mut all_warnings);
    all_warnings.sort_by_key(|w| w.span().map(|s| s.start));

    let mut output = SemaOutput {
        functions,
        strings: global_strings,
        warnings: all_warnings,
        // Provisional: refreshed after specialization below. Specialized
        // bodies can intern *new* composite types (e.g. `[i32; N]` from a
        // `let y: [T; 2]` once `T := i32`) into `sema.type_pool`, so the pool
        // handed to CFG/codegen must be cloned *after* the specialize pass.
        type_pool: sema.type_pool.clone(),
    };

    // Run specialization pass to rewrite CallGeneric instructions to Call
    // and create specialized function bodies
    if let Err(e) = crate::specialize::specialize(&mut output, sema, infer_ctx, sema.interner) {
        errors.push(e);
    }

    // Specialization interns new composite types into `sema.type_pool` (see the
    // provisional-clone note above); re-snapshot so every array/pointer type a
    // specialized body references exists in the pool CFG and codegen consult.
    // Without this, a specialization-only `[i32; N]` decayed to an out-of-bounds
    // ArrayTypeId and ICE'd in drop analysis (RUE-282).
    output.type_pool = sema.type_pool.clone();

    errors.into_result_with(output)
}

/// Emit warnings for unused free functions.
///
/// This intentionally excludes methods/destructors; they have different
/// reachability rules and are not covered by the current spec/UI cases.
fn add_unused_function_warnings(
    sema: &Sema<'_>,
    referenced_functions: &HashSet<Spur>,
    warnings: &mut Vec<CompileWarning>,
) {
    let main_sym = sema.interner.get("main");

    for (name, info) in &sema.functions {
        let source_name = sema.source_function_name(*name);
        let name_str = sema.interner.resolve(&source_name);
        if Some(*name) == main_sym
            || info.is_pub
            || info.allow_unused_function
            || name_str.starts_with('_')
            || referenced_functions.contains(name)
        {
            continue;
        }

        warnings.push(
            CompileWarning::new(WarningKind::UnusedFunction(name_str.to_string()), info.span)
                .with_help(format!(
                    "if this is intentional, prefix it with an underscore: `_{name_str}`"
                )),
        );
    }
}

fn collect_static_function_references(sema: &Sema<'_>) -> HashSet<Spur> {
    let mut referenced = HashSet::new();

    for (_, inst) in sema.rir.iter() {
        let InstData::Call { name, .. } = &inst.data else {
            continue;
        };

        let mut target = *name;
        let mut resolved_alias = false;
        if let Some(const_info) = sema.resolve_const_info_in_file(target, inst.span.file_id)
            && let Some(callee) = const_info.value.as_function()
        {
            target = callee;
            resolved_alias = true;
        }

        if resolved_alias && sema.functions.contains_key(&target) {
            referenced.insert(target);
        } else if let Some(function_key) =
            sema.resolve_function_name_local(target, inst.span.file_id)
        {
            referenced.insert(function_key);
        }
    }

    referenced
}

/// Move newly referenced functions/methods onto the lazy-analysis work queues
/// in a deterministic order.
///
/// `analyze_single_function` (and its method/destructor siblings) collect
/// references as `HashSet`s, whose iteration order is randomized per process.
/// Pushing them unsorted made the whole lazy-analysis order — and with it the
/// order diagnostics are emitted in AND the function order handed to codegen —
/// differ between identical runs (RUE-513: two files with independent sema
/// errors reported them in a random relative order). Sorting by resolved name
/// (and struct id) restores run-to-run determinism at the only place the
/// nondeterminism enters.
fn enqueue_references_sorted(
    interner: &ThreadedRodeo,
    referenced_fns: HashSet<Spur>,
    referenced_meths: HashSet<(StructId, Spur)>,
    analyzed_functions: &HashSet<Spur>,
    analyzed_methods: &HashSet<(StructId, Spur)>,
    pending_functions: &mut Vec<Spur>,
    pending_methods: &mut Vec<(StructId, Spur)>,
) {
    let mut fns: Vec<Spur> = referenced_fns
        .into_iter()
        .filter(|f| !analyzed_functions.contains(f))
        .collect();
    fns.sort_by_key(|f| interner.resolve(f));
    pending_functions.extend(fns);

    let mut meths: Vec<(StructId, Spur)> = referenced_meths
        .into_iter()
        .filter(|m| !analyzed_methods.contains(m))
        .collect();
    meths.sort_by_key(|&(sid, name)| (sid.0, interner.resolve(&name)));
    pending_methods.extend(meths);
}

/// Lazy analysis path (Phase 3 of module system, ADR-0026).
///
/// This implements "lazy semantic analysis" where only functions reachable from
/// the entry point (main) are analyzed. Unreferenced code is not analyzed,
/// not codegen'd, and errors in unreferenced code are not reported.
///
/// This is the same trade-off Zig makes for faster builds and smaller binaries.
fn analyze_function_bodies_lazy(sema: &mut Sema<'_>) -> MultiErrorResult<SemaOutput> {
    // Build inference context once
    let infer_ctx = sema.build_inference_context();

    // Find main() function - this is the entry point for lazy analysis
    let main_sym = match sema.interner.get("main") {
        Some(sym) if sema.functions.contains_key(&sym) => sym,
        _ => {
            // No main function found - this is an error
            return Err(CompileErrors::from(CompileError::without_span(
                ErrorKind::NoMainFunction,
            )));
        }
    };

    // Work queue: functions/methods to analyze
    // Start with main()
    let mut pending_functions: Vec<Spur> = vec![main_sym];
    let mut analyzed_functions: HashSet<Spur> = HashSet::new();
    let mut pending_methods: Vec<(StructId, Spur)> = Vec::new();
    let mut analyzed_methods: HashSet<(StructId, Spur)> = HashSet::new();
    let drop_marker_sym = sema.interner.get_or_intern("__drop");

    // Collect results
    let mut functions_with_strings: Vec<(AnalyzedFunction, Vec<String>)> = Vec::new();
    let mut errors = CompileErrors::new();
    let mut all_warnings = Vec::new();

    // Collect method refs from struct declarations (for later lookup)
    let mut method_refs: HashSet<InstRef> = HashSet::new();
    for (_, inst) in sema.rir.iter() {
        if let InstData::StructDecl {
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

    // Process work queue until empty
    while !pending_functions.is_empty() || !pending_methods.is_empty() {
        // Process pending functions
        while let Some(fn_name) = pending_functions.pop() {
            if analyzed_functions.contains(&fn_name) {
                continue;
            }
            analyzed_functions.insert(fn_name);

            // Look up the function info
            let fn_info = match sema.functions.get(&fn_name) {
                Some(info) => *info,
                None => continue, // Should not happen, but be defensive
            };

            // Skip functions with comptime parameters - they are analyzed per specialization
            if fn_info.is_generic {
                continue;
            }

            let fn_name_str = sema.interner.resolve(&fn_name).to_string();

            // Find the function declaration in RIR to get params
            let mut found = false;
            for (inst_ref, inst) in sema.rir.iter() {
                if let InstData::FnDecl {
                    name,
                    params_start,
                    params_len,
                    return_type,
                    body,
                    ..
                } = &inst.data
                {
                    let function_key = sema
                        .resolve_function_name_local(*name, inst.span.file_id)
                        .unwrap_or(*name);
                    if function_key == fn_name && !method_refs.contains(&inst_ref) {
                        found = true;
                        let params = sema.rir.get_params(*params_start, *params_len);

                        match sema.analyze_single_function(
                            &infer_ctx,
                            &fn_name_str,
                            *return_type,
                            &params,
                            *body,
                            inst.span,
                            fn_info.allow_unused_variable,
                            fn_info.allow_unreachable_code,
                        ) {
                            Ok((
                                analyzed,
                                warnings,
                                local_strings,
                                referenced_fns,
                                referenced_meths,
                            )) => {
                                functions_with_strings.push((analyzed, local_strings));
                                all_warnings.extend(warnings);

                                // Add newly referenced functions to the work queue
                                enqueue_references_sorted(
                                    sema.interner,
                                    referenced_fns,
                                    referenced_meths,
                                    &analyzed_functions,
                                    &analyzed_methods,
                                    &mut pending_functions,
                                    &mut pending_methods,
                                );
                            }
                            Err(e) => errors.push(e),
                        }
                        break;
                    }
                }
            }

            if !found {
                // This could be a builtin or otherwise non-existent function
                // Just skip it
            }
        }

        // Process pending methods
        while let Some((struct_id, method_name)) = pending_methods.pop() {
            if analyzed_methods.contains(&(struct_id, method_name)) {
                continue;
            }
            analyzed_methods.insert((struct_id, method_name));

            // Look up the method info
            let method_info = match sema.methods.get(&(struct_id, method_name)) {
                Some(info) => info.clone(),
                None => continue,
            };

            // Get the struct definition to find its name for impl block lookup
            let struct_def = sema.type_pool.struct_def(struct_id);
            let type_name_str = struct_def.name.clone();
            let type_name_sym = sema.interner.get_or_intern(&type_name_str);
            let method_name_str = sema.interner.resolve(&method_name).to_string();

            // For anonymous structs, use the MethodInfo directly since there's no named StructDecl
            if type_name_str.starts_with("__anon_struct_") {
                let full_name = if method_info.has_self {
                    format!("{}.{}", type_name_str, method_name_str)
                } else {
                    format!("{}::{}", type_name_str, method_name_str)
                };

                // Build param_info from MethodInfo's ParamRange
                let param_names = sema.param_arena.names(method_info.params);
                let param_types = sema.param_arena.types(method_info.params);
                let param_modes = sema.param_arena.modes(method_info.params);
                let param_comptime = sema.param_arena.comptime(method_info.params);

                let mut param_info: Vec<(Spur, Type, RirParamMode, bool)> = Vec::new();

                if method_info.has_self {
                    // Add self parameter in the receiver's declared mode
                    // (by-value `self`, or by-ref `borrow`/`inout self`; RUE-15).
                    let self_sym = sema.interner.get_or_intern("self");
                    param_info.push((
                        self_sym,
                        method_info.struct_type,
                        method_info.self_mode,
                        false,
                    ));
                }

                // Add regular parameters (convert from arena slices)
                for i in 0..param_names.len() {
                    param_info.push((
                        param_names[i],
                        param_types[i],
                        param_modes[i],
                        param_comptime[i],
                    ));
                }

                // Retrieve captured comptime values from struct-level storage
                // Clone the HashMap to avoid borrowing issues with mutable analyze_method_body call
                let struct_id = method_info
                    .struct_type
                    .as_struct()
                    .expect("method must belong to struct");
                let captured_values = sema
                    .anon_struct_captured_values
                    .get(&struct_id)
                    .cloned()
                    .unwrap_or_else(HashMap::new);
                let enclosing_type_subst = sema
                    .anon_struct_type_subst
                    .get(&struct_id)
                    .cloned()
                    .unwrap_or_else(HashMap::new);

                // A `drop fn(self)` in an anonymous struct body is carried
                // under the reserved `__drop` method name (RUE-312). Analyze it
                // as a destructor in the lazy pipeline too: drop glue adds the
                // call implicitly, and destructor analysis has different
                // self-move/drop semantics from an ordinary method.
                let is_destructor = method_name == drop_marker_sym;
                let analysis_result = if is_destructor {
                    sema.analyze_anon_destructor_body(
                        &infer_ctx,
                        &param_info,
                        method_info.body,
                        method_info.struct_type,
                        &captured_values,
                        &full_name,
                        &enclosing_type_subst,
                    )
                } else {
                    sema.analyze_method_body(
                        &infer_ctx,
                        method_info.return_type,
                        &param_info,
                        method_info.body,
                        method_info.struct_type,
                        &captured_values,
                        &enclosing_type_subst,
                    )
                };

                match analysis_result {
                    Ok((
                        air,
                        num_locals,
                        num_param_slots,
                        param_modes_result,
                        warnings,
                        local_strings,
                        referenced_fns,
                        referenced_meths,
                    )) => {
                        let analyzed = AnalyzedFunction {
                            name: full_name,
                            air,
                            num_locals,
                            num_param_slots,
                            param_modes: param_modes_result,
                            allow_unreachable_code: false,
                        };
                        functions_with_strings.push((analyzed, local_strings));
                        all_warnings.extend(warnings);

                        enqueue_references_sorted(
                            sema.interner,
                            referenced_fns,
                            referenced_meths,
                            &analyzed_functions,
                            &analyzed_methods,
                            &mut pending_functions,
                            &mut pending_methods,
                        );
                    }
                    Err(e) => errors.push(e),
                }
                continue;
            }

            // Find the method in struct declarations (for named structs)
            for (_, inst) in sema.rir.iter() {
                if let InstData::StructDecl {
                    name: struct_name,
                    methods_start,
                    methods_len,
                    ..
                } = &inst.data
                {
                    if *struct_name != type_name_sym {
                        continue;
                    }

                    let methods = sema.rir.get_inst_refs(*methods_start, *methods_len);
                    for method_ref in methods {
                        let method_inst = sema.rir.get(method_ref);
                        if let InstData::FnDecl {
                            name: m_name,
                            params_start,
                            params_len,
                            return_type,
                            body,
                            has_self,
                            self_mode,
                            ..
                        } = &method_inst.data
                        {
                            if *m_name != method_name {
                                continue;
                            }

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
                                method_info.struct_type,
                                *has_self,
                                *self_mode,
                            ) {
                                Ok((
                                    analyzed,
                                    warnings,
                                    local_strings,
                                    referenced_fns,
                                    referenced_meths,
                                )) => {
                                    functions_with_strings.push((analyzed, local_strings));
                                    all_warnings.extend(warnings);

                                    enqueue_references_sorted(
                                        sema.interner,
                                        referenced_fns,
                                        referenced_meths,
                                        &analyzed_functions,
                                        &analyzed_methods,
                                        &mut pending_functions,
                                        &mut pending_methods,
                                    );
                                }
                                Err(e) => errors.push(e),
                            }
                        }
                    }
                }
            }
        }

        // Anonymous destructors are not referenced by user-written call AIR;
        // drop glue adds those calls later for instantiated anonymous types.
        // Once comptime evaluation has registered such a destructor, enqueue it
        // so lazy analysis emits `__anon_struct_N.__drop` before the backend
        // links drop glue.
        if pending_functions.is_empty() && pending_methods.is_empty() {
            // sema.methods is a HashMap: sort the enqueued keys so the
            // destructor analysis order is deterministic too (RUE-513).
            let mut anon_dtors: Vec<(StructId, Spur)> = sema
                .methods
                .keys()
                .copied()
                .filter(|&method_key @ (struct_id, method_name)| {
                    method_name == drop_marker_sym
                        && !analyzed_methods.contains(&method_key)
                        && sema
                            .type_pool
                            .struct_def(struct_id)
                            .name
                            .starts_with("__anon_struct_")
                })
                .collect();
            anon_dtors.sort_by_key(|&(sid, name)| (sid.0, sema.interner.resolve(&name)));
            pending_methods.extend(anon_dtors);
        }
    }

    // Also analyze destructors for any structs whose types we've used
    // (This is necessary because drop is implicitly called)
    for (_, inst) in sema.rir.iter() {
        if let InstData::DropFnDecl { type_name, body } = &inst.data {
            let type_name_str = sema.interner.resolve(&*type_name).to_string();
            let struct_id = match sema.structs.get(type_name) {
                Some(id) => *id,
                None => continue,
            };
            let struct_type = Type::new_struct(struct_id);
            let full_name = format!("{}.__drop", type_name_str);

            match sema.analyze_destructor_function(
                &infer_ctx,
                &full_name,
                *body,
                inst.span,
                struct_type,
            ) {
                Ok((analyzed, warnings, local_strings, _, _)) => {
                    functions_with_strings.push((analyzed, local_strings));
                    all_warnings.extend(warnings);
                }
                Err(e) => errors.push(e),
            }
        }
    }

    finalize_function_body_analysis(
        sema,
        &infer_ctx,
        functions_with_strings,
        all_warnings,
        &analyzed_functions,
        errors,
    )
}

/// Reject moving `self` out of a destructor body (RUE-139).
///
/// Dropping a value runs its destructor and then the drop glue; if the
/// destructor moves `self` to a new owner (`consume(self)`, `let x = self`,
/// a by-value method call, ...), that owner drops the value again at ITS
/// scope exit — re-entering the destructor in infinite recursion. This is
/// the spirit of Rust's E0509 (cannot move out of a type implementing Drop).
///
/// Detection: sema wraps every surviving whole-value move of a pass-by-value
/// parameter in an [`AirInstData::MarkMoved`] marker (uses that turn out to
/// be borrows are cancelled in place and leave no marker). A destructor's
/// only parameter is `self` at ABI slot 0, so any whole-value param marker
/// in the analyzed AIR is a move of `self`. Partial field moves
/// (`place: Some(_)`) are not rejected here: they don't re-enter the
/// destructor (the drop-glue double drop of such a field is a separate,
/// pre-existing issue).
fn reject_self_move_in_destructor(air: &Air, full_name: &str) -> CompileResult<()> {
    for (_, inst) in air.iter() {
        if let AirInstData::MarkMoved {
            slot: 0,
            is_param: true,
            place: None,
            ..
        } = inst.data
        {
            let type_name = full_name.strip_suffix(".__drop").unwrap_or(full_name);
            return Err(CompileError::new(
                ErrorKind::MoveSelfOutOfDestructor {
                    type_name: type_name.to_string(),
                },
                inst.span,
            )
            .with_label("`self` is moved out here", inst.span));
        }
    }
    Ok(())
}

/// Build the diagnostic for a move out of an `inout` parameter.
///
/// Rule (RUE-127): moving out of an inout parameter is always rejected, even if
/// the parameter is reassigned afterwards — reinitialization-before-exit is not
/// tracked yet. Without this rule, the call would leave the caller's variable
/// moved-from while the caller still considers it live.
pub(crate) fn move_out_of_inout_error(name: &str, span: Span) -> CompileError {
    CompileError::new(
        ErrorKind::MoveOutOfInout {
            variable: name.to_string(),
        },
        span,
    )
    .with_note(
        "an `inout` parameter is a mutable borrow of the caller's variable; \
         moving its value out would leave the caller's variable uninitialized",
    )
    .with_help(
        "moves out of `inout` parameters are rejected even if the parameter is \
         reassigned before returning (reinitialization is not tracked yet)",
    )
}

/// Build the diagnostic for a non-exhaustive `match` (E0600), naming exactly
/// what is missing (RUE-133).
///
/// - enum scrutinee: lists the uncovered variants ("missing variants: Blue, Green")
/// - bool scrutinee: names the uncovered literal pattern(s)
/// - integer scrutinee: suggests the required wildcard arm
pub(crate) fn non_exhaustive_match_error(
    span: Span,
    scrutinee_type: Type,
    enum_def: Option<&crate::types::EnumDef>,
    variant_covered: impl Fn(u32) -> bool,
    bool_true_covered: bool,
    bool_false_covered: bool,
) -> CompileError {
    let err = CompileError::new(ErrorKind::NonExhaustiveMatch, span);
    if scrutinee_type == Type::BOOL {
        let missing = match (bool_true_covered, bool_false_covered) {
            (false, false) => "patterns `true` and `false` are",
            (false, true) => "pattern `true` is",
            (true, false) => "pattern `false` is",
            // Both covered means the match was exhaustive; we only get here
            // because callers check exhaustiveness first.
            (true, true) => return err,
        };
        err.with_help(format!("{missing} not covered"))
    } else if let Some(def) = enum_def {
        let missing: Vec<&str> = def
            .variants
            .iter()
            .enumerate()
            .filter(|(i, _)| !variant_covered(*i as u32))
            .map(|(_, v)| v.as_str())
            .collect();
        if missing.is_empty() {
            return err;
        }
        err.with_help(format!("missing variants: {}", missing.join(", ")))
    } else {
        err.with_help("integer matches must include a wildcard arm: `_ => ...`")
    }
}

/// Validate that a by-ref (`inout`/`borrow`) call argument is a place — a
/// variable, or a field/index projection chain rooted at one — and return
/// the root variable symbol (RUE-143).
///
/// Codegen passes a by-ref argument by address: place-address formation
/// (frame slot + static field offsets + dynamic index offsets, or a received
/// by-ref pointer minus descending offsets) lives in `rue-codegen`'s shared
/// `byref_args` module. Anything that is not a place (a call result, literal,
/// struct-init expression, arithmetic, ...) has no caller-visible storage to
/// point at and is rejected as a non-lvalue.
fn require_byref_place_arg(rir: &Rir, arg: &RirCallArg) -> CompileResult<Spur> {
    root_variable_of(rir, arg.value).ok_or_else(|| {
        CompileError::new(
            if arg.is_inout() {
                ErrorKind::InoutNonLvalue
            } else {
                ErrorKind::BorrowNonLvalue
            },
            rir.get(arg.value).span,
        )
    })
}

/// Result of the element-wise linear array consumption check (RUE-186); see
/// [`Sema::check_array_elementwise_consumption`].
enum ElementwiseConsumption {
    /// Every element was moved out on every path: the array's must-consume
    /// obligation is satisfied.
    Complete,
    /// No element was ever consumed (or the type is not an array): the
    /// caller reports its usual whole-value diagnostic.
    NotElementwise,
}

/// Intern the move-path segment for a constant array index (RUE-186).
///
/// Element paths reuse the field-path representation: index K becomes the
/// interned decimal string of K. Identifiers can never be all digits, so
/// these segments cannot collide with field names (see
/// [`super::context::FieldPath`]).
pub(crate) fn index_path_segment(interner: &ThreadedRodeo, index: u64) -> Spur {
    interner.get_or_intern(index.to_string())
}

/// True when a move-path segment encodes a constant array index (all-digit
/// interned string; see [`index_path_segment`]).
pub(crate) fn is_index_segment(interner: &ThreadedRodeo, seg: Spur) -> bool {
    let s = interner.resolve(&seg);
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// Format a move path for diagnostics: field segments as `.name`, constant
/// array index segments as `[K]` (e.g. `xs[0]`, `o.a`, `o.items[2].name`).
fn format_move_path(interner: &ThreadedRodeo, root_var: Spur, path: &[Spur]) -> String {
    let mut out = interner.resolve(&root_var).to_string();
    for seg in path {
        let s = interner.resolve(seg);
        if is_index_segment(interner, *seg) {
            out.push_str(&format!("[{s}]"));
        } else {
            out.push('.');
            out.push_str(s);
        }
    }
    out
}

/// The standard fix hint appended to every use-after-move (E0205) diagnostic:
/// Rue's mechanism for using a value without consuming it is to pass it by
/// `borrow`, so naming the moved value makes the suggestion copy-pasteable
/// (RUE-19 item 4). `name` is the value as it appears in the message (a bare
/// variable like `b`, or a path like `o.a`).
pub(crate) fn borrow_instead_of_move_help(name: &str) -> String {
    format!("to use `{name}` after the move, pass it by borrow instead: `borrow {name}`")
}

/// Build the use-after-move error for a field access whose path (or one of
/// its ancestor prefixes) was moved at `moved_span`.
pub(crate) fn use_after_move_path_error(
    interner: &lasso::ThreadedRodeo,
    root_var: Spur,
    field_path: &[Spur],
    span: Span,
    moved_span: Span,
) -> CompileError {
    let path_str = format_move_path(interner, root_var, field_path);
    let help = borrow_instead_of_move_help(&path_str);
    CompileError::new(ErrorKind::UseAfterMove(path_str), span)
        .with_label("value moved here", moved_span)
        .with_help(help)
}

/// Build the error for a linear value that goes out of scope without being
/// consumed on every path.
///
/// `consumed_on_some_path` is the span of a consumption that happened on only
/// SOME paths (if any); when present it selects the more precise "not
/// consumed on all paths" diagnostic over the plain "dropped" one.
pub(crate) fn linear_not_consumed_error(
    name: &str,
    decl_span: Span,
    consumed_on_some_path: Option<Span>,
) -> CompileError {
    match consumed_on_some_path {
        Some(consumed_span) => CompileError::new(
            ErrorKind::LinearValueNotConsumedOnAllPaths(name.to_string()),
            decl_span,
        )
        .with_label("consumed here, but not on every path", consumed_span)
        .with_help(
            "a linear value must be consumed on every path; \
             consume it in the other branches too (paths that diverge, \
             e.g. by returning, are exempt)",
        ),
        None => CompileError::new(
            ErrorKind::LinearValueNotConsumed(name.to_string()),
            decl_span,
        ),
    }
}

/// Extract the root variable symbol from an expression, if it refers to a
/// variable. Canonical, pipeline-agnostic implementation; see
/// [`Sema::extract_root_variable`] for the full contract.
pub(crate) fn root_variable_of(rir: &Rir, inst_ref: InstRef) -> Option<Spur> {
    let inst = rir.get(inst_ref);
    match &inst.data {
        InstData::VarRef { name } => Some(*name),
        InstData::FieldGet { base, .. } => root_variable_of(rir, *base),
        InstData::IndexGet { base, .. } => root_variable_of(rir, *base),
        _ => None,
    }
}

/// Check exclusivity rules for inout and borrow parameters in a call.
///
/// This is the shared implementation behind [`Sema::check_exclusive_access`].
/// It enforces three rules:
/// 1. Inout/borrow arguments must be lvalues (a variable, or a field/index
///    projection chain rooted at one — RUE-143)
/// 2. Same ROOT variable cannot be passed to multiple inout parameters
///    (prevents aliasing; conservatively, even disjoint fields conflict)
/// 3. Same root variable cannot be passed to both inout and borrow (law of
///    exclusivity)
///
/// The law of exclusivity: either one mutable (inout) access OR any number of
/// immutable (borrow) accesses, never both simultaneously.
fn check_exclusive_access_in(
    rir: &Rir,
    interner: &ThreadedRodeo,
    args: &[RirCallArg],
    call_span: Span,
) -> CompileResult<()> {
    let mut inout_vars: HashSet<Spur> = HashSet::new();
    let mut borrow_vars: HashSet<Spur> = HashSet::new();

    for arg in args {
        let maybe_var_symbol = root_variable_of(rir, arg.value);

        // Check that inout/borrow arguments are lvalues
        if arg.is_inout() && maybe_var_symbol.is_none() {
            return Err(CompileError::new(
                ErrorKind::InoutNonLvalue,
                rir.get(arg.value).span,
            ));
        }
        if arg.is_borrow() && maybe_var_symbol.is_none() {
            return Err(CompileError::new(
                ErrorKind::BorrowNonLvalue,
                rir.get(arg.value).span,
            ));
        }

        if let Some(var_symbol) = maybe_var_symbol {
            if arg.is_inout() {
                // Check for duplicate inout access
                if !inout_vars.insert(var_symbol) {
                    let var_name = interner.resolve(&var_symbol).to_string();
                    return Err(CompileError::new(
                        ErrorKind::InoutExclusiveAccess { variable: var_name },
                        call_span,
                    ));
                }
                // Check for borrow/inout conflict
                if borrow_vars.contains(&var_symbol) {
                    let var_name = interner.resolve(&var_symbol).to_string();
                    return Err(CompileError::new(
                        ErrorKind::BorrowInoutConflict { variable: var_name },
                        call_span,
                    ));
                }
            } else if arg.is_borrow() {
                borrow_vars.insert(var_symbol);
                // Check for borrow/inout conflict
                if inout_vars.contains(&var_symbol) {
                    let var_name = interner.resolve(&var_symbol).to_string();
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

/// Shared checks for a module-member function call. Validates module membership, visibility, arity, and argument modes
/// against the callee.
///
/// Membership (spec 4.13:90, RUE-140): a module's type contains only the
/// declarations from the imported file, but the function table is a flat
/// global namespace, so a name-only lookup would resolve a function from
/// any file in the compilation. The callee must be defined in the receiver
/// module's file: `member_file_id` (the callee's source file) must equal the
/// module's canonical file `module_file_id`, otherwise the call is rejected as
/// an unknown member of `module_name`. Comparing by canonical FileId (rather
/// than raw path string) makes equivalent import spellings — `helper.rue` vs
/// `./helper.rue` — resolve members identically (spec 10.2:4, RUE-240).
///
/// Two operations deliberately stay per-pipeline at the call sites: argument
/// analysis (it recurses into the owning pipeline) and the exclusive-access
/// check (whose shared core is `check_exclusive_access_in`, RUE-141).
#[allow(clippy::too_many_arguments)]
fn check_module_member_call(
    rir: &Rir,
    module_name: &str,
    module_file_id: Option<FileId>,
    member_file_id: FileId,
    fn_name_str: &str,
    param_types: &[Type],
    param_modes: &[RirParamMode],
    args: &[RirCallArg],
    accessible: bool,
    span: Span,
) -> CompileResult<()> {
    // Check membership: the function must be defined in the module's file.
    // (Canonical FileId equality, mirroring the struct/enum/const member-access
    // checks in analyze_module_type_member_access.)
    if module_file_id != Some(member_file_id) {
        return Err(CompileError::new(
            ErrorKind::UnknownModuleMember {
                module_name: module_name.to_string(),
                member_name: fn_name_str.to_string(),
            },
            span,
        ));
    }

    // Check visibility: private functions are only accessible from the same directory
    if !accessible {
        return Err(CompileError::new(
            ErrorKind::PrivateMemberAccess {
                item_kind: "function".to_string(),
                name: fn_name_str.to_string(),
            },
            span,
        ));
    }

    // Check argument count
    if args.len() != param_types.len() {
        return Err(CompileError::new(
            ErrorKind::WrongArgumentCount {
                expected: param_types.len(),
                found: args.len(),
            },
            span,
        ));
    }

    // Check that call-site argument modes match function parameter modes
    for (arg, expected_mode) in args.iter().zip(param_modes.iter()) {
        match expected_mode {
            RirParamMode::Inout => {
                if arg.mode != RirArgMode::Inout {
                    return Err(CompileError::new(
                        ErrorKind::InoutKeywordMissing,
                        rir.get(arg.value).span,
                    ));
                }
            }
            RirParamMode::Borrow => {
                if arg.mode != RirArgMode::Borrow {
                    return Err(CompileError::new(
                        ErrorKind::BorrowKeywordMissing,
                        rir.get(arg.value).span,
                    ));
                }
            }
            RirParamMode::Normal => {
                // Normal params accept any mode
            }
            RirParamMode::Comptime => {
                // Comptime params - handled elsewhere
            }
        }
    }
    Ok(())
}

/// Encode the analyzed arguments and emit the Call instruction for a
/// module-member function call.
fn emit_module_member_call(
    air: &mut Air,
    function_name: Spur,
    air_args: &[AirCallArg],
    return_type: Type,
    span: Span,
) -> AnalysisResult {
    let mut extra_data = Vec::with_capacity(air_args.len() * 2);
    for arg in air_args {
        extra_data.push(arg.value.as_u32());
        extra_data.push(arg.mode.as_u32());
    }
    let call_args_start = air.add_extra(&extra_data);
    let call_args_len = air_args.len() as u32;

    let air_ref = air.add_inst(AirInst {
        data: AirInstData::Call {
            name: function_name,
            args_start: call_args_start,
            args_len: call_args_len,
        },
        ty: return_type,
        span,
    });
    AnalysisResult::new(air_ref, return_type)
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

    /// Create a type mismatch error with safe type name resolution.
    ///
    /// This helper method safely resolves type names even for anonymous structs
    /// by using the type pool. This prevents panics when rendering error messages
    /// for anonymous struct types that might not be fully registered yet.
    ///
    /// # Arguments
    /// - `expected`: The expected type
    /// - `found`: The actual type found
    /// - `span`: The source location of the mismatch
    ///
    /// # Returns
    /// A CompileError with properly formatted type names
    #[inline]
    pub(crate) fn type_mismatch_error(
        &self,
        expected: Type,
        found: Type,
        span: Span,
    ) -> CompileError {
        CompileError::new(
            ErrorKind::TypeMismatch {
                expected: expected.safe_name_with_pool(Some(&self.type_pool)),
                found: found.safe_name_with_pool(Some(&self.type_pool)),
            },
            span,
        )
    }

    /// Reject a `type`-valued declaration that would need to exist at runtime.
    ///
    /// A parameter of type `type` must be marked `comptime` (spec 4.14:5); a
    /// non-comptime `type` parameter would carry a type value at runtime, which
    /// spec 4.14:6 forbids. Both facets otherwise slip past sema and ICE in
    /// codegen ("block has no terminator", RUE-217), so we surface a clean
    /// compile-time diagnostic here. Comptime `type` parameters are erased
    /// during specialization and never reach runtime, so they are allowed.
    fn reject_runtime_type_value(
        &self,
        ty: Type,
        is_comptime: bool,
        span: Span,
    ) -> CompileResult<()> {
        if ty.is_comptime_type() && !is_comptime {
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: "a parameter of type `type` must be marked `comptime`".to_string(),
                },
                span,
            ));
        }
        Ok(())
    }
}

mod anon_methods;
mod builtin_ops;
mod calls;
mod functions;
mod instructions;
mod intrinsics;
mod ownership;
mod pointers;
mod type_inference;

#[cfg(test)]
mod error_invariant_tests {
    use super::*;
    use crate::inst::Air;
    use crate::intern_pool::TypeInternPool;

    fn output_with(func: AnalyzedFunction) -> SemaOutput {
        SemaOutput {
            functions: vec![func],
            strings: Vec::new(),
            warnings: Vec::new(),
            type_pool: TypeInternPool::new(),
        }
    }

    fn func_named(name: &str, air: Air) -> AnalyzedFunction {
        AnalyzedFunction {
            name: name.to_string(),
            air,
            num_locals: 0,
            num_param_slots: 0,
            param_modes: Vec::new(),
            allow_unreachable_code: false,
        }
    }

    /// A well-typed function must not trip the sema→CFG error invariant.
    #[test]
    fn no_error_type_is_clean() {
        let mut air = Air::new(Type::I32);
        air.add_inst(AirInst {
            data: AirInstData::Const(0),
            ty: Type::I32,
            span: Span::new(0, 0),
        });
        let output = output_with(func_named("main", air));
        assert!(find_undiagnosed_error_type(&output).is_none());
    }

    /// An `<error>`-typed instruction on the success path is a compiler bug and
    /// must be reported as an internal error (RUE-153).
    #[test]
    fn error_typed_instruction_is_caught() {
        let mut air = Air::new(Type::I32);
        air.add_inst(AirInst {
            data: AirInstData::UnitConst,
            ty: Type::ERROR,
            span: Span::new(0, 0),
        });
        let output = output_with(func_named("main", air));
        let err = find_undiagnosed_error_type(&output).expect("error type must be caught");
        assert!(matches!(err.kind, ErrorKind::InternalError(_)));
    }

    /// An `<error>` return type is likewise a bug and must be caught.
    #[test]
    fn error_return_type_is_caught() {
        let air = Air::new(Type::ERROR);
        let output = output_with(func_named("f", air));
        let err = find_undiagnosed_error_type(&output).expect("error return type must be caught");
        assert!(matches!(err.kind, ErrorKind::InternalError(_)));
    }
}
