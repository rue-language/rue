//! Generic function specialization pass.
//!
//! This module provides the specialization pass that transforms `CallGeneric`
//! instructions into regular `Call` instructions by:
//!
//! 1. Collecting all `CallGeneric` instructions in the analyzed functions
//! 2. For each unique (func_name, type_args, value_args) combination, creating
//!    a specialized function
//! 3. Rewriting `CallGeneric` to `Call` with the specialized function name
//!
//! Specialization covers both comptime *type* parameters (`comptime T: type`)
//! and comptime *value* parameters (`comptime n: i32`, RUE-166): each distinct
//! combination of type and value arguments gets its own specialized body, in
//! which the value parameters are compile-time constants (available to
//! `comptime` blocks and forwardable to other comptime parameters).
//!
//! Specialized bodies can contain further `CallGeneric` instructions (a generic
//! function forwarding its type parameter to another generic, or a
//! comptime-recursive function like `fact(comptime n)` calling `fact(n - 1)`),
//! so these steps repeat until no new specializations are discovered.
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
use crate::sema::{AnalyzedFunction, ConstValue, FunctionInfo, InferenceContext, Sema, SemaOutput};
use crate::types::Type;

/// A key for a specialized function:
/// (base_function_name, type_arguments, value_arguments).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpecializationKey {
    /// Base function name (e.g., "identity")
    pub base_name: Spur,
    /// Type arguments (e.g., [Type::I32]), one per comptime type parameter
    pub type_args: Vec<Type>,
    /// Comptime value arguments (e.g., [Integer(5)]), one per comptime value
    /// parameter (RUE-166)
    pub value_args: Vec<ConstValue>,
}

// ============================================================================
// ConstValue <-> extra-array encoding
// ============================================================================
//
// Comptime value arguments travel from the call site (sema) to this pass
// inside the AIR extra array (a flat `u32` stream), as a tagged word stream:
// each value is a tag word followed by its payload words.

/// Tag for `ConstValue::Integer` (payload: 4 words, i128 as little-endian u32s).
const CV_TAG_INTEGER: u32 = 0;
/// Tag for `ConstValue::Bool` (payload: 1 word, 0 or 1).
const CV_TAG_BOOL: u32 = 1;
/// Tag for `ConstValue::Unit` (no payload).
const CV_TAG_UNIT: u32 = 2;
/// Tag for `ConstValue::Type` (payload: 1 word, `Type::as_u32`).
const CV_TAG_TYPE: u32 = 3;

/// Encode comptime value arguments as a tagged `u32` word stream for the AIR
/// extra array. Decoded by [`decode_const_values`].
pub fn encode_const_values(values: &[ConstValue]) -> Vec<u32> {
    let mut words = Vec::with_capacity(values.len() * 5);
    for value in values {
        match value {
            ConstValue::Integer(n) => {
                words.push(CV_TAG_INTEGER);
                let bits = *n as u128;
                for i in 0..4 {
                    words.push((bits >> (32 * i)) as u32);
                }
            }
            ConstValue::Bool(b) => {
                words.push(CV_TAG_BOOL);
                words.push(u32::from(*b));
            }
            ConstValue::Unit => words.push(CV_TAG_UNIT),
            ConstValue::Type(ty) => {
                words.push(CV_TAG_TYPE);
                words.push(ty.as_u32());
            }
        }
    }
    words
}

