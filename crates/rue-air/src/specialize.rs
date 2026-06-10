//! Generic function specialization pass.
//!
//! This module provides the specialization pass that transforms `CallGeneric`
//! instructions into regular `Call` instructions by:
//!
//! 1. Collecting all `CallGeneric` instructions in the analyzed functions
//! 2. For each unique (func_name, type_args) combination, creating a specialized function
//! 3. Rewriting `CallGeneric` to `Call` with the specialized function name
//!
//! # Architecture
//!
//! The specialization pass runs after semantic analysis but before CFG building.
//! It transforms the AIR in-place and adds new specialized functions to the output.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use lasso::{Spur, ThreadedRodeo};
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_rir::RirParamMode;
use rue_span::Span;

use crate::inst::{Air, AirInstData};
use crate::sema::{AnalyzedFunction, FunctionInfo, InferenceContext, Sema, SemaOutput};
use crate::types::Type;

/// A key for a specialized function: (base_function_name, type_arguments).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpecializationKey {
    /// Base function name (e.g., "identity")
    pub base_name: Spur,
    /// Type arguments (e.g., [Type::I32])
    pub type_args: Vec<Type>,
}

/// Info about a specialization: the mangled name and the first call site span.
struct SpecializationInfo {
    /// The mangled name for the specialized function.
    mangled_name: Spur,
    /// The span of the first call site (for error reporting if the function doesn't exist).
    call_site_span: Span,
}

/// Perform the specialization pass on the sema output.
///
/// This collects all `CallGeneric` instructions, creates specialized functions,
/// and rewrites calls to point to the specialized versions.
pub fn specialize(
    output: &mut SemaOutput,
    sema: &mut Sema<'_>,
    infer_ctx: &InferenceContext,
    interner: &ThreadedRodeo,
) -> CompileResult<()> {
    // Phase 1: Collect all specialization requests
    let mut specializations: HashMap<SpecializationKey, SpecializationInfo> = HashMap::new();

    for func in &output.functions {
        collect_specializations(&func.air, interner, &mut specializations);
    }

    if specializations.is_empty() {
        // No generic calls, nothing to do
        return Ok(());
    }

    // Phase 2: Rewrite CallGeneric to Call in all functions
    for func in &mut output.functions {
        rewrite_call_generic(&mut func.air, &specializations);
    }

    // Phase 3: Create specialized function bodies by re-analyzing with type substitution
    for (key, info) in &specializations {
        let base_info = match sema.functions.get(&key.base_name) {
            Some(info) => info.clone(),
            None => {
                let func_name = interner.resolve(&key.base_name);
                return Err(CompileError::new(
                    ErrorKind::UndefinedFunction(func_name.to_string()),
                    info.call_site_span,
                ));
            }
        };
        let specialized_func = create_specialized_function(
            sema,
            infer_ctx,
            key,
            info.mangled_name,
            &base_info,
            interner,
        )?;
        output.functions.push(specialized_func);
    }

    Ok(())
}

fn collect_specializations(
    air: &Air,
    interner: &ThreadedRodeo,
    specializations: &mut HashMap<SpecializationKey, SpecializationInfo>,
) {
    for inst in air.instructions() {
        if let AirInstData::CallGeneric {
            name,
            type_args_start,
            type_args_len,
            ..
        } = &inst.data
        {
            let type_args: Vec<Type> = air
                .get_extra(*type_args_start, *type_args_len)
                .iter()
                .map(|&encoded| Type::from_u32(encoded))
                .collect();

            let key = SpecializationKey {
                base_name: *name,
                type_args,
            };

            if let Entry::Vacant(entry) = specializations.entry(key) {
                let base_name = interner.resolve(name);
                let mangled = mangle_specialized_name(base_name, &entry.key().type_args);
                let mangled_sym = interner.get_or_intern(&mangled);
                entry.insert(SpecializationInfo {
                    mangled_name: mangled_sym,
                    call_site_span: inst.span,
                });
            }
        }
    }
}

