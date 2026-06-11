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
//! compilation analyzes only reachable functions, per ADR-0026). A third,
//! parallel `SemaContext`-based pipeline and its hand-mirrored `_ctx`
//! function family were dead code and were removed per ADR-0033 phase 1.

use std::collections::{HashMap, HashSet};

use lasso::{Spur, ThreadedRodeo};
use rue_builtins::{BuiltinReturnType, BuiltinTypeDef};
use rue_error::{
    CompileError, CompileErrors, CompileResult, CompileWarning, ErrorKind,
    IntrinsicTypeMismatchError, MultiErrorResult, OptionExt, PreviewFeature, WarningKind,
};
use rue_rir::{InstData, InstRef, Rir, RirArgMode, RirCallArg, RirDirective, RirParamMode};
use rue_span::Span;
use rue_target::{Arch, Os};

use super::context::{
    AnalysisContext, AnalysisResult, BuiltinMethodContext, ConstValue, ParamInfo, ReceiverInfo,
    StringReceiverStorage,
};
use super::{AnalyzedFunction, InferenceContext, MethodInfo, Sema, SemaOutput};
use crate::inference::{
    Constraint, ConstraintContext, ConstraintGenerator, ParamVarInfo, Unifier, UnifyResult,
};
use crate::inst::{
    Air, AirArgMode, AirCallArg, AirInst, AirInstData, AirPlaceBase, AirProjection, AirRef,
};
use crate::types::{ModuleId, StructField, StructId, Type, TypeKind};

/// Main entry point for analyzing all function bodies.
///
/// Called from Sema::analyze_all after declarations are collected.
/// Currently uses the sequential analysis path while the parallel infrastructure
/// is being completed.
pub(crate) fn analyze_all_function_bodies(mut sema: Sema<'_>) -> MultiErrorResult<SemaOutput> {
    // Use lazy analysis when imports are present (multi-file compilation)
    // This ensures only reachable code is analyzed, per ADR-0026
    if sema.has_imports() {
        analyze_function_bodies_lazy(&mut sema)
    } else {
        // Use eager analysis for single-file compilation (backwards compatibility)
        analyze_all_function_bodies_sequential(&mut sema)
    }
}

