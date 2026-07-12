//! Function body analysis and AIR generation.
//!
//! This module contains the core semantic analysis functionality:
//! - Function analysis (analyze_single_function, analyze_method_function, analyze_destructor_function)
//! - Hindley-Milner type inference (run_type_inference)
//! - RIR to AIR instruction lowering (analyze_inst)
//! - Helper functions for expression analysis
//!
//! The demand-driven driver analyzes ordinary function and method bodies only
//! when they are reachable from `main`, per ADR-0045. Program shape does not
//! change which bodies are checked.

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
    AnalysisContext, AnalysisResult, BuiltinMethodContext, CallLoanKind, ConstValue, ParamInfo,
    ReceiverInfo, StringReceiverStorage,
};
use super::{AnalyzedFunction, InferenceContext, MethodInfo, ParamSlotModes, Sema, SemaOutput};
use crate::inference::{
    Constraint, ConstraintContext, ConstraintGenerator, InferType, ParamVarInfo, Unifier,
    UnifyResult,
};
use crate::inst::{
    Air, AirArgMode, AirCallArg, AirInst, AirInstData, AirPlaceBase, AirProjection, AirRef,
};
use crate::types::{
    ModuleId, StructField, StructId, Type, TypeKind, parse_array_type_syntax,
    parse_type_call_syntax,
};