/// Decode a tagged word stream produced by [`encode_const_values`].
pub fn decode_const_values(words: &[u32]) -> Vec<ConstValue> {
    let mut values = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let tag = words[i];
        i += 1;
        let value = match tag {
            CV_TAG_INTEGER => {
                let mut bits: u128 = 0;
                for j in 0..4 {
                    bits |= u128::from(words[i + j]) << (32 * j);
                }
                i += 4;
                ConstValue::Integer(bits as i128)
            }
            CV_TAG_BOOL => {
                let b = words[i] != 0;
                i += 1;
                ConstValue::Bool(b)
            }
            CV_TAG_UNIT => ConstValue::Unit,
            CV_TAG_TYPE => {
                let ty = Type::from_u32(words[i]);
                i += 1;
                ConstValue::Type(ty)
            }
            _ => unreachable!("invalid ConstValue tag {tag} in extra array"),
        };
        values.push(value);
    }
    values
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
/// calls — including comptime-value recursion (`fact(comptime n)` calling
/// `fact(n - 1)` adds one round per distinct value, RUE-166). Unbounded
/// growth is possible when a generic function instantiates itself with an
/// ever-growing type (e.g. `f(Pair(T), ...)` inside `f`) or a
/// comptime-recursive function never reaches a comptime-known base case,
/// so this cap turns a would-be-infinite loop into a clean E1200.
pub(crate) const MAX_SPECIALIZATION_ROUNDS: usize = 64;

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
                        "specialization of '{}' exceeded the maximum nesting depth ({}); \
                         is a comptime-recursive function missing a compile-time-known \
                         base case, or a generic function recursively instantiating \
                         itself with new types?",
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
            value_args_start,
            value_args_len,
            ..
        } = &inst.data
        {
            let type_args: Vec<Type> = air
                .get_extra(*type_args_start, *type_args_len)
                .iter()
                .map(|&encoded| Type::from_u32(encoded))
                .collect();
            let value_args = decode_const_values(air.get_extra(*value_args_start, *value_args_len));

            let key = SpecializationKey {
                base_name: *name,
                type_args,
                value_args,
            };

            if let Entry::Vacant(entry) = specializations.entry(key) {
                let base_name = interner.resolve(name);
                let mangled = mangle_specialized_name(
                    base_name,
                    &entry.key().type_args,
                    &entry.key().value_args,
                );
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
            value_args_start,
            value_args_len,
            args_start,
            args_len,
        } = &inst.data
        {
            let type_args: Vec<Type> = air
                .get_extra(*type_args_start, *type_args_len)
                .iter()
                .map(|&encoded| Type::from_u32(encoded))
                .collect();
            let value_args = decode_const_values(air.get_extra(*value_args_start, *value_args_len));

            let key = SpecializationKey {
                base_name: *name,
                type_args,
                value_args,
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

/// Separator between mangled segments in a specialized symbol name.
///
/// A `.` is deliberately *illegal* in a Rue identifier (which is
/// `[a-zA-Z_][a-zA-Z0-9_]*`), so a specialized symbol like `identity.i32`
/// lives in a namespace disjoint from every name a user can spell. This is
/// the targeted fix for RUE-41: previously the separator was `__`, so
/// `identity<i32>` mangled to `identity__i32` — a name a user could legally
/// define as a plain function, producing a duplicate-symbol link error for a
/// valid program. Switching to an identifier-illegal separator makes the
/// collision impossible by construction, without a full user-symbol mangling
/// overhaul (that is RUE-178). The full user-symbol mangling scheme (RUE-178)
/// is still future work; this only guarantees specialized names can't clash
/// with user names.
const SPEC_SEP: char = '.';

/// Generate a mangled name for a specialized function.
///
/// Type arguments and value arguments each contribute a [`SPEC_SEP`]-separated
/// segment, so distinct comptime values yield distinct symbols
/// (`fact.v5`, `fact.v4`, ...; RUE-166) exactly like distinct types do
/// (RUE-100's lesson: colliding mangles mean duplicate symbols at link time).
/// The `.` separator is illegal in Rue identifiers, so these names cannot
/// collide with a user-spellable function name (RUE-41).
fn mangle_specialized_name(
    base_name: &str,
    type_args: &[Type],
    value_args: &[ConstValue],
) -> String {
    let mut mangled = base_name.to_string();
    for ty in type_args {
        mangled.push(SPEC_SEP);
        mangled.push_str(&mangle_type(*ty));
    }
    for value in value_args {
        mangled.push(SPEC_SEP);
        mangled.push_str(&mangle_const_value(value));
    }
    mangled
}

/// Mangle a single comptime value argument into a unique symbol fragment.
///
/// Negative integers use an `m` (minus) prefix on the magnitude because `-`
/// is not a safe symbol character (`-3` -> `vm3`).
fn mangle_const_value(value: &ConstValue) -> String {
    match value {
        ConstValue::Integer(n) if *n < 0 => format!("vm{}", n.unsigned_abs()),
        ConstValue::Integer(n) => format!("v{}", n),
        ConstValue::Bool(b) => format!("v{}", b),
        ConstValue::Unit => "vunit".to_string(),
        ConstValue::Type(ty) => format!("v{}", mangle_type(*ty)),
    }
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

/// Create a specialized function by re-analyzing the body with type and
/// value substitution.
///
/// This builds a type substitution map from the comptime type parameters to
/// their concrete type arguments and a value substitution map from the
/// comptime value parameters to their concrete values (RUE-166), then
/// re-analyzes the function body with these substitutions.
fn create_specialized_function(
    sema: &mut Sema<'_>,
    infer_ctx: &InferenceContext,
    key: &SpecializationKey,
    specialized_name: Spur,
    base_info: &FunctionInfo,
    interner: &ThreadedRodeo,
) -> CompileResult<AnalyzedFunction> {
    let specialized_name_str = interner.resolve(&specialized_name).to_string();

    // Pair each comptime parameter with its argument: type parameters
    // (declared `: type`) consume the type_args stream, value parameters
    // consume the value_args stream — mirroring how the call site collected
    // them (both in parameter order).
    let mut type_subst: HashMap<Spur, Type> = HashMap::new();
    let mut value_subst: HashMap<Spur, ConstValue> = HashMap::new();
    let mut type_arg_idx = 0;
    let mut value_arg_idx = 0;
    for (name, ty, _, is_comptime) in sema.param_arena.iter(base_info.params) {
        if !*is_comptime {
            continue;
        }
        if *ty == Type::COMPTIME_TYPE {
            if type_arg_idx < key.type_args.len() {
                type_subst.insert(*name, key.type_args[type_arg_idx]);
                type_arg_idx += 1;
            }
        } else if value_arg_idx < key.value_args.len() {
            value_subst.insert(*name, key.value_args[value_arg_idx]);
            value_arg_idx += 1;
        }
    }

    // Resolve the return type, substituting type parameters - bare (`-> T`)
    // or inside a composite (`-> [T; 3]`, RUE-172). Value parameters also
    // substitute so an array length can name a `comptime N: i32` (`-> [i32; N]`,
    // RUE-16).
    let return_type = if base_info.return_type == Type::COMPTIME_TYPE {
        sema.resolve_type_for_comptime_with_subst_and_values(
            base_info.return_type_sym,
            &type_subst,
            &value_subst,
        )
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
            if ty == Type::COMPTIME_TYPE {
                // Comptime TYPE parameters are erased from the specialized
                // signature (types don't exist at runtime).
                continue;
            }
            // Comptime VALUE parameters stay in the runtime signature — the
            // call site still passes them (see `analyze_call`); their constant
            // value is additionally substituted into the body via value_subst
            // so comptime contexts see it (RUE-166).
            specialized_params.push((name, ty, mode, true));
            continue;
        }
        let concrete_ty = if ty == Type::COMPTIME_TYPE {
            substitute_param_type(sema, base_info, name, &type_subst, &value_subst).unwrap_or_else(
                || {
                    debug_assert!(false, "type substitution failed for param");
                    ty
                },
            )
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
        &value_subst,
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
/// (`a: [T; 3]`, `p: ptr const T`; RUE-172). Value parameters also substitute
/// so an array length can name a `comptime N: i32` (`arr: [i32; N]`, RUE-16).
fn substitute_param_type(
    sema: &mut Sema<'_>,
    base_info: &FunctionInfo,
    param_name: Spur,
    type_subst: &HashMap<Spur, Type>,
    value_subst: &HashMap<Spur, ConstValue>,
) -> Option<Type> {
    let type_sym = sema
        .rir
        .get_params(base_info.rir_params_start, base_info.rir_params_len)
        .iter()
        .find(|param| param.name == param_name)
        .map(|param| param.ty)?;
    sema.resolve_type_for_comptime_with_subst_and_values(type_sym, type_subst, value_subst)
}
