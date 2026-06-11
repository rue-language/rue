//! Generic function specialization pass.
//!
//! This module provides the specialization pass that transforms `CallGeneric`
//! instructions into regular `Call` instructions by:
//!
//! 1. Collecting all `CallGeneric` instructions in the analyzed functions
//! 2. For each unique (func_name, type_args) combination, creating a specialized function
//! 3. Rewriting `CallGeneric` to `Call` with the specialized function name
//!
//! Specialized bodies can contain further `CallGeneric` instructions (a generic
//! function forwarding its type parameter to another generic), so these steps
//! repeat until no new specializations are discovered.
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

/// Maximum number of specialization rounds before giving up.
///
/// Each round creates the bodies for the specializations discovered in the
/// previous round, so this bounds the nesting depth of generic-to-generic
/// calls. Unbounded growth is possible when a generic function instantiates
/// itself with an ever-growing type (e.g. `f(Pair(T), ...)` inside `f`).
const MAX_SPECIALIZATION_ROUNDS: usize = 64;

/// Perform the specialization pass on the sema output.
///
/// This collects all `CallGeneric` instructions, creates specialized functions,
/// and rewrites calls to point to the specialized versions.
///
/// Specialized function bodies can themselves contain `CallGeneric` instructions
/// (a generic function forwarding its type parameter to another generic call),
/// so the pass iterates to a fixpoint: each round scans the functions created in
/// the previous round for new specialization requests (RUE-102).
pub fn specialize(
    output: &mut SemaOutput,
    sema: &mut Sema<'_>,
    infer_ctx: &InferenceContext,
    interner: &ThreadedRodeo,
) -> CompileResult<()> {
    // All specializations ever requested, used to deduplicate across rounds.
    let mut specializations: HashMap<SpecializationKey, SpecializationInfo> = HashMap::new();
    // Index of the first function not yet scanned for CallGeneric instructions.
    let mut next_unscanned = 0;
    let mut rounds = 0;

    loop {
        // Phase 1: Collect specialization requests from not-yet-scanned functions
        let mut pending: Vec<SpecializationKey> = Vec::new();
        for func in &output.functions[next_unscanned..] {
            collect_specializations(&func.air, interner, &mut specializations, &mut pending);
        }

        // Phase 2: Rewrite CallGeneric to Call in the newly scanned functions
        // (previously scanned functions have no CallGeneric left)
        for func in &mut output.functions[next_unscanned..] {
            rewrite_call_generic(&mut func.air, &specializations);
        }
        next_unscanned = output.functions.len();

        if pending.is_empty() {
            return Ok(());
        }

        rounds += 1;
        if rounds > MAX_SPECIALIZATION_ROUNDS {
            let key = &pending[0];
            return Err(CompileError::new(
                ErrorKind::ComptimeEvaluationFailed {
                    reason: format!(
                        "generic specialization of '{}' exceeded the maximum nesting depth ({}); \
                         is a generic function recursively instantiating itself with new types?",
                        interner.resolve(&key.base_name),
                        MAX_SPECIALIZATION_ROUNDS
                    ),
                },
                specializations[key].call_site_span,
            ));
        }

        // Phase 3: Create specialized function bodies by re-analyzing with type
        // substitution. These new functions are scanned in the next round.
        for key in &pending {
            let info = &specializations[key];
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
    }
}

fn collect_specializations(
    air: &Air,
    interner: &ThreadedRodeo,
    specializations: &mut HashMap<SpecializationKey, SpecializationInfo>,
    pending: &mut Vec<SpecializationKey>,
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
                pending.push(entry.key().clone());
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
        mangled.push_str(&mangle_type(*ty));
    }
    mangled
}

/// Mangle a single type argument into a unique string.
///
/// Scalar types use their source-level name. Aggregate types must encode their
/// identity (the struct/enum/array/pointer ID): `Type::name()` returns a generic
/// placeholder like `"<struct>"` for them, which made every struct instantiation
/// of a generic function collide on a single specialized symbol (RUE-100).
fn mangle_type(ty: Type) -> String {
    use crate::types::TypeKind;
    match ty.kind() {
        TypeKind::Struct(id) => format!("struct{}", id.0),
        TypeKind::Enum(id) => format!("enum{}", id.0),
        TypeKind::Array(id) => format!("array{}", id.0),
        TypeKind::PtrConst(id) => format!("ptr_const{}", id.0),
        TypeKind::PtrMut(id) => format!("ptr_mut{}", id.0),
        TypeKind::Module(id) => format!("module{}", id.0),
        _ => ty.name().to_string(),
    }
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

    // Resolve the return type, substituting type parameters - bare (`-> T`)
    // or inside a composite (`-> [T; 3]`, RUE-172).
    let return_type = if base_info.return_type == Type::COMPTIME_TYPE {
        sema.resolve_type_for_comptime_with_subst(base_info.return_type_sym, &type_subst)
            .unwrap_or(Type::UNIT)
    } else {
        base_info.return_type
    };

    // Copy out the param data first: substitution needs `&mut Sema` (composite
    // types like `[T; 3]` may intern new array types), which can't overlap a
    // borrow of the param arena.
    let base_params: Vec<(Spur, Type, RirParamMode, bool)> = sema
        .param_arena
        .iter(base_info.params)
        .map(|(name, ty, mode, is_comptime)| (*name, *ty, *mode, *is_comptime))
        .collect();
    let mut specialized_params: Vec<(Spur, Type, RirParamMode, bool)> =
        Vec::with_capacity(base_params.len());
    for (name, ty, mode, is_comptime) in base_params {
        if is_comptime {
            // Comptime parameters are erased from the specialized signature.
            continue;
        }
        let concrete_ty = if ty == Type::COMPTIME_TYPE {
            substitute_param_type(sema, base_info, name, &type_subst).unwrap_or_else(|| {
                debug_assert!(false, "type substitution failed for param");
                ty
            })
        } else {
            ty
        };
        specialized_params.push((name, concrete_ty, mode, false));
    }

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

/// Resolve a parameter's concrete type by substituting type parameters into
/// its declared type symbol - bare (`x: T`) or inside a composite
/// (`a: [T; 3]`, `p: ptr const T`; RUE-172).
fn substitute_param_type(
    sema: &mut Sema<'_>,
    base_info: &FunctionInfo,
    param_name: Spur,
    type_subst: &HashMap<Spur, Type>,
) -> Option<Type> {
    let type_sym = sema
        .rir
        .get_params(base_info.rir_params_start, base_info.rir_params_len)
        .iter()
        .find(|param| param.name == param_name)
        .map(|param| param.ty)?;
    sema.resolve_type_for_comptime_with_subst(type_sym, type_subst)
}