fn rewrite_call_generic(
    air: &mut Air,
    specializations: &HashMap<SpecializationKey, SpecializationInfo>,
) {
    let mut rewrites: Vec<(usize, AirInstData)> = Vec::new();

    for (i, inst) in air.instructions().iter().enumerate() {
        if let AirInstData::CallGeneric {
            name,
            type_args_start,
            type_args_len,
            args_start,
            args_len,
        } = &inst.data
        {
            let type_args: Vec<Type> = air
                .get_extra(*type_args_start, *type_args_len)
                .iter()
                .map(|&encoded| Type::from_u32(encoded))
                .collect();

            let key = SpecializationKey {
                base_name: *name,
                type_args,
            };

            if let Some(info) = specializations.get(&key) {
                rewrites.push((
                    i,
                    AirInstData::Call {
                        name: info.mangled_name,
                        args_start: *args_start,
                        args_len: *args_len,
                    },
                ));
            }
        }
    }

    for (index, new_data) in rewrites {
        air.rewrite_inst_data(index, new_data);
    }
}

/// Generate a mangled name for a specialized function.
fn mangle_specialized_name(base_name: &str, type_args: &[Type]) -> String {
    let mut mangled = base_name.to_string();
    for ty in type_args {
        mangled.push_str("__");
        mangled.push_str(ty.name());
    }
    mangled
}

/// Create a specialized function by re-analyzing the body with type substitution.
///
/// This builds a type substitution map from the comptime parameters to their concrete
/// type arguments, then re-analyzes the function body with these substitutions.
fn create_specialized_function(
    sema: &mut Sema<'_>,
    infer_ctx: &InferenceContext,
    key: &SpecializationKey,
    specialized_name: Spur,
    base_info: &FunctionInfo,
    interner: &ThreadedRodeo,
) -> CompileResult<AnalyzedFunction> {
    let specialized_name_str = interner.resolve(&specialized_name).to_string();

    let mut type_subst: HashMap<Spur, Type> = HashMap::new();
    let mut type_arg_idx = 0;
    for (name, _, _, is_comptime) in sema.param_arena.iter(base_info.params) {
        if *is_comptime && type_arg_idx < key.type_args.len() {
            type_subst.insert(*name, key.type_args[type_arg_idx]);
            type_arg_idx += 1;
        }
    }

    let return_type = if base_info.return_type == Type::COMPTIME_TYPE {
        type_subst
            .get(&base_info.return_type_sym)
            .copied()
            .unwrap_or(Type::UNIT)
    } else {
        base_info.return_type
    };

    let specialized_params: Vec<(Spur, Type, RirParamMode)> = sema
        .param_arena
        .iter(base_info.params)
        .filter(|(_, _, _, is_comptime)| !**is_comptime)
        .map(|(name, ty, mode, _)| {
            let concrete_ty = if *ty == Type::COMPTIME_TYPE {
                substitute_param_type(sema, base_info, *name, &type_subst).unwrap_or_else(|| {
                    debug_assert!(false, "type substitution failed for param");
                    *ty
                })
            } else {
                *ty
            };
            (*name, concrete_ty, *mode)
        })
        .collect();

    let (
        air,
        num_locals,
        num_param_slots,
        param_modes,
        _warnings,
        _local_strings,
        _ref_fns,
        _ref_meths,
    ) = sema.analyze_specialized_function(
        infer_ctx,
        return_type,
        &specialized_params,
        base_info.body,
        &type_subst,
    )?;

    Ok(AnalyzedFunction {
        name: specialized_name_str,
        air,
        num_locals,
        num_param_slots,
        param_modes,
    })
}

/// Look up a parameter's concrete type from the type substitution map.
fn substitute_param_type(
    sema: &Sema<'_>,
    base_info: &FunctionInfo,
    param_name: Spur,
    type_subst: &HashMap<Spur, Type>,
) -> Option<Type> {
    let params = sema
        .rir
        .get_params(base_info.rir_params_start, base_info.rir_params_len);
    for param in params {
        if param.name == param_name {
            return type_subst.get(&param.ty).copied();
        }
    }
    None
}