/// Main entry point for analyzing all function bodies.
///
/// Called from Sema::analyze_all after declarations are collected.
/// Uses the demand-driven driver for every program shape.
pub(crate) fn analyze_all_function_bodies(mut sema: Sema<'_>) -> MultiErrorResult<SemaOutput> {
    // Declarations are complete: re-point destructor symbols of struct
    // names that span multiple files at their file-qualified form (RUE-571).
    // Must precede any body analysis — destructor definitions build their
    // symbol from the same helper.
    sema.requalify_colliding_destructor_symbols();

    // ADR-0045 defines reachability from `main` as the function-body analysis
    // frontier for every executable.
    let result = analyze_function_bodies_lazy(&mut sema);

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

/// Shared finalization for demand-driven function-body analysis.
fn finalize_function_body_analysis(
    sema: &mut Sema<'_>,
    functions_with_strings: Vec<(AnalyzedFunction, Vec<String>)>,
    mut all_warnings: Vec<CompileWarning>,
    unused_function_roots: &HashSet<Spur>,
    errors: CompileErrors,
) -> MultiErrorResult<SemaOutput> {
    // String IDs flow into AIR, assembly, rodata, and relocations. Assign them
    // by content rather than function-discovery order so worklist scheduling
    // cannot change any downstream representation (RUE-624).
    let mut global_strings: Vec<String> = functions_with_strings
        .iter()
        .flat_map(|(_, local_strings)| local_strings.iter().cloned())
        .collect();
    global_strings.sort();
    global_strings.dedup();
    let global_string_table: HashMap<String, u32> = global_strings
        .iter()
        .cloned()
        .enumerate()
        .map(|(id, string)| (string, id as u32))
        .collect();

    let mut functions: Vec<AnalyzedFunction> = Vec::new();
    for (mut analyzed, local_strings) in functions_with_strings {
        if !local_strings.is_empty() {
            let local_to_global: Vec<u32> = local_strings
                .into_iter()
                .map(|string| {
                    *global_string_table
                        .get(&string)
                        .expect("every local string was collected globally")
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

    let output = SemaOutput {
        functions,
        strings: global_strings,
        warnings: all_warnings,
        // Specialization can intern new composite types, so snapshot only
        // after its fixed point has completed.
        type_pool: sema.type_pool.clone(),
        body_analysis_work: sema.body_analysis_work,
        ordinary_free_function_dependencies: {
            sema.ordinary_free_function_dependencies.sort();
            sema.ordinary_free_function_dependencies.dedup();
            sema.ordinary_free_function_dependencies.clone()
        },
        ordinary_free_function_dependencies_complete: true,
        specialized_free_function_origins: {
            sema.specialized_free_function_origins.sort();
            sema.specialized_free_function_origins.dedup();
            sema.specialized_free_function_origins.clone()
        },
        specialized_free_function_dependencies: {
            sema.specialized_free_function_dependencies.sort();
            sema.specialized_free_function_dependencies.dedup();
            sema.specialized_free_function_dependencies.clone()
        },
        specialized_free_function_dependencies_complete: true,
        named_method_dependencies: {
            sema.named_method_dependencies.sort();
            sema.named_method_dependencies.dedup();
            sema.named_method_dependencies.clone()
        },
        non_generic_named_method_dependencies_complete: sema
            .non_generic_named_method_dependencies_complete,
        generic_named_method_dependencies_complete: false,
        named_destructor_dependencies: {
            sema.named_destructor_dependencies.sort();
            sema.named_destructor_dependencies.dedup();
            sema.named_destructor_dependencies.clone()
        },
        named_destructor_dependencies_complete: true,
        declaration_type_dependencies: {
            sema.declaration_type_dependencies.sort();
            sema.declaration_type_dependencies.dedup();
            sema.declaration_type_dependencies.clone()
        },
        declaration_type_dependencies_complete: false,
    };

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
        match &inst.data {
            InstData::Call { name, .. } => {
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

            // Type-position applications: a `-> type` constructor applied in
            // type-annotation syntax (`fn take(p: Pair(i32))`, `struct S { v:
            // Vec(i32) }`, `enum E { Full(Vec(i32)) }`, `let x: Wrap(i8)`) is
            // a reference too, but it is a type *symbol*, not a `Call` inst —
            // without scanning these, a constructor used only in type
            // positions warned "unused function" (RUE-608).
            InstData::FnDecl {
                params_start,
                params_len,
                return_type,
                ..
            } => {
                for param in sema.rir.get_params(*params_start, *params_len) {
                    mark_type_syntax_refs(sema, param.ty, inst.span.file_id, &mut referenced);
                }
                mark_type_syntax_refs(sema, *return_type, inst.span.file_id, &mut referenced);
            }
            InstData::StructDecl {
                fields_start,
                fields_len,
                ..
            } => {
                for (_, field_ty) in sema.rir.get_field_decls(*fields_start, *fields_len) {
                    mark_type_syntax_refs(sema, field_ty, inst.span.file_id, &mut referenced);
                }
            }
            InstData::EnumDecl {
                payloads_start,
                payloads_len,
                ..
            } => {
                // Self-describing payload region: `[k, t0, .., t_{k-1}]` per
                // variant (see `resolve_enum_payloads`).
                let words = sema.rir.get_extra(*payloads_start, *payloads_len);
                let mut i = 0usize;
                while i < words.len() {
                    let k = words[i] as usize;
                    i += 1;
                    for _ in 0..k {
                        if let Some(ty_sym) = Spur::try_from_usize(words[i] as usize) {
                            mark_type_syntax_refs(sema, ty_sym, inst.span.file_id, &mut referenced);
                        }
                        i += 1;
                    }
                }
            }
            InstData::Alloc { ty: Some(ty), .. } | InstData::ConstDecl { ty: Some(ty), .. } => {
                mark_type_syntax_refs(sema, *ty, inst.span.file_id, &mut referenced);
            }

            _ => {}
        }
    }

    referenced
}

/// Mark `-> type` constructors applied inside a type symbol's syntax
/// (`Pair(i32)`, `[Pair(i32); 3]`, `ptr const Pair(i32)`, nested
/// applications) as statically referenced (RUE-608). Resolution is
/// module-local, matching how the annotation itself resolves; unknown names
/// are skipped (the real type resolution diagnoses them). A dotted head
/// (`m.G(T)`) is a cross-module reference, which requires the constructor to
/// be `pub` — already exempt from the unused warning — so only unqualified
/// heads need marking.
fn mark_type_syntax_refs(
    sema: &Sema<'_>,
    ty_sym: Spur,
    file_id: FileId,
    referenced: &mut HashSet<Spur>,
) {
    fn walk(sema: &Sema<'_>, name: &str, file_id: FileId, referenced: &mut HashSet<Spur>) {
        if let Some((element, _len)) = parse_array_type_syntax(name) {
            walk(sema, &element, file_id, referenced);
            return;
        }
        if let Some(pointee) = name
            .strip_prefix("ptr const ")
            .or_else(|| name.strip_prefix("ptr mut "))
        {
            walk(sema, pointee, file_id, referenced);
            return;
        }
        let Some((call_name, arg_strs)) = parse_type_call_syntax(name) else {
            return;
        };
        // `Str(N)` is the built-in fixed string, not a user constructor.
        if call_name != "Str"
            && !call_name.contains('.')
            && let Some(name_sym) = sema.interner.get(&call_name)
            && let Some(function_key) = sema.resolve_function_name_local(name_sym, file_id)
        {
            referenced.insert(function_key);
        }
        for arg in &arg_strs {
            walk(sema, arg, file_id, referenced);
        }
    }
    walk(sema, sema.interner.resolve(&ty_sym), file_id, referenced);
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

fn named_method_dependency_events(
    sema: &Sema<'_>,
    caller_struct: StructId,
    caller_method: Spur,
    referenced_functions: &HashSet<Spur>,
    referenced_methods: &HashSet<(StructId, Spur)>,
) -> CompileResult<Vec<super::NamedMethodDependencyEvent>> {
    let caller_info = sema
        .methods
        .get(&(caller_struct, caller_method))
        .ok_or_else(|| {
            CompileError::new(
                ErrorKind::InvalidCompilerInput(
                    "named-method dependency caller is absent from semantic declarations".into(),
                ),
                rue_span::Span::default(),
            )
        })?;
    let caller_owner_name = sema.type_pool.struct_def(caller_struct).name.clone();
    let caller_method_name = sema.interner.resolve(&caller_method).to_string();
    let mut events = Vec::new();
    for callee in referenced_functions {
        let info = sema.functions.get(callee).ok_or_else(|| {
            CompileError::new(
                ErrorKind::InvalidCompilerInput(
                    "named method references a free function absent from semantic declarations"
                        .into(),
                ),
                caller_info.span,
            )
        })?;
        events.push(super::NamedMethodDependencyEvent {
            caller_file: caller_info.span.file_id.index(),
            caller_owner_name: caller_owner_name.clone(),
            caller_method_name: caller_method_name.clone(),
            target: super::NamedMethodDependencyTargetEvent::FreeFunction {
                file: info.file_id.index(),
                name: sema
                    .interner
                    .resolve(&sema.source_function_name(*callee))
                    .to_string(),
            },
        });
    }
    for (callee_struct, callee_method) in referenced_methods {
        let owner_name = sema.type_pool.struct_def(*callee_struct).name.clone();
        if owner_name.starts_with("__anon_struct_") {
            continue;
        }
        let info = sema
            .methods
            .get(&(*callee_struct, *callee_method))
            .ok_or_else(|| {
                CompileError::new(
                    ErrorKind::InvalidCompilerInput(
                        "named method references a named method absent from semantic declarations"
                            .into(),
                    ),
                    caller_info.span,
                )
            })?;
        events.push(super::NamedMethodDependencyEvent {
            caller_file: caller_info.span.file_id.index(),
            caller_owner_name: caller_owner_name.clone(),
            caller_method_name: caller_method_name.clone(),
            target: super::NamedMethodDependencyTargetEvent::NamedMethod {
                file: info.span.file_id.index(),
                owner_name,
                method_name: sema.interner.resolve(callee_method).to_string(),
            },
        });
    }
    Ok(events)
}

fn named_destructor_dependency_events(
    sema: &Sema<'_>,
    caller_struct: StructId,
    caller_span: rue_span::Span,
    referenced_functions: &HashSet<Spur>,
    referenced_methods: &HashSet<(StructId, Spur)>,
) -> CompileResult<Vec<super::NamedDestructorDependencyEvent>> {
    let caller_owner_name = sema.type_pool.struct_def(caller_struct).name.clone();
    let mut events = Vec::new();
    for callee in referenced_functions {
        let info = sema.functions.get(callee).ok_or_else(|| {
            CompileError::new(
                ErrorKind::InvalidCompilerInput(
                    "named destructor references an absent free function".into(),
                ),
                caller_span,
            )
        })?;
        events.push(super::NamedDestructorDependencyEvent {
            caller_file: caller_span.file_id.index(),
            caller_owner_name: caller_owner_name.clone(),
            target: super::NamedMethodDependencyTargetEvent::FreeFunction {
                file: info.file_id.index(),
                name: sema
                    .interner
                    .resolve(&sema.source_function_name(*callee))
                    .to_string(),
            },
        });
    }
    for (callee_struct, callee_method) in referenced_methods {
        let owner_name = sema.type_pool.struct_def(*callee_struct).name.clone();
        if owner_name.starts_with("__anon_struct_") {
            continue;
        }
        let info = sema
            .methods
            .get(&(*callee_struct, *callee_method))
            .ok_or_else(|| {
                CompileError::new(
                    ErrorKind::InvalidCompilerInput(
                        "named destructor references an absent named method".into(),
                    ),
                    caller_span,
                )
            })?;
        events.push(super::NamedDestructorDependencyEvent {
            caller_file: caller_span.file_id.index(),
            caller_owner_name: caller_owner_name.clone(),
            target: super::NamedMethodDependencyTargetEvent::NamedMethod {
                file: info.span.file_id.index(),
                owner_name,
                method_name: sema.interner.resolve(callee_method).to_string(),
            },
        });
    }
    Ok(events)
}

/// Enqueue anonymous destructors registered by comptime evaluation.
///
/// Drop glue, rather than user-written AIR, is their caller, so these implicit
/// roots need an explicit deterministic scan whenever the ordinary reference
/// frontier drains.
fn enqueue_anonymous_destructors(
    sema: &Sema<'_>,
    drop_marker_sym: Spur,
    analyzed_methods: &HashSet<(StructId, Spur)>,
    pending_methods: &mut Vec<(StructId, Spur)>,
) {
    let mut destructors: Vec<(StructId, Spur)> = sema
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
    destructors.sort_by_key(|&(sid, name)| (sid.0, sema.interner.resolve(&name)));
    pending_methods.extend(destructors);
}

/// Demand-driven body-analysis path (ADR-0045).
///
/// Ordinary function and method bodies are analyzed only when reachable from
/// the entry point (`main`). Declaration gathering remains eager, and named
/// destructors are currently implicit roots because drop glue is synthesized
/// from the full type pool.
///
/// This is the same trade-off Zig makes for faster builds and smaller binaries.
fn analyze_function_bodies_lazy(sema: &mut Sema<'_>) -> MultiErrorResult<SemaOutput> {
    // Build inference context once
    let infer_ctx = sema.build_inference_context();

    // Find main() - the reference root for executable body analysis.
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
    let mut named_destructors_analyzed = false;
    let mut specializer = crate::specialize::Specializer::default();

    // Collect results
    let mut functions_with_strings: Vec<(AnalyzedFunction, Vec<String>)> = Vec::new();
    let mut errors = CompileErrors::new();
    let mut all_warnings = Vec::new();

    // Process the transitive reference frontier until neither source-level
    // calls nor implicit destructor roots discover more work.
    loop {
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

            // Bind the body through the exact free-function declaration that
            // declaration gathering indexed. A receiverless associated
            // function has the same FnDecl shape as a free function and must
            // never win this lookup merely because it occurs first in RIR.
            let source_name = sema.source_function_name(fn_name);
            sema.body_analysis_work.free_function_record_lookups += 1;
            let declaration = sema
                .declaration_index
                .first_free_function(source_name, Some(fn_info.file_id));

            let Some(declaration) = declaration else {
                // This could be a builtin or otherwise non-existent function.
                // Preserve the historical defensive behavior and skip it.
                continue;
            };
            let inst = sema.rir.get(declaration);
            let (name, params_start, params_len, return_type, body, has_self, span) =
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
                    (
                        *name,
                        *params_start,
                        *params_len,
                        *return_type,
                        *body,
                        *has_self,
                        inst.span,
                    )
                } else {
                    unreachable!("free-function index contains only FnDecl instructions");
                };

            debug_assert_eq!(name, source_name);
            debug_assert!(!has_self);
            debug_assert_eq!(params_start, fn_info.rir_params_start);
            debug_assert_eq!(params_len, fn_info.rir_params_len);
            debug_assert_eq!(return_type, fn_info.return_type_sym);
            debug_assert_eq!(body, fn_info.body);
            debug_assert_eq!(span, fn_info.span);
            debug_assert_eq!(span.file_id, fn_info.file_id);

            let params = sema.rir.get_params(params_start, params_len);

            match sema.analyze_single_function(
                &infer_ctx,
                &fn_name_str,
                return_type,
                &params,
                body,
                span,
                fn_info.allow_unused_variable,
                fn_info.allow_unreachable_code,
            ) {
                Ok((analyzed, warnings, local_strings, referenced_fns, referenced_meths)) => {
                    for callee in &referenced_fns {
                        if let Some(callee_info) = sema.functions.get(callee) {
                            sema.ordinary_free_function_dependencies.push(
                                super::OrdinaryFreeFunctionDependencyEvent {
                                    caller_file: fn_info.file_id.index(),
                                    caller_name: sema.interner.resolve(&source_name).to_string(),
                                    callee_file: callee_info.file_id.index(),
                                    callee_name: sema
                                        .interner
                                        .resolve(&sema.source_function_name(*callee))
                                        .to_string(),
                                },
                            );
                            sema.body_analysis_work
                                .ordinary_free_function_dependency_events += 1;
                        }
                    }
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
            let method_name_str = sema.interner.resolve(&method_name).to_string();

            // For anonymous structs, use the MethodInfo directly since there's no named StructDecl
            if type_name_str.starts_with("__anon_struct_") {
                sema.body_analysis_work.anonymous_method_record_lookups += 1;
                let full_name =
                    sema.method_symbol(struct_id, &method_name_str, method_info.has_self);

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

            sema.body_analysis_work.named_method_record_lookups += 1;
            let Some(&method_ref) = sema
                .named_method_declarations
                .get(&(struct_id, method_name))
            else {
                debug_assert!(
                    false,
                    "gathered named MethodInfo must have a private declaration record"
                );
                continue;
            };
            let method_inst = sema.rir.get(method_ref);
            let InstData::FnDecl {
                name: m_name,
                params_start,
                params_len,
                return_type,
                body,
                has_self,
                self_mode,
                ..
            } = &method_inst.data
            else {
                unreachable!("named method record pointed at a non-FnDecl");
            };
            debug_assert_eq!(*m_name, method_name);
            debug_assert_eq!(*body, method_info.body);
            debug_assert_eq!(method_inst.span, method_info.span);
            debug_assert_eq!(*has_self, method_info.has_self);
            debug_assert_eq!(*self_mode, method_info.self_mode);

            let params = sema.rir.get_params(*params_start, *params_len);
            let full_name = sema.method_symbol(struct_id, &method_name_str, *has_self);
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
                Ok((analyzed, warnings, local_strings, referenced_fns, referenced_meths)) => {
                    let caller_is_generic = sema
                        .param_arena
                        .comptime(method_info.params)
                        .iter()
                        .any(|is_comptime| *is_comptime);
                    if caller_is_generic {
                    } else {
                        match named_method_dependency_events(
                            sema,
                            struct_id,
                            method_name,
                            &referenced_fns,
                            &referenced_meths,
                        ) {
                            Ok(events) => {
                                sema.body_analysis_work.named_method_dependency_events +=
                                    events.len();
                                sema.named_method_dependencies.extend(events);
                            }
                            Err(error) => {
                                errors.push(error);
                                continue;
                            }
                        }
                    }
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

        // Anonymous destructors are not referenced by user-written call AIR;
        // drop glue adds those calls later for instantiated anonymous types.
        // Once comptime evaluation has registered such a destructor, enqueue it
        // so lazy analysis emits `__anon_struct_N.__drop` before the backend
        // links drop glue.
        if pending_functions.is_empty() && pending_methods.is_empty() {
            enqueue_anonymous_destructors(
                sema,
                drop_marker_sym,
                &analyzed_methods,
                &mut pending_methods,
            );
        }

        if !pending_functions.is_empty() || !pending_methods.is_empty() {
            continue;
        }

        // Named destructors are implicit roots: drop glue can call them without
        // a source-level reference. Analyze each once, then feed any calls made
        // by their bodies back through the same deterministic work queues.
        if !named_destructors_analyzed {
            named_destructors_analyzed = true;
            // Copy the request-local records before mutating `sema` below.
            // The loop finishes selecting every destructor before the queued
            // references are processed on the next fixed-point iteration.
            let destructor_records = sema.declaration_index.destructors().to_vec();
            for destructor in destructor_records {
                sema.body_analysis_work
                    .named_destructor_declarations_visited += 1;
                // File-aware first (RUE-558), matching the historical scan.
                let struct_id = match sema
                    .structs_by_file_name
                    .get(&(destructor.span.file_id, destructor.type_name))
                    .or_else(|| sema.structs.get(&destructor.type_name))
                {
                    Some(id) => *id,
                    None => continue,
                };
                let struct_type = Type::new_struct(struct_id);
                let full_name = sema.destructor_symbol(struct_id);

                match sema.analyze_destructor_function(
                    &infer_ctx,
                    &full_name,
                    destructor.body,
                    destructor.span,
                    struct_type,
                ) {
                    Ok((analyzed, warnings, local_strings, referenced_fns, referenced_meths)) => {
                        match named_destructor_dependency_events(
                            sema,
                            struct_id,
                            destructor.span,
                            &referenced_fns,
                            &referenced_meths,
                        ) {
                            Ok(events) => {
                                sema.body_analysis_work.named_destructor_dependency_events +=
                                    events.len();
                                sema.named_destructor_dependencies.extend(events);
                            }
                            Err(error) => {
                                errors.push(error);
                                continue;
                            }
                        }
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
            continue;
        }

        // Specialized generic bodies are part of the same reachability fixed
        // point as source-level bodies. Feed every ordinary function or method
        // they call back through the deterministic analysis queues, then scan
        // any later source bodies for further specialization requests.
        match specializer.run_to_fixpoint(
            &mut functions_with_strings,
            &mut all_warnings,
            sema,
            &infer_ctx,
            sema.interner,
        ) {
            Ok(discovered) => enqueue_references_sorted(
                sema.interner,
                discovered.functions,
                discovered.methods,
                &analyzed_functions,
                &analyzed_methods,
                &mut pending_functions,
                &mut pending_methods,
            ),
            Err(error) => {
                errors.push(error);
                break;
            }
        }

        // Comptime evaluation inside a specialization may register a new
        // anonymous destructor without an explicit call in its AIR. Keep it as
        // an implicit root, just like anonymous destructors registered by an
        // ordinary source body above.
        if pending_functions.is_empty() && pending_methods.is_empty() {
            enqueue_anonymous_destructors(
                sema,
                drop_marker_sym,
                &analyzed_methods,
                &mut pending_methods,
            );
        }

        if !pending_functions.is_empty() || !pending_methods.is_empty() {
            continue;
        }

        break;
    }

    finalize_function_body_analysis(
        sema,
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
            // Strip the RUE-571 file qualifier (`P$2` -> `P`) for display.
            let type_name = type_name.split('$').next().unwrap_or(type_name);
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

/// Shared checks for a module-member function call. Validates module
/// membership, visibility, and arity against the callee.
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
    module_name: &str,
    module_file_id: Option<FileId>,
    member_file_id: FileId,
    fn_name_str: &str,
    param_types: &[Type],
    args: &[RirCallArg],
    accessible: bool,
    via_reexport: bool,
    span: Span,
) -> CompileResult<()> {
    // Check membership: the function must be defined in the module's file
    // (canonical FileId equality) — UNLESS the call resolved through a
    // re-export const in the facade, whose presence is the membership grant
    // (`pub const f = @import("x").f;`, ADR-0026, RUE-592).
    if !via_reexport && module_file_id != Some(member_file_id) {
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
pub(crate) use ownership::FirstClassStrSite;
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
            body_analysis_work: crate::BodyAnalysisWork::default(),
            ordinary_free_function_dependencies: Vec::new(),
            ordinary_free_function_dependencies_complete: false,
            specialized_free_function_origins: Vec::new(),
            specialized_free_function_dependencies: Vec::new(),
            specialized_free_function_dependencies_complete: false,
            named_method_dependencies: Vec::new(),
            non_generic_named_method_dependencies_complete: false,
            generic_named_method_dependencies_complete: false,
            named_destructor_dependencies: Vec::new(),
            named_destructor_dependencies_complete: false,
            declaration_type_dependencies: Vec::new(),
            declaration_type_dependencies_complete: false,
        }
    }

    fn func_named(name: &str, air: Air) -> AnalyzedFunction {
        AnalyzedFunction {
            name: name.to_string(),
            air,
            num_locals: 0,
            num_param_slots: 0,
            param_modes: ParamSlotModes::default(),
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