/// Sequential analysis path (current implementation).
fn analyze_all_function_bodies_sequential(sema: &mut Sema<'_>) -> MultiErrorResult<SemaOutput> {
    // Build inference context once
    let infer_ctx = sema.build_inference_context();

    // Collect analyzed functions with their local strings.
    let mut functions_with_strings: Vec<(AnalyzedFunction, Vec<String>)> = Vec::new();
    let mut errors = CompileErrors::new();
    let mut all_warnings = Vec::new();

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

            // Skip FnDecls that are not in the functions table.
            // These are anonymous struct methods which are analyzed separately.
            if !sema.functions.contains_key(name) {
                continue;
            }

            // Skip generic functions - they'll be analyzed during specialization
            if let Some(fn_info) = sema.functions.get(name) {
                if fn_info.is_generic {
                    continue;
                }
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
                Ok((analyzed, warnings, local_strings, _ref_fns, _ref_meths)) => {
                    functions_with_strings.push((analyzed, local_strings));
                    all_warnings.extend(warnings);
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
                        Ok((analyzed, warnings, local_strings, _ref_fns, _ref_meths)) => {
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
            let struct_type = Type::new_struct(struct_id);
            let full_name = format!("{}.__drop", type_name_str);

            match sema.analyze_destructor_function(
                &infer_ctx,
                &full_name,
                *body,
                inst.span,
                struct_type,
            ) {
                Ok((analyzed, warnings, local_strings, _ref_fns, _ref_meths)) => {
                    functions_with_strings.push((analyzed, local_strings));
                    all_warnings.extend(warnings);
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

            let mut param_info: Vec<(Spur, Type, RirParamMode)> = Vec::new();

            if method_info.has_self {
                // Add self parameter (Normal mode - passed by value)
                let self_sym = sema.interner.get_or_intern("self");
                param_info.push((self_sym, method_info.struct_type, RirParamMode::Normal));
            }

            // Add regular parameters (convert from arena slices)
            for i in 0..param_names.len() {
                param_info.push((param_names[i], param_types[i], param_modes[i]));
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

            match sema.analyze_method_body(
                &infer_ctx,
                method_info.return_type,
                &param_info,
                method_info.body,
                method_info.struct_type,
                &captured_values,
            ) {
                Ok((
                    air,
                    num_locals,
                    num_param_slots,
                    param_modes_result,
                    warnings,
                    local_strings,
                    _ref_fns,
                    _ref_meths,
                )) => {
                    let analyzed = AnalyzedFunction {
                        name: full_name,
                        air,
                        num_locals,
                        num_param_slots,
                        param_modes: param_modes_result,
                    };
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

    let mut output = SemaOutput {
        functions,
        strings: global_strings,
        warnings: all_warnings,
        type_pool: sema.type_pool.clone(),
    };

    // Run specialization pass to rewrite CallGeneric instructions to Call
    // and create specialized function bodies
    if let Err(e) = crate::specialize::specialize(&mut output, sema, &infer_ctx, sema.interner) {
        errors.push(e);
    }

    errors.into_result_with(output)
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

            // Skip generic functions - they're analyzed during specialization
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
                    if *name == fn_name && !method_refs.contains(&inst_ref) {
                        found = true;
                        let params = sema.rir.get_params(*params_start, *params_len);

                        match sema.analyze_single_function(
                            &infer_ctx,
                            &fn_name_str,
                            *return_type,
                            &params,
                            *body,
                            inst.span,
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
                                for ref_fn in referenced_fns {
                                    if !analyzed_functions.contains(&ref_fn) {
                                        pending_functions.push(ref_fn);
                                    }
                                }
                                for ref_meth in referenced_meths {
                                    if !analyzed_methods.contains(&ref_meth) {
                                        pending_methods.push(ref_meth);
                                    }
                                }
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

                let mut param_info: Vec<(Spur, Type, RirParamMode)> = Vec::new();

                if method_info.has_self {
                    // Add self parameter (Normal mode - passed by value)
                    let self_sym = sema.interner.get_or_intern("self");
                    param_info.push((self_sym, method_info.struct_type, RirParamMode::Normal));
                }

                // Add regular parameters (convert from arena slices)
                for i in 0..param_names.len() {
                    param_info.push((param_names[i], param_types[i], param_modes[i]));
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

                match sema.analyze_method_body(
                    &infer_ctx,
                    method_info.return_type,
                    &param_info,
                    method_info.body,
                    method_info.struct_type,
                    &captured_values,
                ) {
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
                        };
                        functions_with_strings.push((analyzed, local_strings));
                        all_warnings.extend(warnings);

                        for ref_fn in referenced_fns {
                            if !analyzed_functions.contains(&ref_fn) {
                                pending_functions.push(ref_fn);
                            }
                        }
                        for ref_meth in referenced_meths {
                            if !analyzed_methods.contains(&ref_meth) {
                                pending_methods.push(ref_meth);
                            }
                        }
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

                                    for ref_fn in referenced_fns {
                                        if !analyzed_functions.contains(&ref_fn) {
                                            pending_functions.push(ref_fn);
                                        }
                                    }
                                    for ref_meth in referenced_meths {
                                        if !analyzed_methods.contains(&ref_meth) {
                                            pending_methods.push(ref_meth);
                                        }
                                    }
                                }
                                Err(e) => errors.push(e),
                            }
                        }
                    }
                }
            }
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

    let mut output = SemaOutput {
        functions,
        strings: global_strings,
        warnings: all_warnings,
        type_pool: sema.type_pool.clone(),
    };

    // Run specialization pass to rewrite CallGeneric instructions to Call
    // and create specialized function bodies
    if let Err(e) = crate::specialize::specialize(&mut output, sema, &infer_ctx, sema.interner) {
        errors.push(e);
    }

    errors.into_result_with(output)
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
/// (`field: Some(_)`) are not rejected here: they don't re-enter the
/// destructor (the drop-glue double drop of such a field is a separate,
/// pre-existing issue).
fn reject_self_move_in_destructor(air: &Air, full_name: &str) -> CompileResult<()> {
    for (_, inst) in air.iter() {
        if let AirInstData::MarkMoved {
            slot: 0,
            is_param: true,
            field: None,
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

/// Validate that a by-ref (`inout`/`borrow`) call argument is a plain variable
/// and return its symbol.
///
/// Codegen passes a by-ref argument by taking the address of the variable's
/// slot, so fields and array elements cannot be passed by reference yet
/// (RUE-127); reject them here rather than panicking during lowering.
fn require_plain_byref_arg(rir: &Rir, arg: &RirCallArg) -> CompileResult<Spur> {
    let inst = rir.get(arg.value);
    match &inst.data {
        InstData::VarRef { name } | InstData::ParamRef { name, .. } => Ok(*name),
        InstData::FieldGet { .. } | InstData::IndexGet { .. } => Err(CompileError::new(
            ErrorKind::ByRefArgNotPlainVariable,
            inst.span,
        )
        .with_help("bind the value to a local variable first and pass that variable by reference")),
        _ => Err(CompileError::new(
            if arg.is_inout() {
                ErrorKind::InoutNonLvalue
            } else {
                ErrorKind::BorrowNonLvalue
            },
            inst.span,
        )),
    }
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
    let root_name = interner.resolve(&root_var);
    let path_str = if field_path.is_empty() {
        root_name.to_string()
    } else {
        let field_names: Vec<_> = field_path
            .iter()
            .map(|s| interner.resolve(s).to_string())
            .collect();
        format!("{}.{}", root_name, field_names.join("."))
    };
    CompileError::new(ErrorKind::UseAfterMove(path_str), span)
        .with_label("value moved here", moved_span)
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
fn root_variable_of(rir: &Rir, inst_ref: InstRef) -> Option<Spur> {
    let inst = rir.get(inst_ref);
    match &inst.data {
        InstData::VarRef { name } => Some(*name),
        InstData::ParamRef { name, .. } => Some(*name),
        InstData::FieldGet { base, .. } => root_variable_of(rir, *base),
        InstData::IndexGet { base, .. } => root_variable_of(rir, *base),
        _ => None,
    }
}

/// Check exclusivity rules for inout and borrow parameters in a call.
///
/// This is the shared implementation behind [`Sema::check_exclusive_access`].
/// It enforces three rules:
/// 1. Inout/borrow arguments must be lvalues (refer to a variable)
/// 2. Same variable cannot be passed to multiple inout parameters (prevents aliasing)
/// 3. Same variable cannot be passed to both inout and borrow (law of exclusivity)
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
/// module's file: `member_file_path` (the callee's source file) must equal
/// `module_file_path`, otherwise the call is rejected as an unknown member
/// of `module_name`.
///
/// Two operations deliberately stay per-pipeline at the call sites: argument
/// analysis (it recurses into the owning pipeline) and the exclusive-access
/// check (whose shared core is `check_exclusive_access_in`, RUE-141).
#[allow(clippy::too_many_arguments)]
fn check_module_member_call(
    rir: &Rir,
    module_name: &str,
    module_file_path: &str,
    member_file_path: Option<&str>,
    fn_name_str: &str,
    param_types: &[Type],
    param_modes: &[RirParamMode],
    args: &[RirCallArg],
    accessible: bool,
    span: Span,
) -> CompileResult<()> {
    // Check membership: the function must be defined in the module's file.
    // (Strict path equality, mirroring the struct/enum/const member-access
    // checks in analyze_module_type_member_access.)
    if member_file_path != Some(module_file_path) {
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

    fn analyze_single_function(
        &mut self,
        infer_ctx: &InferenceContext,
        fn_name: &str,
        return_type: Spur,
        params: &[rue_rir::RirParam],
        body: InstRef,
        span: Span,
    ) -> CompileResult<(
        AnalyzedFunction,
        Vec<CompileWarning>,
        Vec<String>,
        HashSet<Spur>,
        HashSet<(StructId, Spur)>,
    )> {
        let ret_type = self.resolve_type(return_type, span)?;

        // Resolve parameter types and modes
        let param_info: Vec<(Spur, Type, RirParamMode)> = params
            .iter()
            .map(|p| {
                let ty = self.resolve_type(p.ty, span)?;
                Ok((p.name, ty, p.mode))
            })
            .collect::<CompileResult<Vec<_>>>()?;

        let (
            air,
            num_locals,
            num_param_slots,
            param_modes,
            warnings,
            local_strings,
            ref_fns,
            ref_meths,
        ) = self.analyze_function(infer_ctx, ret_type, &param_info, body)?;

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
            ref_fns,
            ref_meths,
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
    ) -> CompileResult<(
        AnalyzedFunction,
        Vec<CompileWarning>,
        Vec<String>,
        HashSet<Spur>,
        HashSet<(StructId, Spur)>,
    )> {
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

        let (
            air,
            num_locals,
            num_param_slots,
            param_modes,
            warnings,
            local_strings,
            ref_fns,
            ref_meths,
        ) = self.analyze_function(infer_ctx, ret_type, &param_info, body)?;

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
            ref_fns,
            ref_meths,
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
    ) -> CompileResult<(
        AnalyzedFunction,
        Vec<CompileWarning>,
        Vec<String>,
        HashSet<Spur>,
        HashSet<(StructId, Spur)>,
    )> {
        // Destructors take self parameter and return unit
        let self_sym = self.interner.get_or_intern("self");
        let param_info: Vec<(Spur, Type, RirParamMode)> =
            vec![(self_sym, struct_type, RirParamMode::Normal)];

        let (
            mut air,
            num_locals,
            num_param_slots,
            param_modes,
            warnings,
            local_strings,
            ref_fns,
            ref_meths,
        ) = self.analyze_function(infer_ctx, Type::UNIT, &param_info, body)?;

        reject_self_move_in_destructor(&air, full_name)?;

        // The destructor consumes `self`; the drop glue (not the destructor
        // itself) drops the fields afterwards, so the destructor must not
        // re-drop its own parameter — that would recurse forever.
        air.clear_param_drops();

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
            ref_fns,
            ref_meths,
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
    ) -> CompileResult<(
        Air,
        u32,
        u32,
        Vec<bool>,
        Vec<CompileWarning>,
        Vec<String>,
        HashSet<Spur>,
        HashSet<(StructId, Spur)>,
    )> {
        self.analyze_function_internal(infer_ctx, return_type, params, body, None, None)
    }

    /// Internal function analysis with optional type substitutions.
    ///
    /// When `type_subst` is provided (for specialized generic functions), it populates
    /// `comptime_type_vars` so that type parameters can be resolved in struct initialization
    /// (e.g., `P { x: 1, y: 2 }` where `P` is a type parameter).
    fn analyze_function_internal(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode)],
        body: InstRef,
        type_subst: Option<&std::collections::HashMap<Spur, Type>>,
        value_subst: Option<&std::collections::HashMap<Spur, ConstValue>>,
    ) -> CompileResult<(
        Air,
        u32,
        u32,
        Vec<bool>,
        Vec<CompileWarning>,
        Vec<String>,
        HashSet<Spur>,
        HashSet<(StructId, Spur)>,
    )> {
        let mut air = Air::new(return_type);
        let mut param_vec: Vec<ParamInfo> = Vec::new();
        let mut param_modes: Vec<bool> = Vec::new();

        // Add parameters to the param vec, tracking ABI slot offsets.
        // Each parameter starts at the next available ABI slot.
        // For struct parameters, the slot count is the number of fields.
        let mut next_abi_slot: u32 = 0;
        for (pname, ptype, mode) in params.iter() {
            param_vec.push(ParamInfo {
                name: *pname,
                abi_slot: next_abi_slot,
                ty: *ptype,
                mode: *mode,
            });
            // Inout and Borrow parameters are passed by reference.
            // Comptime parameters are VALUE params (like `comptime n: i32`), passed by value.
            // Normal parameters are passed by value.
            let is_by_ref = *mode == RirParamMode::Inout || *mode == RirParamMode::Borrow;
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

        // The callee owns its pass-by-value (Normal) parameters and must drop
        // them at exit unless they are moved out (RUE-61). Inout/borrow params
        // stay owned by the caller; comptime params are substituted away.
        // Destructors clear this list after analysis (see the destructor path).
        air.set_param_drops(
            param_vec
                .iter()
                .filter(|p| p.mode == RirParamMode::Normal)
                .map(|p| (p.abi_slot, p.ty))
                .collect(),
        );

        // ======================================================================
        // Phase 1-2: Hindley-Milner Type Inference
        // ======================================================================
        // Run constraint generation and unification to determine types
        // for all expressions BEFORE emitting AIR.
        let resolved_types = self.run_type_inference(
            infer_ctx,
            return_type,
            params,
            body,
            type_subst,
            value_subst,
        )?;

        // Create analysis context with resolved types
        // If type_subst is provided, initialize comptime_type_vars with the substitutions
        // so that type parameters can be resolved during struct initialization.
        let comptime_type_vars = type_subst.map(|s| s.clone()).unwrap_or_else(HashMap::new);
        let comptime_value_vars = value_subst.map(|s| s.clone()).unwrap_or_else(HashMap::new);
        let mut ctx = AnalysisContext {
            locals: HashMap::new(),
            params: &param_vec,
            next_slot: 0,
            loop_depth: 0,
            loop_break_stack: Vec::new(),
            used_locals: HashSet::new(),
            return_type,
            scope_stack: Vec::new(),
            resolved_types: &resolved_types,
            moved_vars: HashMap::new(),
            warnings: Vec::new(),
            local_string_table: HashMap::new(),
            local_strings: Vec::new(),
            comptime_type_vars,
            comptime_value_vars,
            referenced_functions: HashSet::new(),
            referenced_methods: HashSet::new(),
            byref_arg_root: None,
            in_loop_move_recheck: false,
        };

        // ======================================================================
        // Phase 3: AIR Emission
        // ======================================================================
        // Analyze the body expression, emitting AIR with resolved types
        let body_result = self.analyze_inst(&mut air, body, &mut ctx)?;

        // Add implicit return only if body doesn't already diverge (e.g., explicit return)
        if body_result.ty != Type::NEVER {
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
            ctx.referenced_functions,
            ctx.referenced_methods,
        ))
    }

    /// Analyze a specialized function body.
    ///
    /// This is similar to `analyze_function` but for generic function specialization.
    /// The `type_subst` map provides substitutions for type parameters to their
    /// concrete types.
    ///
    /// For example, when specializing `fn identity<T>(x: T) -> T { x }` with `T = i32`,
    /// the `params` will be `[(x, i32, Normal)]` and `return_type` will be `i32`.
    pub fn analyze_specialized_function(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode)],
        body: InstRef,
        type_subst: &std::collections::HashMap<Spur, Type>,
    ) -> CompileResult<(
        Air,
        u32,
        u32,
        Vec<bool>,
        Vec<CompileWarning>,
        Vec<String>,
        HashSet<Spur>,
        HashSet<(StructId, Spur)>,
    )> {
        // For specialized functions, we need to populate comptime_type_vars with the
        // type substitutions so that references to type parameters (like `P { ... }`)
        // can be resolved in the function body.
        self.analyze_function_internal(infer_ctx, return_type, params, body, Some(type_subst), None)
    }

    /// Analyze a method body with `Self` type resolution.
    ///
    /// This is used for anonymous struct methods where `Self` should resolve to the
    /// struct type. The `self_type` is added to the type substitution map under the
    /// symbol "Self", allowing `Self { ... }` struct literals to work correctly.
    fn analyze_method_body(
        &mut self,
        infer_ctx: &InferenceContext,
        return_type: Type,
        params: &[(Spur, Type, RirParamMode)],
        body: InstRef,
        self_type: Type,
        captured_comptime_values: &std::collections::HashMap<Spur, ConstValue>,
    ) -> CompileResult<(
        Air,
        u32,
        u32,
        Vec<bool>,
        Vec<CompileWarning>,
        Vec<String>,
        HashSet<Spur>,
        HashSet<(StructId, Spur)>,
    )> {
        // Create a type substitution map with Self -> the struct type
        let self_sym = self.interner.get_or_intern("Self");
        let mut type_subst = HashMap::new();
        type_subst.insert(self_sym, self_type);

        self.analyze_function_internal(
            infer_ctx,
            return_type,
            params,
            body,
            Some(&type_subst),
            Some(captured_comptime_values),
        )
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
        type_subst: Option<&HashMap<Spur, Type>>,
        value_subst: Option<&HashMap<Spur, ConstValue>>,
    ) -> CompileResult<HashMap<InstRef, Type>> {
        // Create constraint generator using pre-computed inference context
        let mut cgen = ConstraintGenerator::with_type_subst(
            self.rir,
            self.interner,
            &infer_ctx.func_sigs,
            &infer_ctx.struct_types,
            &infer_ctx.enum_types,
            &infer_ctx.method_sigs,
            &self.type_pool,
            type_subst,
        )
        .with_const_types(&infer_ctx.const_types);

        // Build parameter map for constraint context.
        // Convert Type to InferType so arrays are represented structurally.
        let mut param_vars: HashMap<Spur, ParamVarInfo> = params
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

        // Add comptime value variables as if they were parameters
        // This allows constraint generation to see captured comptime values
        if let Some(values) = value_subst {
            for (name, const_val) in values {
                let ty = match const_val {
                    ConstValue::Integer(_) => Type::I32, // TODO: Track actual type
                    ConstValue::Bool(_) => Type::BOOL,
                    ConstValue::Type(t) => *t,
                    ConstValue::Unit => Type::UNIT,
                };
                param_vars.insert(
                    *name,
                    ParamVarInfo {
                        ty: self.type_to_infer_type(ty),
                    },
                );
            }
        }

        // Create constraint context
        let mut cgen_ctx = ConstraintContext::new(&param_vars, return_type);

        // Phase 1: Generate constraints
        let body_info = cgen.generate(body, &mut cgen_ctx);

        // The function body's type must match the return type.
        // This handles implicit returns like `fn foo() -> i8 { 42 }`.
        // For arrays, we need to convert Type to InferType structurally.
        cgen.add_constraint(Constraint::equal(
            body_info.ty,
            self.type_to_infer_type(return_type),
            body_info.span,
        ));

        // Consume the constraint generator to release borrows
        let (constraints, int_literal_vars, expr_types, type_var_count) = cgen.into_parts();

        // Phase 2: Solve constraints via unification
        // Pre-size the substitution for better performance on large functions
        let mut unifier = Unifier::with_capacity(type_var_count);
        unifier.mark_int_literal_vars(&int_literal_vars);
        let errors = unifier.solve_constraints(&constraints);

        // Convert unification errors to compile errors
        // For now, we collect the first error. In the future, we could
        // report multiple errors for better diagnostics.
        if let Some(err) = errors.first() {
            // Map each UnifyResult variant to the appropriate ErrorKind
            let error_kind = match &err.kind {
                UnifyResult::Ok => unreachable!("UnificationError should never contain Ok"),
                UnifyResult::TypeMismatch { expected, found } => ErrorKind::TypeMismatch {
                    expected: expected.name_with_pool(&self.type_pool),
                    found: found.name_with_pool(&self.type_pool),
                },
                UnifyResult::IntLiteralNonInteger { found } => ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: found.safe_name_with_pool(Some(&self.type_pool)),
                },
                UnifyResult::OccursCheck { var, ty } => ErrorKind::TypeMismatch {
                    expected: "non-recursive type".to_string(),
                    found: format!("{var} = {ty} (infinite type)"),
                },
                UnifyResult::NotSigned { ty } => {
                    ErrorKind::CannotNegateUnsigned(ty.safe_name_with_pool(Some(&self.type_pool)))
                }
                UnifyResult::NotInteger { ty } => ErrorKind::TypeMismatch {
                    expected: "integer type".to_string(),
                    found: ty.safe_name_with_pool(Some(&self.type_pool)),
                },
                UnifyResult::NotUnsigned { ty } => ErrorKind::TypeMismatch {
                    expected: "unsigned integer type".to_string(),
                    found: ty.safe_name_with_pool(Some(&self.type_pool)),
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
            if let Some(param_info) = ctx.params.iter().find(|p| p.name == *name) {
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

            let struct_id = match base_type.kind() {
                TypeKind::Struct(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::FieldAccessOnNonStruct {
                            found: base_type.safe_name_with_pool(Some(&self.type_pool)),
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

            let array_type_id = match base_type.kind() {
                TypeKind::Array(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::IndexOnNonArray {
                            found: base_type.safe_name_with_pool(Some(&self.type_pool)),
                        },
                        inst.span,
                    ));
                }
            };

            let (element_type, length) = self.type_pool.array_def(array_type_id);

            // Index must be an unsigned integer
            let index_result = self.analyze_inst(air, *index, ctx)?;
            if !index_result.ty.is_unsigned() && !index_result.ty.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "unsigned integer type".to_string(),
                        found: index_result.ty.safe_name_with_pool(Some(&self.type_pool)),
                    },
                    self.rir.get(*index).span,
                ));
            }

            let array_length = length;

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
    /// - **Declarations**: DropFnDecl, FnDecl
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
            | InstData::Break { .. }
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
            InstData::DropFnDecl { .. } | InstData::FnDecl { .. } | InstData::ConstDecl { .. } => {
                self.analyze_decl_noop(air, inst_ref, ctx)
            }

            // Comptime block expression
            InstData::Comptime { expr } => {
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
                                    ty: ty.safe_name_with_pool(Some(&self.type_pool)),
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
                        let ty = Type::BOOL;
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::BoolConst(value),
                            ty,
                            span: inst.span,
                        });
                        Ok(AnalysisResult::new(air_ref, ty))
                    }
                    Some(ConstValue::Type(_type_val)) => {
                        // Type values can only exist at comptime - they cannot be returned
                        // from a comptime block since they can't exist at runtime.
                        Err(CompileError::new(
                            ErrorKind::ComptimeEvaluationFailed {
                                reason: "type values cannot exist at runtime".to_string(),
                            },
                            inst.span,
                        ))
                    }
                    Some(ConstValue::Unit) => {
                        let ty = Type::UNIT;
                        let air_ref = air.add_inst(AirInst {
                            data: AirInstData::UnitConst,
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

            // Type constant: a type used as a value (e.g., `i32` in `identity(i32, 42)`)
            InstData::TypeConst { type_name } => {
                // Resolve the type name to a concrete type
                let ty = self.resolve_type(*type_name, inst.span)?;
                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(ty),
                    ty: Type::COMPTIME_TYPE,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE))
            }

            // Anonymous struct type: a struct type constructed at comptime
            // (e.g., `struct { first: T, second: T, fn get(self) -> T { ... } }` in a comptime function)
            InstData::AnonStructType {
                fields_start,
                fields_len,
                methods_start,
                methods_len,
            } => {
                // Get the field declarations from the RIR
                let field_decls = self.rir.get_field_decls(*fields_start, *fields_len);

                // Empty structs are not allowed (unless they have methods)
                if field_decls.is_empty() && *methods_len == 0 {
                    return Err(CompileError::new(ErrorKind::EmptyStruct, inst.span));
                }

                // Resolve each field type and build the struct fields
                let mut struct_fields = Vec::with_capacity(field_decls.len());
                for (name_sym, type_sym) in field_decls {
                    let name_str = self.interner.resolve(&name_sym).to_string();
                    let field_ty = self.resolve_type(type_sym, inst.span)?;
                    struct_fields.push(StructField {
                        name: name_str,
                        ty: field_ty,
                    });
                }

                // Extract method signatures for structural equality comparison
                // (uses type symbols, not resolved Types, so Self matches Self)
                let method_sigs = self.extract_anon_method_sigs(*methods_start, *methods_len);

                // Check if an equivalent anonymous struct already exists (structural equality)
                // This now compares fields, method signatures, AND captured comptime values
                let (struct_ty, _is_new) =
                    self.find_or_create_anon_struct(&struct_fields, &method_sigs, &HashMap::new());

                // DON'T register methods here - they should be registered during const evaluation
                // (either try_evaluate_const for non-comptime, or try_evaluate_const_with_subst for comptime).
                // If we register here, we create a struct without captured comptime values, which is incorrect.
                //
                // if is_new && *methods_len > 0 {
                //     let struct_id = struct_ty
                //         .as_struct()
                //         .expect("anon struct should have StructId");
                //     self.register_anon_struct_methods(
                //         struct_id,
                //         struct_ty,
                //         *methods_start,
                //         *methods_len,
                //         inst.span,
                //     )?;
                // }

                let air_ref = air.add_inst(AirInst {
                    data: AirInstData::TypeConst(struct_ty),
                    ty: Type::COMPTIME_TYPE,
                    span: inst.span,
                });
                Ok(AnalysisResult::new(air_ref, Type::COMPTIME_TYPE))
            }

            // Checked block: evaluate the inner expression
            // The actual checking of unchecked operations happens in Phase 2
            InstData::Checked { expr } => self.analyze_inst(air, *expr, ctx),
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
        use crate::sema::analyze_ops::ProjectionInfo;

        // Try to trace the base to a place
        if let Some(mut trace) = self.try_trace_place(base, air, ctx)? {
            // Check if the root variable was fully moved
            if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
                if let Some(moved_span) = state.full_move {
                    let root_name = self.interner.resolve(&trace.root_var);
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(root_name.to_string()),
                        span,
                    )
                    .with_label("value moved here", moved_span));
                }
            }

            // Check mutability
            let root_name = self.interner.resolve(&trace.root_var).to_string();
            if !trace.is_root_mutable {
                // Check if this is a borrow parameter - special error message
                if trace.is_borrow_param {
                    return Err(CompileError::new(
                        ErrorKind::MutateBorrowedValue {
                            variable: root_name,
                        },
                        span,
                    ));
                }

                let root_type = trace.base_type;
                // Provide more specific error based on whether it's a param or local
                match trace.base {
                    AirPlaceBase::Param(_) => {
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(root_name.clone()),
                            span,
                        )
                        .with_help(format!(
                            "consider making parameter `{}` inout: `inout {}: {}`",
                            root_name,
                            root_name,
                            root_type.safe_name_with_pool(Some(&self.type_pool))
                        )));
                    }
                    AirPlaceBase::Local(_) => {
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(root_name),
                            span,
                        ));
                    }
                }
            }

            // Add the final field projection
            let base_type = trace.result_type();
            let struct_id = match base_type.as_struct() {
                Some(id) => id,
                None => {
                    return Err(CompileError::new(
                        ErrorKind::FieldAccessOnNonStruct {
                            found: base_type.safe_name_with_pool(Some(&self.type_pool)),
                        },
                        span,
                    ));
                }
            };

            let struct_def = self.type_pool.struct_def(struct_id);
            let field_name_str = self.interner.resolve(&field).to_string();

            let (field_index, struct_field) =
                struct_def.find_field(&field_name_str).ok_or_compile_error(
                    ErrorKind::UnknownField {
                        struct_name: struct_def.name.clone(),
                        field_name: field_name_str.clone(),
                    },
                    span,
                )?;

            let field_type = struct_field.ty;

            // Add the field projection to the trace
            trace.projections.push(ProjectionInfo {
                proj: AirProjection::Field {
                    struct_id,
                    field_index: field_index as u32,
                },
                result_type: field_type,
                field_name: Some(field),
            });

            // Analyze the value
            let value_result = self.analyze_inst(air, value, ctx)?;

            // The write reinitializes its destination: the assigned path
            // (and any moved sub-paths under it) is no longer moved, so
            // `o.f = ...` after `consume(o.f)` makes `o.f` usable again.
            // Index projections are skipped: `arr[i].f` records its moves
            // under the index-agnostic path `f` (see PlaceTrace::field_path),
            // so unmarking on a write to `arr[0].f` would wrongly forget a
            // move out of `arr[1].f`.
            if !trace
                .projections
                .iter()
                .any(|p| matches!(p.proj, AirProjection::Index { .. }))
            {
                let assigned_path = trace.field_path();
                if let Some(state) = ctx.moved_vars.get_mut(&trace.root_var) {
                    state.mark_path_reinitialized(&assigned_path);
                    if state.is_empty() {
                        ctx.moved_vars.remove(&trace.root_var);
                    }
                }
            }

            // Emit PlaceWrite instruction
            let place_ref = Self::build_place_ref(air, &trace);
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::PlaceWrite {
                    place: place_ref,
                    value: value_result.air_ref,
                },
                ty: Type::UNIT,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::UNIT));
        }

        // Fallback: base is not a place (e.g., function call result)
        // This shouldn't normally happen for valid assignment targets
        Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span))
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
        use crate::sema::analyze_ops::ProjectionInfo;

        // Try to trace the base to a place
        if let Some(mut trace) = self.try_trace_place(base, air, ctx)? {
            // Check if the root variable was fully moved
            if let Some(state) = ctx.moved_vars.get(&trace.root_var) {
                if let Some(moved_span) = state.full_move {
                    let root_name = self.interner.resolve(&trace.root_var);
                    return Err(CompileError::new(
                        ErrorKind::UseAfterMove(root_name.to_string()),
                        span,
                    )
                    .with_label("value moved here", moved_span));
                }
            }

            // Check mutability
            let root_name = self.interner.resolve(&trace.root_var).to_string();
            if !trace.is_root_mutable {
                // Check if this is a borrow parameter - special error message
                if trace.is_borrow_param {
                    return Err(CompileError::new(
                        ErrorKind::MutateBorrowedValue {
                            variable: root_name,
                        },
                        span,
                    ));
                }

                let root_type = trace.base_type;
                match trace.base {
                    AirPlaceBase::Param(_) => {
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(root_name.clone()),
                            span,
                        )
                        .with_help(format!(
                            "consider making parameter `{}` inout: `inout {}: {}`",
                            root_name,
                            root_name,
                            root_type.safe_name_with_pool(Some(&self.type_pool))
                        )));
                    }
                    AirPlaceBase::Local(_) => {
                        return Err(CompileError::new(
                            ErrorKind::AssignToImmutable(root_name),
                            span,
                        ));
                    }
                }
            }

            // Get array type info from the trace
            let base_type = trace.result_type();
            let (_array_type_id, elem_type, array_len) = match base_type.as_array() {
                Some(id) => {
                    let (elem, len) = self.type_pool.array_def(id);
                    (id, elem, len)
                }
                None => {
                    return Err(CompileError::new(
                        ErrorKind::IndexOnNonArray {
                            found: base_type.safe_name_with_pool(Some(&self.type_pool)),
                        },
                        span,
                    ));
                }
            };

            // Analyze index
            let index_result = self.analyze_inst(air, index, ctx)?;
            if !index_result.ty.is_unsigned() && !index_result.ty.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "unsigned integer type".to_string(),
                        found: index_result.ty.safe_name_with_pool(Some(&self.type_pool)),
                    },
                    self.rir.get(index).span,
                ));
            }

            // Compile-time bounds check for constant indices
            if let Some(const_index) = self.try_get_const_index(index) {
                if const_index < 0 || const_index as u64 >= array_len {
                    return Err(CompileError::new(
                        ErrorKind::IndexOutOfBounds {
                            index: const_index,
                            length: array_len,
                        },
                        self.rir.get(index).span,
                    ));
                }
            }

            // Add the index projection
            trace.projections.push(ProjectionInfo {
                proj: AirProjection::Index {
                    array_type: base_type,
                    index: index_result.air_ref,
                },
                result_type: elem_type,
                field_name: None,
            });

            // Analyze the value
            let value_result = self.analyze_inst(air, value, ctx)?;

            // Emit PlaceWrite instruction
            let place_ref = Self::build_place_ref(air, &trace);
            let air_ref = air.add_inst(AirInst {
                data: AirInstData::PlaceWrite {
                    place: place_ref,
                    value: value_result.air_ref,
                },
                ty: Type::UNIT,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::UNIT));
        }

        // Fallback: base is not a place
        Err(CompileError::new(ErrorKind::InvalidAssignmentTarget, span))
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

        // Handle module member access: module.function() becomes a direct function call
        if let Some(module_id) = receiver_type.as_module() {
            return self.analyze_module_member_call_impl(
                air, module_id, method, args_start, args_len, span, ctx,
            );
        }

        // Check that receiver is a struct type
        let struct_id = match receiver_type.kind() {
            TypeKind::Struct(id) => id,
            _ => {
                return Err(CompileError::new(
                    ErrorKind::MethodCallOnNonStruct {
                        found: receiver_type.safe_name_with_pool(Some(&self.type_pool)),
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

        // Look up the struct name by its ID (for error messages)
        let struct_def = self.type_pool.struct_def(struct_id);
        let struct_name_str = struct_def.name.clone();

        // Look up the method using StructId directly
        let method_key = (struct_id, method);
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

        // Check argument count (method_info.params excludes self)
        let method_param_types = self.param_arena.types(method_info.params);
        if args.len() != method_param_types.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: method_param_types.len(),
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

    /// Analyze a module member call: `module.function(args)` becomes a direct function call.
    ///
    /// In Phase 1 of the module system, modules are virtual namespaces. When you import
    /// a module with `@import("foo.rue")`, all of foo.rue's functions are already in the
    /// global function table (via multi-file compilation). The module provides a
    /// namespace at the source level; `check_module_member_call` enforces that only
    /// functions defined in the imported file resolve as members.
    #[allow(clippy::too_many_arguments)]
    fn analyze_module_member_call_impl(
        &mut self,
        air: &mut Air,
        module_id: ModuleId,
        function_name: Spur,
        args_start: u32,
        args_len: u32,
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Look up the function in the global function table. Not finding it
        // there means the module certainly has no such member.
        let fn_name_str = self.interner.resolve(&function_name).to_string();
        let module_def = self.module_registry.get_def(module_id);
        let fn_info = self
            .functions
            .get(&function_name)
            .ok_or_compile_error(
                ErrorKind::UnknownModuleMember {
                    module_name: module_def.import_path.clone(),
                    member_name: fn_name_str.clone(),
                },
                span,
            )?
            .clone();

        // Track this function as referenced (for lazy analysis)
        ctx.referenced_functions.insert(function_name);

        let param_types = self.param_arena.types(fn_info.params).to_vec();
        let param_modes = self.param_arena.modes(fn_info.params).to_vec();
        let args = self.rir.get_call_args(args_start, args_len);
        let accessible = self.is_accessible(span.file_id, fn_info.file_id, fn_info.is_pub);
        check_module_member_call(
            self.rir,
            &module_def.import_path,
            &module_def.file_path,
            self.get_file_path(fn_info.file_id),
            &fn_name_str,
            &param_types,
            &param_modes,
            &args,
            accessible,
            span,
        )?;

        // Check for exclusive access violation. (The old, pre-deduplication
        // copy of this function skipped this check entirely.)
        self.check_exclusive_access(&args, span)?;

        // Analyze arguments (the per-pipeline recursion seam)
        let air_args = self.analyze_call_args(air, &args, ctx)?;

        Ok(emit_module_member_call(
            air,
            function_name,
            &air_args,
            fn_info.return_type,
            span,
        ))
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
        // First check if it's a comptime type variable (e.g., `let P = Point(); P::origin()`)
        let struct_id = if let Some(&ty) = ctx.comptime_type_vars.get(&type_name) {
            // Extract struct ID from the comptime type
            match ty.kind() {
                TypeKind::Struct(id) => id,
                _ => {
                    return Err(CompileError::new(
                        ErrorKind::TypeMismatch {
                            expected: "struct type".to_string(),
                            found: ty.safe_name_with_pool(Some(&self.type_pool)),
                        },
                        span,
                    ));
                }
            }
        } else {
            *self
                .structs
                .get(&type_name)
                .ok_or_compile_error(ErrorKind::UnknownType(type_name_str.clone()), span)?
        };

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

        // Look up the function using StructId
        let method_key = (struct_id, function);
        let method_info = self.methods.get(&method_key).ok_or_compile_error(
            ErrorKind::UndefinedAssocFn {
                type_name: type_name_str.clone(),
                function_name: function_name_str.clone(),
            },
            span,
        )?;

        // Track this associated function/method as referenced (for lazy analysis)
        ctx.referenced_methods.insert(method_key);

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
        let method_param_types = self.param_arena.types(method_info.params);
        if args.len() != method_param_types.len() {
            return Err(CompileError::new(
                ErrorKind::WrongArgumentCount {
                    expected: method_param_types.len(),
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
        // Use the internal struct name (e.g., "__anon_struct_0") for anonymous structs,
        // not the user-visible type variable name (e.g., "P")
        let struct_def = self.type_pool.struct_def(struct_id);
        let internal_type_name = &struct_def.name;
        let call_name = format!("{}::{}", internal_type_name, function_name_str);
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
        } else if name == known.import {
            self.analyze_import_intrinsic(air, &args, span)
        } else if name == known.random_u32 {
            self.analyze_random_u32_intrinsic(air, name, &args, span)
        } else if name == known.random_u64 {
            self.analyze_random_u64_intrinsic(air, name, &args, span)
        } else if name == known.ptr_read {
            self.analyze_ptr_read_intrinsic(air, name, &args, span, ctx)
        } else if name == known.ptr_write {
            self.analyze_ptr_write_intrinsic(air, name, &args, span, ctx)
        } else if name == known.ptr_offset {
            self.analyze_ptr_offset_intrinsic(air, name, &args, span, ctx)
        } else if name == known.ptr_to_int {
            self.analyze_ptr_to_int_intrinsic(air, name, &args, span, ctx)
        } else if name == known.int_to_ptr {
            self.analyze_int_to_ptr_intrinsic(air, name, inst_ref, &args, span, ctx)
        } else if name == known.raw {
            self.analyze_addr_of_intrinsic(air, &args, span, ctx, false)
        } else if name == known.raw_mut {
            self.analyze_addr_of_intrinsic(air, &args, span, ctx, true)
        } else if name == known.syscall {
            self.analyze_syscall_intrinsic(air, name, &args, span, ctx)
        } else if name == known.target_arch {
            self.analyze_target_arch_intrinsic(air, &args, span)
        } else if name == known.target_os {
            self.analyze_target_os_intrinsic(air, &args, span)
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

        // Validate type: @dbg supports integers, bool, and String (spec 4.13:7).
        // Structs, enums, and arrays must be rejected HERE — codegen has no
        // lowering for them and would panic ("@dbg only supports scalars and
        // strings"), which the spec mandates as a compile error instead.
        if !arg_type.is_integer()
            && arg_type != Type::BOOL
            && !self.is_builtin_string(arg_type)
            && !arg_type.is_error()
            && !arg_type.is_never()
        {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "dbg".to_string(),
                    expected: "integer, bool, or String".to_string(),
                    found: arg_type.safe_name_with_pool(Some(&self.type_pool)),
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
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
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
                    found: source_type.safe_name_with_pool(Some(&self.type_pool)),
                })),
                span,
            ));
        }
        if !target_type.is_integer() && !target_type.is_error() && !target_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "cast".to_string(),
                    expected: "integer target type".to_string(),
                    found: target_type.safe_name_with_pool(Some(&self.type_pool)),
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
                ty: Type::NEVER,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::NEVER));
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
            ty: Type::NEVER,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::NEVER))
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
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
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
                    found: from_ty.safe_name_with_pool(Some(&self.type_pool)),
                })),
                span,
            ));
        }

        // Get the target type from HM inference
        let target_ty = match ctx.resolved_types.get(&inst_ref).copied() {
            Some(ty) if ty.is_integer() => ty,
            Some(Type::ERROR) => {
                // Error already reported during type inference
                return Err(CompileError::new(ErrorKind::TypeAnnotationRequired, span));
            }
            Some(ty) => {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: intrinsic_name.to_string(),
                        expected: "integer".to_string(),
                        found: ty.safe_name_with_pool(Some(&self.type_pool)),
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
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
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
                    found: arg_type.safe_name_with_pool(Some(&self.type_pool)),
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

    /// Analyze @random_u32 intrinsic.
    fn analyze_random_u32_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // @random_u32() - takes no arguments, returns u32
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "random_u32".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        // Create the intrinsic instruction that returns u32
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start: 0, // No args
                args_len: 0,
            },
            ty: Type::U32,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::U32))
    }

    /// Analyze @random_u64 intrinsic.
    fn analyze_random_u64_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // @random_u64() - takes no arguments, returns u64
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "random_u64".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        // Create the intrinsic instruction that returns u64
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start: 0, // No args
                args_len: 0,
            },
            ty: Type::U64,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::U64))
    }

    /// Analyze @import intrinsic.
    ///
    /// This requires the `modules` preview feature and takes a single string literal
    /// argument specifying the module path to import.
    fn analyze_import_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // @import takes exactly one argument
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "import".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        // Get the argument instruction - it must be a string literal
        let arg_inst = self.rir.get(args[0].value);
        let import_path = match &arg_inst.data {
            rue_rir::InstData::StringConst(path_spur) => {
                self.interner.resolve(path_spur).to_string()
            }
            _ => {
                return Err(CompileError::new(
                    ErrorKind::ImportRequiresStringLiteral,
                    arg_inst.span,
                ));
            }
        };

        // Resolve the import path relative to the current source file
        // Resolution order (per ADR-0026):
        // 1. foo.rue (simple file module)
        // 2. _foo.rue with foo/ directory (directory module)
        // 3. (Future) Dependency from rue.toml
        let resolved_path = self.resolve_import_path(&import_path, span)?;

        // Get or create the module in the registry
        // The module will be populated lazily when member access is performed
        let (module_id, _is_new) = self
            .module_registry
            .get_or_create(import_path.clone(), resolved_path);

        // Return a module type
        // AIR doesn't have a ModuleConst instruction, so we use UnitConst as a placeholder
        // The type is what matters for subsequent member access resolution
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::UnitConst, // Placeholder - module values are compile-time only
            ty: Type::new_module(module_id),
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::new_module(module_id)))
    }

    /// Resolve an import path to an absolute file path.
    ///
    /// Resolution order (per ADR-0026):
    /// 1. Standard library: `@import("std")` resolves to the bundled std library
    /// 2. Pre-loaded files (multi-file compilation)
    /// 3. `foo.rue` (simple file module)
    /// 4. `_foo.rue` with `foo/` directory (directory module)
    /// 5. (Future) Dependency from rue.toml
    pub(crate) fn resolve_import_path(
        &self,
        import_path: &str,
        span: Span,
    ) -> CompileResult<String> {
        use std::path::Path;

        // Phase 0: Check for standard library import
        // @import("std") resolves to the compiler's bundled standard library
        if import_path == "std" {
            return self.resolve_std_import(span);
        }

        // Phase 1: Check if the import path matches an already-loaded file
        // (unit tests, multi-file compilation, and files the driver's import
        // discovery loaded). Delegates to the structured ModulePath resolver
        // so loaded-path matching has exactly one implementation.
        let module_path = super::module_path::ModulePath::parse(import_path);
        if let Some((file_module, dir_module)) =
            module_path.find_ambiguity(self.file_paths.values())
        {
            return Err(CompileError::new(
                ErrorKind::AmbiguousModule(Box::new(rue_error::AmbiguousModuleData {
                    path: import_path.to_string(),
                    file_module,
                    dir_module,
                })),
                span,
            ));
        }
        if let Some(resolved) = module_path.resolve(self.file_paths.values()) {
            return Ok(resolved);
        }

        // Phase 2: Try to find the file on disk (for directory modules and actual file imports)
        // Get the directory of the current source file
        let source_path = self.get_source_path(span);
        let source_dir = source_path
            .and_then(|p| Path::new(p).parent())
            .unwrap_or(Path::new("."));

        let mut candidates = Vec::new();

        // Strip .rue extension if present for base name calculation
        let base_name = import_path.strip_suffix(".rue").unwrap_or(import_path);

        // Both module forms existing at once is ambiguous, not a precedence
        // question — mirror Rust's E0761 and refuse (RUE-137).
        let file_candidate = source_dir.join(format!("{}.rue", base_name));
        let facade_candidate = source_dir
            .join(base_name)
            .join(format!("_{}.rue", base_name));
        if !import_path.ends_with(".rue") && file_candidate.exists() && facade_candidate.exists() {
            return Err(CompileError::new(
                ErrorKind::AmbiguousModule(Box::new(rue_error::AmbiguousModuleData {
                    path: import_path.to_string(),
                    file_module: file_candidate.to_string_lossy().to_string(),
                    dir_module: facade_candidate.to_string_lossy().to_string(),
                })),
                span,
            ));
        }

        // Resolution order:
        // 1. Try foo.rue (simple file module)
        candidates.push(file_candidate.display().to_string());
        if file_candidate.exists() {
            return Ok(file_candidate.to_string_lossy().to_string());
        }

        // 2. If the path already ends in .rue, also try it directly
        if import_path.ends_with(".rue") {
            let candidate = source_dir.join(import_path);
            if !candidates.contains(&candidate.display().to_string()) {
                candidates.push(candidate.display().to_string());
            }
            if candidate.exists() {
                return Ok(candidate.to_string_lossy().to_string());
            }
        }

        // 3. Try the directory module: foo/ containing the facade foo/_foo.rue
        // (the facade lives INSIDE the directory, like std/_std.rue — RUE-137).
        candidates.push(facade_candidate.display().to_string());
        if facade_candidate.exists() {
            return Ok(facade_candidate.to_string_lossy().to_string());
        }

        // Module not found - report error with candidates tried
        Err(CompileError::new(
            ErrorKind::ModuleNotFound {
                path: import_path.to_string(),
                candidates,
            },
            span,
        ))
    }

    /// Resolve the standard library import.
    ///
    /// The standard library is located using the following resolution order:
    /// 1. `RUE_STD_PATH` environment variable (if set)
    /// 2. `std/` directory relative to the source file
    /// 3. Known installation paths
    ///
    /// Returns the path to `_std.rue`, the standard library root module.
    fn resolve_std_import(&self, span: Span) -> CompileResult<String> {
        use std::path::Path;

        // Check if we have a pre-loaded std library in file_paths
        for (_file_id, path) in &self.file_paths {
            // Check for _std.rue
            if path.ends_with("_std.rue") || path.ends_with("std/_std.rue") {
                return Ok(path.clone());
            }
        }

        // 1. Check RUE_STD_PATH environment variable
        if let Ok(std_path) = std::env::var("RUE_STD_PATH") {
            let std_root = Path::new(&std_path).join("_std.rue");
            if std_root.exists() {
                return Ok(std_root.to_string_lossy().to_string());
            }
        }

        // 2. Look for std/ relative to the source file
        if let Some(source_path) = self.get_source_path(span) {
            let source_dir = Path::new(source_path).parent().unwrap_or(Path::new("."));

            // Try std/_std.rue relative to source
            let std_root = source_dir.join("std").join("_std.rue");
            if std_root.exists() {
                return Ok(std_root.to_string_lossy().to_string());
            }
        }

        // Note: We intentionally do NOT check the current working directory
        // because it's unreliable and may find the wrong std library.
        // Users should either:
        // 1. Set RUE_STD_PATH environment variable
        // 2. Have std/ in the same directory as their source files
        // 3. Use aux_files in tests to provide std

        // Standard library not found
        Err(CompileError::new(ErrorKind::StdLibNotFound, span))
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
                    found: lhs_result.ty.safe_name_with_pool(Some(&self.type_pool)),
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
                ty: Type::BOOL,
                span,
            });
            return Ok(AnalysisResult::new(air_ref, Type::BOOL));
        }

        // Validate the type is appropriate for this comparison
        if allow_bool {
            // Equality operators (==, !=) work on integers, booleans, strings, unit, and structs
            // Note: String is now a struct, so is_struct() covers it
            if !lhs_type.is_integer()
                && lhs_type != Type::BOOL
                && lhs_type != Type::UNIT
                && !lhs_type.is_struct()
                && !self.is_builtin_string(lhs_type)
            {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: "integer, bool, string, unit, or struct".to_string(),
                        found: lhs_type.safe_name_with_pool(Some(&self.type_pool)),
                    },
                    self.rir.get(lhs).span,
                ));
            }
        } else if !lhs_type.is_integer() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: "integer".to_string(),
                    found: lhs_type.safe_name_with_pool(Some(&self.type_pool)),
                },
                self.rir.get(lhs).span,
            ));
        }

        let air_ref = air.add_inst(AirInst {
            data: make_data(lhs_result.air_ref, rhs_result.air_ref),
            ty: Type::BOOL,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::BOOL))
    }

    /// Try to evaluate an RIR expression as a compile-time constant.
    ///
    /// Returns `Some(value)` if the expression can be fully evaluated at compile time,
    /// or `None` if evaluation requires runtime information (e.g., variable values,
    /// function calls) or would cause overflow/panic.
    ///
    /// This is the foundation for compile-time bounds checking and can be extended
    /// for future `comptime` features.
    pub(crate) fn try_evaluate_const(&mut self, inst_ref: InstRef) -> Option<ConstValue> {
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
                    // Can't negate a boolean, type, or unit
                    ConstValue::Bool(_) | ConstValue::Type(_) | ConstValue::Unit => None,
                }
            }

            // Logical NOT: !expr
            InstData::Not { operand } => {
                match self.try_evaluate_const(*operand)? {
                    ConstValue::Bool(b) => Some(ConstValue::Bool(!b)),
                    // Can't logical-NOT an integer, type, or unit
                    ConstValue::Integer(_) | ConstValue::Type(_) | ConstValue::Unit => None,
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

            // Block: evaluate the result expression (last expression in the block)
            InstData::Block { extra_start, len } => {
                // A block is comptime-evaluable if it has a single instruction
                // (which is the result expression) OR if all statements are
                // side-effect-free and the result is comptime-evaluable.
                // For now, only handle the single-instruction case (common for
                // simple type-returning functions like `fn make_type() -> type { i32 }`).
                if *len == 1 {
                    let inst_refs = self.rir.get_extra(*extra_start, *len);
                    let result_ref = InstRef::from_raw(inst_refs[0]);
                    self.try_evaluate_const(result_ref)
                } else {
                    None // Blocks with multiple instructions need full interpreter support
                }
            }

            // Anonymous struct type: evaluate to a comptime type value
            InstData::AnonStructType {
                fields_start,
                fields_len,
                methods_start,
                methods_len,
            } => {
                // Get the field declarations from the RIR
                let field_decls = self.rir.get_field_decls(*fields_start, *fields_len);

                // Resolve each field type and build the struct fields
                let mut struct_fields = Vec::with_capacity(field_decls.len());
                for (name_sym, type_sym) in field_decls {
                    let name_str = self.interner.resolve(&name_sym).to_string();
                    // Try to resolve the type - for anonymous structs in comptime context,
                    // we need to be able to resolve the field types
                    let field_ty = self.resolve_type_for_comptime(type_sym)?;
                    struct_fields.push(StructField {
                        name: name_str,
                        ty: field_ty,
                    });
                }

                // Extract method signatures for structural equality comparison
                let method_sigs = self.extract_anon_method_sigs(*methods_start, *methods_len);

                // Find or create the anonymous struct type
                let (struct_ty, is_new) =
                    self.find_or_create_anon_struct(&struct_fields, &method_sigs, &HashMap::new());

                // Register methods if present and struct is new
                // This handles non-comptime functions like `fn Counter() -> type { struct { fn get() {} } }`
                // For comptime functions with captured values, use try_evaluate_const_with_subst instead
                if is_new && *methods_len > 0 {
                    let struct_id = struct_ty.as_struct()?;
                    // Use comptime-safe method registration (no type subst, no value subst for non-comptime)
                    self.register_anon_struct_methods_for_comptime_with_subst(
                        struct_id,
                        struct_ty,
                        *methods_start,
                        *methods_len,
                        inst.span,
                        &HashMap::new(), // Empty type substitution
                        &HashMap::new(), // Empty value substitution (non-comptime)
                    )?;
                }
                Some(ConstValue::Type(struct_ty))
            }

            // TypeConst: a type used as a value (e.g., `i32` in `identity(i32, 42)`)
            InstData::TypeConst { type_name } => {
                let type_name_str = self.interner.resolve(type_name);
                let ty = match type_name_str {
                    "i8" => Type::I8,
                    "i16" => Type::I16,
                    "i32" => Type::I32,
                    "i64" => Type::I64,
                    "u8" => Type::U8,
                    "u16" => Type::U16,
                    "u32" => Type::U32,
                    "u64" => Type::U64,
                    "bool" => Type::BOOL,
                    "()" => Type::UNIT,
                    "!" => Type::NEVER,
                    _ => {
                        // Check for struct types
                        if let Some(&struct_id) = self.structs.get(type_name) {
                            Type::new_struct(struct_id)
                        } else if let Some(&enum_id) = self.enums.get(type_name) {
                            Type::new_enum(enum_id)
                        } else {
                            return None; // Unknown type
                        }
                    }
                };
                Some(ConstValue::Type(ty))
            }

            // VarRef: when a variable reference is actually a type name (e.g., `Point` in `fn make_type() -> type { Point }`)
            InstData::VarRef { name } => {
                // Try to resolve as a type - if it's a type name, return the type
                let name_str = self.interner.resolve(name);
                let ty = match name_str {
                    "i8" => Type::I8,
                    "i16" => Type::I16,
                    "i32" => Type::I32,
                    "i64" => Type::I64,
                    "u8" => Type::U8,
                    "u16" => Type::U16,
                    "u32" => Type::U32,
                    "u64" => Type::U64,
                    "bool" => Type::BOOL,
                    "()" => Type::UNIT,
                    "!" => Type::NEVER,
                    _ => {
                        // Check for struct types
                        if let Some(&struct_id) = self.structs.get(name) {
                            Type::new_struct(struct_id)
                        } else if let Some(&enum_id) = self.enums.get(name) {
                            Type::new_enum(enum_id)
                        } else {
                            return None; // Not a type name - can't evaluate at compile time
                        }
                    }
                };
                Some(ConstValue::Type(ty))
            }

            // Everything else requires runtime evaluation
            _ => None,
        }
    }

    /// Try to extract a constant integer value from an RIR index expression.
    ///
    /// This is used for compile-time bounds checking. Returns `Some(value)` if
    /// the index can be evaluated to an integer constant at compile time.
    pub(crate) fn try_get_const_index(&mut self, inst_ref: InstRef) -> Option<i64> {
        self.try_evaluate_const(inst_ref)?.as_integer()
    }

    /// Try to evaluate an RIR instruction to a compile-time constant value with type substitution.
    ///
    /// This is used when evaluating generic functions that return `type`. For example,
    /// when calling `fn Pair(comptime T: type) -> type { struct { first: T, second: T } }`
    /// with `Pair(i32)`, we need to substitute `T -> i32` when evaluating the body.
    ///
    /// The `type_subst` map contains mappings from type parameter names to concrete types.
    pub(crate) fn try_evaluate_const_with_subst(
        &mut self,
        inst_ref: InstRef,
        type_subst: &std::collections::HashMap<Spur, Type>,
        value_subst: &std::collections::HashMap<Spur, ConstValue>,
    ) -> Option<ConstValue> {
        let inst = self.rir.get(inst_ref);
        match &inst.data {
            // Integer literals
            InstData::IntConst(value) => i64::try_from(*value).ok().map(ConstValue::Integer),

            // Boolean literals
            InstData::BoolConst(value) => Some(ConstValue::Bool(*value)),

            // Unary negation: -expr
            InstData::Neg { operand } => {
                match self.try_evaluate_const_with_subst(*operand, type_subst, value_subst)? {
                    ConstValue::Integer(n) => n.checked_neg().map(ConstValue::Integer),
                    ConstValue::Bool(_) | ConstValue::Type(_) | ConstValue::Unit => None,
                }
            }

            // Logical NOT: !expr
            InstData::Not { operand } => {
                match self.try_evaluate_const_with_subst(*operand, type_subst, value_subst)? {
                    ConstValue::Bool(b) => Some(ConstValue::Bool(!b)),
                    ConstValue::Integer(_) | ConstValue::Type(_) | ConstValue::Unit => None,
                }
            }

            // Binary arithmetic operations
            InstData::Add { lhs, rhs } => {
                let l = self
                    .try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?
                    .as_integer()?;
                let r = self
                    .try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?
                    .as_integer()?;
                l.checked_add(r).map(ConstValue::Integer)
            }
            InstData::Sub { lhs, rhs } => {
                let l = self
                    .try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?
                    .as_integer()?;
                let r = self
                    .try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?
                    .as_integer()?;
                l.checked_sub(r).map(ConstValue::Integer)
            }
            InstData::Mul { lhs, rhs } => {
                let l = self
                    .try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?
                    .as_integer()?;
                let r = self
                    .try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?
                    .as_integer()?;
                l.checked_mul(r).map(ConstValue::Integer)
            }
            InstData::Div { lhs, rhs } => {
                let l = self
                    .try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?
                    .as_integer()?;
                let r = self
                    .try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?
                    .as_integer()?;
                if r == 0 {
                    None
                } else {
                    l.checked_div(r).map(ConstValue::Integer)
                }
            }
            InstData::Mod { lhs, rhs } => {
                let l = self
                    .try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?
                    .as_integer()?;
                let r = self
                    .try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?
                    .as_integer()?;
                if r == 0 {
                    None
                } else {
                    l.checked_rem(r).map(ConstValue::Integer)
                }
            }

            // Comparison operations
            InstData::Eq { lhs, rhs } => {
                let l = self.try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?;
                let r = self.try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?;
                match (l, r) {
                    (ConstValue::Integer(a), ConstValue::Integer(b)) => {
                        Some(ConstValue::Bool(a == b))
                    }
                    (ConstValue::Bool(a), ConstValue::Bool(b)) => Some(ConstValue::Bool(a == b)),
                    _ => None,
                }
            }
            InstData::Ne { lhs, rhs } => {
                let l = self.try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?;
                let r = self.try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?;
                match (l, r) {
                    (ConstValue::Integer(a), ConstValue::Integer(b)) => {
                        Some(ConstValue::Bool(a != b))
                    }
                    (ConstValue::Bool(a), ConstValue::Bool(b)) => Some(ConstValue::Bool(a != b)),
                    _ => None,
                }
            }
            InstData::Lt { lhs, rhs } => {
                let l = self
                    .try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?
                    .as_integer()?;
                let r = self
                    .try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?
                    .as_integer()?;
                Some(ConstValue::Bool(l < r))
            }
            InstData::Gt { lhs, rhs } => {
                let l = self
                    .try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?
                    .as_integer()?;
                let r = self
                    .try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?
                    .as_integer()?;
                Some(ConstValue::Bool(l > r))
            }
            InstData::Le { lhs, rhs } => {
                let l = self
                    .try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?
                    .as_integer()?;
                let r = self
                    .try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?
                    .as_integer()?;
                Some(ConstValue::Bool(l <= r))
            }
            InstData::Ge { lhs, rhs } => {
                let l = self
                    .try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?
                    .as_integer()?;
                let r = self
                    .try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?
                    .as_integer()?;
                Some(ConstValue::Bool(l >= r))
            }

            // Logical operations
            InstData::And { lhs, rhs } => {
                let l = self
                    .try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?
                    .as_bool()?;
                let r = self
                    .try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?
                    .as_bool()?;
                Some(ConstValue::Bool(l && r))
            }
            InstData::Or { lhs, rhs } => {
                let l = self
                    .try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?
                    .as_bool()?;
                let r = self
                    .try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?
                    .as_bool()?;
                Some(ConstValue::Bool(l || r))
            }

            // Bitwise operations
            InstData::BitAnd { lhs, rhs } => {
                let l = self
                    .try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?
                    .as_integer()?;
                let r = self
                    .try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?
                    .as_integer()?;
                Some(ConstValue::Integer(l & r))
            }
            InstData::BitOr { lhs, rhs } => {
                let l = self
                    .try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?
                    .as_integer()?;
                let r = self
                    .try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?
                    .as_integer()?;
                Some(ConstValue::Integer(l | r))
            }
            InstData::BitXor { lhs, rhs } => {
                let l = self
                    .try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?
                    .as_integer()?;
                let r = self
                    .try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?
                    .as_integer()?;
                Some(ConstValue::Integer(l ^ r))
            }
            InstData::Shl { lhs, rhs } => {
                let l = self
                    .try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?
                    .as_integer()?;
                let r = self
                    .try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?
                    .as_integer()?;
                if r < 0 || r >= 8 {
                    return None;
                }
                Some(ConstValue::Integer(l << r))
            }
            InstData::Shr { lhs, rhs } => {
                let l = self
                    .try_evaluate_const_with_subst(*lhs, type_subst, value_subst)?
                    .as_integer()?;
                let r = self
                    .try_evaluate_const_with_subst(*rhs, type_subst, value_subst)?
                    .as_integer()?;
                if r < 0 || r >= 8 {
                    return None;
                }
                Some(ConstValue::Integer(l >> r))
            }
            InstData::BitNot { operand } => {
                let n = self
                    .try_evaluate_const_with_subst(*operand, type_subst, value_subst)?
                    .as_integer()?;
                Some(ConstValue::Integer(!n))
            }

            // Comptime block: comptime { expr } is compile-time evaluable if its inner expr is
            InstData::Comptime { expr } => {
                self.try_evaluate_const_with_subst(*expr, type_subst, value_subst)
            }

            // Block: evaluate the result expression (last expression in the block)
            InstData::Block { extra_start, len } => {
                if *len == 1 {
                    let inst_refs = self.rir.get_extra(*extra_start, *len);
                    let result_ref = InstRef::from_raw(inst_refs[0]);
                    self.try_evaluate_const_with_subst(result_ref, type_subst, value_subst)
                } else {
                    None
                }
            }

            // Anonymous struct type: evaluate to a comptime type value with substitution
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
                    // Use the substitution-aware type resolution
                    let field_ty =
                        self.resolve_type_for_comptime_with_subst(type_sym, type_subst)?;
                    struct_fields.push(StructField {
                        name: name_str,
                        ty: field_ty,
                    });
                }

                // Extract method signatures for structural equality comparison
                let method_sigs = self.extract_anon_method_sigs(*methods_start, *methods_len);

                let (struct_ty, _is_new) =
                    self.find_or_create_anon_struct(&struct_fields, &method_sigs, value_subst);

                // Register methods if present.
                // Register if either:
                // 1. This is a newly created struct (is_new=true), OR
                // 2. The struct exists but has no methods registered yet
                if *methods_len > 0 {
                    let struct_id = struct_ty.as_struct()?;

                    // Check if methods are already registered for this struct
                    let method_refs = self.rir.get_inst_refs(*methods_start, *methods_len);
                    let first_method_ref = method_refs[0];
                    let first_method_inst = self.rir.get(first_method_ref);
                    if let InstData::FnDecl {
                        name: method_name, ..
                    } = &first_method_inst.data
                    {
                        let needs_registration =
                            !self.methods.contains_key(&(struct_id, *method_name));

                        if needs_registration {
                            // Use comptime-safe method registration with type substitution
                            self.register_anon_struct_methods_for_comptime_with_subst(
                                struct_id,
                                struct_ty,
                                *methods_start,
                                *methods_len,
                                inst.span,
                                type_subst,
                                value_subst,
                            )?;
                        }
                    }
                }
                Some(ConstValue::Type(struct_ty))
            }

            // TypeConst: a type used as a value
            InstData::TypeConst { type_name } => {
                // First check the substitution map
                if let Some(&ty) = type_subst.get(type_name) {
                    return Some(ConstValue::Type(ty));
                }

                let type_name_str = self.interner.resolve(type_name);
                let ty = match type_name_str {
                    "i8" => Type::I8,
                    "i16" => Type::I16,
                    "i32" => Type::I32,
                    "i64" => Type::I64,
                    "u8" => Type::U8,
                    "u16" => Type::U16,
                    "u32" => Type::U32,
                    "u64" => Type::U64,
                    "bool" => Type::BOOL,
                    "()" => Type::UNIT,
                    "!" => Type::NEVER,
                    _ => {
                        if let Some(&struct_id) = self.structs.get(type_name) {
                            Type::new_struct(struct_id)
                        } else if let Some(&enum_id) = self.enums.get(type_name) {
                            Type::new_enum(enum_id)
                        } else {
                            return None;
                        }
                    }
                };
                Some(ConstValue::Type(ty))
            }

            // VarRef: check substitution maps first, then try as a type name
            InstData::VarRef { name } => {
                // Check if this is a type parameter in the type substitution map
                if let Some(&ty) = type_subst.get(name) {
                    return Some(ConstValue::Type(ty));
                }

                // Check if this is a comptime value variable in the value substitution map
                if let Some(value) = value_subst.get(name) {
                    return Some(value.clone());
                }

                // Try to resolve as a type name
                let name_str = self.interner.resolve(name);
                let ty = match name_str {
                    "i8" => Type::I8,
                    "i16" => Type::I16,
                    "i32" => Type::I32,
                    "i64" => Type::I64,
                    "u8" => Type::U8,
                    "u16" => Type::U16,
                    "u32" => Type::U32,
                    "u64" => Type::U64,
                    "bool" => Type::BOOL,
                    "()" => Type::UNIT,
                    "!" => Type::NEVER,
                    _ => {
                        if let Some(&struct_id) = self.structs.get(name) {
                            Type::new_struct(struct_id)
                        } else if let Some(&enum_id) = self.enums.get(name) {
                            Type::new_enum(enum_id)
                        } else {
                            return None;
                        }
                    }
                };
                Some(ConstValue::Type(ty))
            }

            // Everything else requires runtime evaluation
            _ => None,
        }
    }

    /// Check if an RIR instruction is a VarRef to a comptime type variable.
    ///
    /// This is used when validating comptime arguments to detect variables
    /// that hold comptime type values (e.g., `let P = Point(); ... Line(P)`).
    pub(crate) fn is_comptime_type_var(&self, inst_ref: InstRef, ctx: &AnalysisContext) -> bool {
        if let InstData::VarRef { name } = &self.rir.get(inst_ref).data {
            ctx.comptime_type_vars.contains_key(name)
        } else {
            false
        }
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
        use rue_builtins::BuiltinParamType;

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
                BuiltinParamType::Bool => Type::BOOL,
                BuiltinParamType::SelfType => Type::new_struct(struct_id),
            };

            // Type check
            if arg_result.ty != expected_ty && !arg_result.ty.is_error() {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: expected_ty.safe_name_with_pool(Some(&self.type_pool)),
                        found: arg_result.ty.safe_name_with_pool(Some(&self.type_pool)),
                    },
                    span,
                ));
            }

            air_args.push((arg_result.air_ref, AirArgMode::Normal));
        }

        // Determine return type
        // Use builtin_air_type for SelfType to get correct AIR output type
        let return_ty = match assoc_fn.return_ty {
            BuiltinReturnType::Unit => Type::UNIT,
            BuiltinReturnType::U64 => Type::U64,
            BuiltinReturnType::U8 => Type::U8,
            BuiltinReturnType::Bool => Type::BOOL,
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
        use rue_builtins::{BuiltinParamType, ReceiverMode};

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
            ReceiverMode::ByRef | ReceiverMode::ByMutRef => {
                // Borrow (ByRef) / mutation (ByMutRef) semantics - "unmove"
                // the variable since it's not consumed, and cancel the move
                // marker the receiver analysis emitted so drop elaboration
                // doesn't treat this borrow as a move.
                if let Some(var_symbol) = receiver.var {
                    ctx.moved_vars.remove(&var_symbol);
                }
                air.cancel_move_marker(receiver.result.air_ref);
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
                BuiltinParamType::Bool => Type::BOOL,
                BuiltinParamType::SelfType => Type::new_struct(method_ctx.struct_id),
            };

            // Type check
            if arg_result.ty != expected_ty
                && !arg_result.ty.is_error()
                && !(self.is_builtin_string(arg_result.ty)
                    && matches!(method.params[i].ty, BuiltinParamType::SelfType))
            {
                return Err(CompileError::new(
                    ErrorKind::TypeMismatch {
                        expected: expected_ty.safe_name_with_pool(Some(&self.type_pool)),
                        found: arg_result.ty.safe_name_with_pool(Some(&self.type_pool)),
                    },
                    method_ctx.span,
                ));
            }

            air_args.push((arg_result.air_ref, AirArgMode::Normal));
        }

        // Determine return type
        // Use builtin_air_type for SelfType to get correct AIR output type
        let return_ty = match method.return_ty {
            BuiltinReturnType::Unit => Type::UNIT,
            BuiltinReturnType::U64 => Type::U64,
            BuiltinReturnType::U8 => Type::U8,
            BuiltinReturnType::Bool => Type::BOOL,
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
                if let Some(param_info) = ctx.params.iter().find(|p| p.name == *name) {
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
                        RirParamMode::Normal | RirParamMode::Comptime => {
                            // Normal and comptime parameters are immutable
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
                ty: Type::UNIT,
                span,
            }),
            StringReceiverStorage::Param { abi_slot } => air.add_inst(AirInst {
                data: AirInstData::ParamStore {
                    param_slot: abi_slot,
                    value: call_ref,
                },
                ty: Type::UNIT,
                span,
            }),
        };

        Ok(AnalysisResult::new(store_ref, Type::UNIT))
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

            // Consumption must hold on EVERY path (must-consume).
            // `full_move` alone is may-move (union at branch joins): a value
            // consumed in only one branch of an if/match still leaks on the
            // other paths.
            let state = ctx.moved_vars.get(symbol);
            if !state.is_some_and(|s| s.full_move_on_all_paths) {
                let name = self.interner.resolve(&*symbol);
                return Err(linear_not_consumed_error(
                    name,
                    local.span,
                    state.and_then(|s| s.full_move),
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
        root_variable_of(self.rir, inst_ref)
    }

    /// Check exclusivity rules for inout and borrow parameters in a call
    /// (adapter over the shared [`check_exclusive_access_in`], RUE-141).
    pub(crate) fn check_exclusive_access(
        &self,
        args: &[RirCallArg],
        call_span: Span,
    ) -> CompileResult<()> {
        check_exclusive_access_in(self.rir, self.interner, args, call_span)
    }

    /// Analyze a list of call arguments, enforcing by-ref argument rules.
    ///
    /// Inout/borrow arguments are borrows, not moves: `ctx.byref_arg_root` is
    /// set while the argument value is analyzed so the variable-reference
    /// analysis skips move tracking (and permits forwarding an inout parameter
    /// to another function's by-ref parameter). By-ref arguments must also be
    /// plain variables: codegen takes the address of the variable's slot
    /// directly, and field/element projections are not supported yet (RUE-127).
    pub(crate) fn analyze_call_args(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        ctx: &mut AnalysisContext,
    ) -> CompileResult<Vec<AirCallArg>> {
        let mut air_args = Vec::new();
        for arg in args.iter() {
            let byref_root = if arg.is_inout() || arg.is_borrow() {
                Some(require_plain_byref_arg(self.rir, arg)?)
            } else {
                None
            };

            // Set while analyzing the argument so the use is treated as a
            // borrow, not a move; restored afterwards.
            let prev_byref_root = std::mem::replace(&mut ctx.byref_arg_root, byref_root);
            let arg_result = self.analyze_inst(air, arg.value, ctx);
            ctx.byref_arg_root = prev_byref_root;
            let arg_result = arg_result?;

            air_args.push(AirCallArg {
                value: arg_result.air_ref,
                mode: Self::convert_arg_mode(arg.mode),
            });
        }
        Ok(air_args)
    }

    /// Register methods from an anonymous struct type.
    ///
    /// This is called when an anonymous struct with methods is encountered during
    /// comptime evaluation. The methods are registered with the anonymous struct's
    /// StructId as the key, enabling method lookup via the standard method resolution
    /// mechanism.
    ///
    /// Note: Self type in method signatures is resolved to the anonymous struct's
    /// StructId during parameter type resolution.
    #[allow(dead_code)] // Currently unused; kept for reference. Methods are registered via _for_comptime variants.
    fn register_anon_struct_methods(
        &mut self,
        struct_id: StructId,
        struct_type: Type,
        methods_start: u32,
        methods_len: u32,
        _span: Span,
    ) -> CompileResult<()> {
        let method_refs = self.rir.get_inst_refs(methods_start, methods_len);

        for method_ref in method_refs {
            let method_inst = self.rir.get(method_ref);
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
                let key = (struct_id, *method_name);

                // Check for duplicate methods
                if self.methods.contains_key(&key) {
                    let struct_def = self.type_pool.struct_def(struct_id);
                    let method_name_str = self.interner.resolve(method_name).to_string();
                    return Err(CompileError::new(
                        ErrorKind::DuplicateMethod {
                            type_name: struct_def.name.clone(),
                            method_name: method_name_str,
                        },
                        method_inst.span,
                    ));
                }

                // Resolve parameter types (Self -> this anonymous struct's type)
                let params = self.rir.get_params(*params_start, *params_len);
                let param_names: Vec<Spur> = params.iter().map(|p| p.name).collect();
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| {
                        // Resolve type, with Self mapping to this struct
                        self.resolve_type_with_self(p.ty, struct_type, method_inst.span)
                    })
                    .collect::<CompileResult<Vec<_>>>()?;
                let ret_type =
                    self.resolve_type_with_self(*return_type, struct_type, method_inst.span)?;

                // Allocate method parameters in the arena
                let param_range = self
                    .param_arena
                    .alloc_method(param_names.into_iter(), param_types.into_iter());

                self.methods.insert(
                    key,
                    MethodInfo {
                        struct_type,
                        has_self: *has_self,
                        params: param_range,
                        return_type: ret_type,
                        body: *body,
                        span: method_inst.span,
                    },
                );
            }
        }
        Ok(())
    }

    /// Register methods from an anonymous struct type (comptime-safe version).
    ///
    /// This is the comptime-safe version of `register_anon_struct_methods`.
    /// It returns `Option<()>` instead of `CompileResult<()>`, allowing
    /// `try_evaluate_const` to gracefully fall back when method registration
    /// encounters issues that would be errors at compile time.
    ///
    /// Key differences from `register_anon_struct_methods`:
    /// - Uses `resolve_type_for_comptime` instead of `resolve_type`
    /// - Returns `None` on any failure instead of an error
    /// - Silently skips duplicate methods (returns None)
    #[allow(dead_code)] // Currently unused; methods registered via analyze_inst or _with_subst variant
    fn register_anon_struct_methods_for_comptime(
        &mut self,
        struct_id: StructId,
        struct_type: Type,
        methods_start: u32,
        methods_len: u32,
        _span: Span,
    ) -> Option<()> {
        let method_refs = self.rir.get_inst_refs(methods_start, methods_len);

        for method_ref in method_refs {
            let method_inst = self.rir.get(method_ref);
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
                let key = (struct_id, *method_name);

                // Check for duplicate methods - return None in comptime context
                if self.methods.contains_key(&key) {
                    return None;
                }

                // Resolve parameter types using comptime-safe resolution
                let params = self.rir.get_params(*params_start, *params_len);
                let param_names: Vec<Spur> = params.iter().map(|p| p.name).collect();
                let mut param_types: Vec<Type> = Vec::with_capacity(params.len());

                for p in params {
                    // Resolve type, with Self mapping to this struct
                    let type_str = self.interner.resolve(&p.ty);
                    let resolved_ty = if type_str == "Self" {
                        struct_type
                    } else {
                        self.resolve_type_for_comptime(p.ty)?
                    };
                    param_types.push(resolved_ty);
                }

                // Resolve return type
                let ret_type_str = self.interner.resolve(return_type);
                let ret_type = if ret_type_str == "Self" {
                    struct_type
                } else {
                    self.resolve_type_for_comptime(*return_type)?
                };

                // Allocate method parameters in the arena
                let param_range = self
                    .param_arena
                    .alloc_method(param_names.into_iter(), param_types.into_iter());

                self.methods.insert(
                    key,
                    MethodInfo {
                        struct_type,
                        has_self: *has_self,
                        params: param_range,
                        return_type: ret_type,
                        body: *body,
                        span: method_inst.span,
                    },
                );
            }
        }
        Some(())
    }

    /// Register methods from an anonymous struct type with type substitution (comptime-safe).
    ///
    /// This variant supports comptime parameter capture by using `resolve_type_for_comptime_with_subst`
    /// to resolve type parameters like `T` to their concrete types from the enclosing function's
    /// comptime arguments.
    ///
    /// For example, in:
    /// ```rue
    /// fn Wrapper(comptime T: type) -> type {
    ///     struct { value: T, fn get(self) -> T { self.value } }
    /// }
    /// ```
    /// When `Wrapper(i32)` is called, the type_subst map will contain `T -> i32`, so the
    /// method's return type `T` is resolved to `i32`.
    fn register_anon_struct_methods_for_comptime_with_subst(
        &mut self,
        struct_id: StructId,
        struct_type: Type,
        methods_start: u32,
        methods_len: u32,
        _span: Span,
        type_subst: &std::collections::HashMap<Spur, Type>,
        _value_subst: &std::collections::HashMap<Spur, ConstValue>,
    ) -> Option<()> {
        let method_refs = self.rir.get_inst_refs(methods_start, methods_len);

        // Track method names in this registration batch to detect duplicates
        let mut seen_methods: std::collections::HashSet<Spur> = std::collections::HashSet::new();

        for method_ref in method_refs {
            let method_inst = self.rir.get(method_ref);
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
                let key = (struct_id, *method_name);

                // Check for duplicate methods within this struct definition
                if seen_methods.contains(method_name) {
                    return None; // Duplicate method in same struct - evaluation fails
                }
                seen_methods.insert(*method_name);

                // Check if method was already registered from a previous call
                if self.methods.contains_key(&key) {
                    return None;
                }

                // Resolve parameter types using comptime-safe resolution with substitution
                let params = self.rir.get_params(*params_start, *params_len);
                let param_names: Vec<Spur> = params.iter().map(|p| p.name).collect();
                let mut param_types: Vec<Type> = Vec::with_capacity(params.len());

                for p in params {
                    // Resolve type, with Self mapping to this struct
                    let type_str = self.interner.resolve(&p.ty);
                    let resolved_ty = if type_str == "Self" {
                        struct_type
                    } else {
                        self.resolve_type_for_comptime_with_subst(p.ty, type_subst)?
                    };
                    param_types.push(resolved_ty);
                }

                // Resolve return type
                let ret_type_str = self.interner.resolve(return_type);
                let ret_type = if ret_type_str == "Self" {
                    struct_type
                } else {
                    self.resolve_type_for_comptime_with_subst(*return_type, type_subst)?
                };

                // Allocate method parameters in the arena
                let param_range = self
                    .param_arena
                    .alloc_method(param_names.into_iter(), param_types.into_iter());

                self.methods.insert(
                    key,
                    MethodInfo {
                        struct_type,
                        has_self: *has_self,
                        params: param_range,
                        return_type: ret_type,
                        body: *body,
                        span: method_inst.span,
                    },
                );
            }
        }
        Some(())
    }

    /// Resolve a type symbol, with special handling for Self.
    ///
    /// If the type symbol is "Self", it resolves to the provided self_type.
    /// Otherwise, it delegates to the standard resolve_type method.
    fn resolve_type_with_self(
        &mut self,
        type_sym: Spur,
        self_type: Type,
        span: Span,
    ) -> CompileResult<Type> {
        let type_str = self.interner.resolve(&type_sym);
        if type_str == "Self" {
            Ok(self_type)
        } else {
            self.resolve_type(type_sym, span)
        }
    }

    /// Extract method signatures from RIR for structural equality comparison.
    ///
    /// This extracts method signatures as type symbols (Spur), not resolved Types.
    /// This is intentional: for structural equality, we compare type symbols directly
    /// so that `Self` matches `Self` even before we know the concrete StructId.
    fn extract_anon_method_sigs(
        &self,
        methods_start: u32,
        methods_len: u32,
    ) -> Vec<super::AnonMethodSig> {
        let method_refs = self.rir.get_inst_refs(methods_start, methods_len);
        let mut sigs = Vec::with_capacity(method_refs.len());

        for method_ref in method_refs {
            let method_inst = self.rir.get(method_ref);
            if let InstData::FnDecl {
                name,
                params_start,
                params_len,
                return_type,
                has_self,
                ..
            } = &method_inst.data
            {
                // Extract parameter types as symbols (excluding self)
                let params = self.rir.get_params(*params_start, *params_len);
                let param_types: Vec<Spur> = params.iter().map(|p| p.ty).collect();

                sigs.push(super::AnonMethodSig {
                    name: *name,
                    has_self: *has_self,
                    param_types,
                    return_type: *return_type,
                });
            }
        }

        sigs
    }

    // ========================================================================
    // Pointer intrinsics (require unchecked context)
    // ========================================================================

    /// Analyze @ptr_read intrinsic: reads value through pointer.
    /// Signature: @ptr_read(ptr: ptr const T) -> T
    fn analyze_ptr_read_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "ptr_read".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        let ptr_result = self.analyze_inst(air, args[0].value, ctx)?;
        let ptr_type = ptr_result.ty;

        // Get the pointee type from the pointer type
        let pointee_type = match ptr_type.kind() {
            TypeKind::PtrConst(ptr_id) => self.type_pool.ptr_const_def(ptr_id),
            TypeKind::PtrMut(ptr_id) => self.type_pool.ptr_mut_def(ptr_id),
            _ => {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: "ptr_read".to_string(),
                        expected: "ptr const T or ptr mut T".to_string(),
                        found: self.format_type_name(ptr_type),
                    })),
                    span,
                ));
            }
        };

        // Create the intrinsic call instruction
        let args_start = air.add_extra(&[ptr_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 1,
            },
            ty: pointee_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, pointee_type))
    }

    /// Analyze @ptr_write intrinsic: writes value through pointer.
    /// Signature: @ptr_write(ptr: ptr mut T, value: T) -> ()
    fn analyze_ptr_write_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 2 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "ptr_write".to_string(),
                    expected: 2,
                    found: args.len(),
                },
                span,
            ));
        }

        let ptr_result = self.analyze_inst(air, args[0].value, ctx)?;
        let value_result = self.analyze_inst(air, args[1].value, ctx)?;
        let ptr_type = ptr_result.ty;
        let value_type = value_result.ty;

        // Pointer must be ptr mut T
        let pointee_type = match ptr_type.kind() {
            TypeKind::PtrMut(ptr_id) => self.type_pool.ptr_mut_def(ptr_id),
            TypeKind::PtrConst(_) => {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: "ptr_write".to_string(),
                        expected: "ptr mut T (cannot write through ptr const)".to_string(),
                        found: self.format_type_name(ptr_type),
                    })),
                    span,
                ));
            }
            _ => {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: "ptr_write".to_string(),
                        expected: "ptr mut T".to_string(),
                        found: self.format_type_name(ptr_type),
                    })),
                    span,
                ));
            }
        };

        // Check that value type matches pointee type
        if value_type != pointee_type && !value_type.is_error() && !value_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::TypeMismatch {
                    expected: self.format_type_name(pointee_type),
                    found: self.format_type_name(value_type),
                },
                span,
            ));
        }

        // Create the intrinsic call instruction
        let args_start =
            air.add_extra(&[ptr_result.air_ref.as_u32(), value_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 2,
            },
            ty: Type::UNIT,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::UNIT))
    }

    /// Analyze @ptr_offset intrinsic: pointer arithmetic.
    /// Signature: @ptr_offset(ptr: ptr T, offset: i64) -> ptr T
    fn analyze_ptr_offset_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 2 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "ptr_offset".to_string(),
                    expected: 2,
                    found: args.len(),
                },
                span,
            ));
        }

        let ptr_result = self.analyze_inst(air, args[0].value, ctx)?;
        let offset_result = self.analyze_inst(air, args[1].value, ctx)?;
        let ptr_type = ptr_result.ty;
        let offset_type = offset_result.ty;

        // Validate pointer type
        if !ptr_type.is_ptr() && !ptr_type.is_error() && !ptr_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "ptr_offset".to_string(),
                    expected: "ptr const T or ptr mut T".to_string(),
                    found: self.format_type_name(ptr_type),
                })),
                span,
            ));
        }

        // Validate offset type (must be integer)
        if !offset_type.is_integer() && !offset_type.is_error() && !offset_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "ptr_offset".to_string(),
                    expected: "integer offset".to_string(),
                    found: self.format_type_name(offset_type),
                })),
                span,
            ));
        }

        // Create the intrinsic call instruction (returns same pointer type)
        let args_start =
            air.add_extra(&[ptr_result.air_ref.as_u32(), offset_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 2,
            },
            ty: ptr_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, ptr_type))
    }

    /// Analyze @ptr_to_int intrinsic: converts pointer to u64.
    /// Signature: @ptr_to_int(ptr: ptr T) -> u64
    fn analyze_ptr_to_int_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "ptr_to_int".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        let ptr_result = self.analyze_inst(air, args[0].value, ctx)?;
        let ptr_type = ptr_result.ty;

        // Validate pointer type
        if !ptr_type.is_ptr() && !ptr_type.is_error() && !ptr_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "ptr_to_int".to_string(),
                    expected: "ptr const T or ptr mut T".to_string(),
                    found: self.format_type_name(ptr_type),
                })),
                span,
            ));
        }

        // Create the intrinsic call instruction (returns u64)
        let args_start = air.add_extra(&[ptr_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 1,
            },
            ty: Type::U64,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::U64))
    }

    /// Analyze @int_to_ptr intrinsic: converts u64 to pointer.
    /// Signature: @int_to_ptr(addr: u64) -> ptr mut T
    /// The result type T is inferred from context (e.g., `let p: ptr mut i32 = @int_to_ptr(addr)`)
    fn analyze_int_to_ptr_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        inst_ref: InstRef,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        if args.len() != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "int_to_ptr".to_string(),
                    expected: 1,
                    found: args.len(),
                },
                span,
            ));
        }

        let addr_result = self.analyze_inst(air, args[0].value, ctx)?;
        let addr_type = addr_result.ty;

        // Validate address type (must be u64)
        if addr_type != Type::U64 && !addr_type.is_error() && !addr_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "int_to_ptr".to_string(),
                    expected: "u64".to_string(),
                    found: self.format_type_name(addr_type),
                })),
                span,
            ));
        }

        // Get the result type from HM inference (must be a ptr mut T)
        let result_type = Self::get_resolved_type(ctx, inst_ref, span, "@int_to_ptr intrinsic")?;

        // Validate that the inferred type is a mutable pointer
        if !result_type.is_ptr_mut() && !result_type.is_error() && !result_type.is_never() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                    name: "int_to_ptr".to_string(),
                    expected: "ptr mut T".to_string(),
                    found: self.format_type_name(result_type),
                })),
                span,
            ));
        }

        // Create the intrinsic call instruction
        let args_start = air.add_extra(&[addr_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 1,
            },
            ty: result_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_type))
    }

    /// Analyze @addr_of / @addr_of_mut intrinsics: takes address of lvalue.
    /// Signature: @addr_of(lvalue) -> ptr const T
    /// Signature: @addr_of_mut(lvalue) -> ptr mut T
    fn analyze_addr_of_intrinsic(
        &mut self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
        is_mut: bool,
    ) -> CompileResult<AnalysisResult> {
        let intrinsic_name = if is_mut { "addr_of_mut" } else { "addr_of" };

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

        let arg_result = self.analyze_inst(air, args[0].value, ctx)?;
        let pointee_type = arg_result.ty;

        // For addr_of, we need the argument to be an lvalue (addressable)
        // This is validated at the RIR level - here we just compute the result type

        // Create the pointer type
        let result_type = if is_mut {
            let ptr_type_id = self.type_pool.intern_ptr_mut_from_type(pointee_type);
            Type::new_ptr_mut(ptr_type_id)
        } else {
            let ptr_type_id = self.type_pool.intern_ptr_const_from_type(pointee_type);
            Type::new_ptr_const(ptr_type_id)
        };

        // Create the intrinsic call instruction
        let name = if is_mut {
            self.known.raw_mut
        } else {
            self.known.raw
        };
        let args_start = air.add_extra(&[arg_result.air_ref.as_u32()]);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: 1,
            },
            ty: result_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_type))
    }

    /// Analyze @syscall intrinsic: perform a raw OS syscall.
    /// Signature: @syscall(syscall_num: u64, arg0?: u64, ..., arg5?: u64) -> i64
    ///
    /// Takes a syscall number and up to 6 arguments, all of which must be u64.
    /// Returns i64 (the syscall return value, which may be negative for errors).
    /// Requires a checked block.
    fn analyze_syscall_intrinsic(
        &mut self,
        air: &mut Air,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
        ctx: &mut AnalysisContext,
    ) -> CompileResult<AnalysisResult> {
        // Syscall takes 1-7 arguments: syscall number + up to 6 arguments
        if args.is_empty() || args.len() > 7 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "syscall".to_string(),
                    expected: 7, // Show max expected for "at least 1, at most 7"
                    found: args.len(),
                },
                span,
            ));
        }

        // Analyze all arguments and verify they are u64
        let mut arg_refs = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            let arg_result = self.analyze_inst(air, arg.value, ctx)?;
            let arg_type = arg_result.ty;

            // All syscall arguments must be u64
            if arg_type != Type::U64 && !arg_type.is_error() && !arg_type.is_never() {
                return Err(CompileError::new(
                    ErrorKind::IntrinsicTypeMismatch(Box::new(IntrinsicTypeMismatchError {
                        name: "syscall".to_string(),
                        expected: format!("u64 for argument {}", i),
                        found: self.format_type_name(arg_type),
                    })),
                    span,
                ));
            }

            arg_refs.push(arg_result.air_ref.as_u32());
        }

        // Create the intrinsic call instruction
        let args_start = air.add_extra(&arg_refs);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::Intrinsic {
                name,
                args_start,
                args_len: args.len() as u32,
            },
            ty: Type::I64,
            span,
        });
        Ok(AnalysisResult::new(air_ref, Type::I64))
    }

    /// Analyze @target_arch() intrinsic - returns target CPU architecture enum.
    ///
    /// This intrinsic takes no arguments and returns an Arch enum value
    /// representing the target CPU architecture (X86_64 or Aarch64).
    fn analyze_target_arch_intrinsic(
        &self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // Validate: no arguments
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "target_arch".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        let arch_enum_id = self
            .builtin_arch_id
            .expect("Arch enum not injected - internal compiler error");

        // Determine variant index based on host architecture (compile-time evaluation)
        // Currently we always compile for the host architecture
        let variant_index = match rue_target::Target::host().arch() {
            Arch::X86_64 => 0,
            Arch::Aarch64 => 1,
        };

        let result_type = Type::new_enum(arch_enum_id);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::EnumVariant {
                enum_id: arch_enum_id,
                variant_index,
            },
            ty: result_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_type))
    }

    /// Analyze @target_os() intrinsic - returns target operating system enum.
    ///
    /// This intrinsic takes no arguments and returns an Os enum value
    /// representing the target operating system (Linux or Macos).
    fn analyze_target_os_intrinsic(
        &self,
        air: &mut Air,
        args: &[RirCallArg],
        span: Span,
    ) -> CompileResult<AnalysisResult> {
        // Validate: no arguments
        if !args.is_empty() {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "target_os".to_string(),
                    expected: 0,
                    found: args.len(),
                },
                span,
            ));
        }

        let os_enum_id = self
            .builtin_os_id
            .expect("Os enum not injected - internal compiler error");

        // Determine variant index based on host OS (compile-time evaluation)
        // Currently we always compile for the host OS
        let variant_index = match rue_target::Target::host().os() {
            Os::Linux => 0,
            Os::Macos => 1,
        };

        let result_type = Type::new_enum(os_enum_id);
        let air_ref = air.add_inst(AirInst {
            data: AirInstData::EnumVariant {
                enum_id: os_enum_id,
                variant_index,
            },
            ty: result_type,
            span,
        });
        Ok(AnalysisResult::new(air_ref, result_type))
    }
}
