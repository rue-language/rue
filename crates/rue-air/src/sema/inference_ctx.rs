//! Demand-populated type information for constraint generation.
//!
//! This module contains [`InferenceContext`], the per-analysis cache that backs
//! Hindley-Milner constraint generation, and [`SemaInferenceFacts`], the
//! [`LazyInferenceFacts`] provider that materializes each consulted
//! function/struct/enum/method/const family on first lookup instead of eagerly
//! projecting the whole declaration universe (RUE-1091 slice r5b).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use lasso::Spur;
use rue_span::FileId;

use super::{ConstValue, DeclarationPhase, Sema};
use crate::inference::{FunctionSig, LazyInferenceFacts, MethodSig};
use crate::types::{ModuleId, StructId, Type};

/// Demand-population cache for constraint generation (RUE-1091 slice r5b).
///
/// Constraint generation consults the thirteen declaration-universe families
/// purely by key. Rather than convert every function/method signature and
/// project every struct/enum/const into owned `HashMap`s before a single body
/// is analyzed — the O(universe)-per-body term RUE-1083 removes — this context
/// holds only:
///
/// - **positive-only caches** for the two non-`Copy` signature families
///   (`func_sigs`, `method_sigs`), materialized on first consult through
///   [`SemaInferenceFacts`]; and
/// - **small frozen snapshots** of the generated (synthetic) nominal overlays
///   that the eager projection merged at build time, so a slice/`Str(N)` struct
///   generated while analyzing a *later* body cannot retroactively change an
///   earlier body's builtin/by-file type resolution.
///
/// Every other family is answered by a direct keyed lookup against the frozen
/// declaration state, which is already O(consumed).
///
/// The cache carries no borrow of `Sema`: it is built once and shared across
/// every reached body's `&mut self` analysis, so it cannot hold `&Sema`. The
/// fill source (immutable `Sema` sub-borrows) is threaded separately per body
/// via [`SemaInferenceFacts`], which is sound because Phase-1 constraint
/// generation only reads `Sema` immutably.
///
/// # Cross-body caching soundness
///
/// The two signature caches are shared across bodies (in the whole-program
/// path) so each consulted signature is converted at most once. Named functions
/// and methods are frozen after declaration gathering, so a cached value never
/// diverges. Anonymous-struct methods are registered lazily during body
/// analysis, but their `(StructId, name)` entries are immutable once present and
/// negatives are never cached, so a late-registered method is materialized on
/// its first consult — subsuming the old per-body `extra_method_sigs`
/// reconciliation (RUE-164) with no behavior change.
#[derive(Debug, Default)]
pub struct InferenceContext {
    func_sigs: RefCell<HashMap<Spur, Rc<FunctionSig>>>,
    method_sigs: RefCell<HashMap<(StructId, Spur), Rc<MethodSig>>>,
    /// Generated builtin struct nominals present when the context was built,
    /// keyed by source name. In the eager projection these *overwrote* the base
    /// builtin table, so they win over `builtin_structs` at lookup.
    gen_builtin_struct_types: HashMap<Spur, Type>,
    /// Generated struct nominals keyed by (defining file, source name). In the
    /// eager projection the base `structs_by_file_name` was inserted first and
    /// these only filled gaps (`or_insert`), so the base wins at lookup.
    gen_struct_types_by_file: HashMap<(FileId, Spur), Type>,
    /// Generated enum nominals keyed by (defining file, source name). Base wins,
    /// as for `gen_struct_types_by_file`.
    gen_enum_types_by_file: HashMap<(FileId, Spur), Type>,
}

impl InferenceContext {
    /// Construct the context, snapshotting the generated-nominal overlays that
    /// the eager projection merged. The signature caches start empty and fill on
    /// demand.
    pub(crate) fn new<D: DeclarationPhase>(sema: &Sema<'_, D>) -> Self {
        let mut gen_builtin_struct_types = HashMap::new();
        let mut gen_struct_types_by_file = HashMap::new();
        for (name, id) in &sema.generated_structs {
            let def = sema.type_pool.struct_def(*id);
            if def.is_builtin {
                gen_builtin_struct_types.insert(*name, Type::new_struct(*id));
            }
            gen_struct_types_by_file
                .entry((def.file_id, *name))
                .or_insert_with(|| Type::new_struct(*id));
        }
        let mut gen_enum_types_by_file = HashMap::new();
        for (name, id) in &sema.generated_enums {
            let def = sema.type_pool.enum_def(*id);
            gen_enum_types_by_file
                .entry((def.file_id, *name))
                .or_insert_with(|| Type::new_enum(*id));
        }
        Self {
            func_sigs: RefCell::new(HashMap::new()),
            method_sigs: RefCell::new(HashMap::new()),
            gen_builtin_struct_types,
            gen_struct_types_by_file,
            gen_enum_types_by_file,
        }
    }
}

/// The demand-population provider that materializes inference-context families
/// from a body's frozen `Sema` state (RUE-1091 slice r5b).
///
/// Constructed fresh per `run_type_inference` call, holding the shared
/// [`InferenceContext`] cache plus an immutable borrow of the analyzing `Sema`.
/// The generator consults it through [`LazyInferenceFacts`]; every answer equals
/// the value the eager projection would have held for that key.
pub(crate) struct SemaInferenceFacts<'a, 'src, D: DeclarationPhase> {
    ctx: &'a InferenceContext,
    sema: &'a Sema<'src, D>,
}

impl<'a, 'src, D: DeclarationPhase> SemaInferenceFacts<'a, 'src, D> {
    pub(crate) fn new(ctx: &'a InferenceContext, sema: &'a Sema<'src, D>) -> Self {
        Self { ctx, sema }
    }

    fn build_func_sig(&self, name: Spur) -> Option<FunctionSig> {
        let info = self.sema.functions.get(&name)?;
        Some(FunctionSig {
            param_types: self
                .sema
                .param_arena
                .types(info.params)
                .iter()
                .map(|t| self.sema.type_to_infer_type(*t))
                .collect(),
            return_type: self.sema.type_to_infer_type(info.return_type),
            is_generic: info.is_generic,
            param_modes: self.sema.param_arena.modes(info.params).to_vec(),
            param_comptime: self.sema.param_arena.comptime(info.params).to_vec(),
            param_comptime_type: self.sema.comptime_type_param_flags(info),
            param_names: self.sema.param_arena.names(info.params).to_vec(),
            param_type_syms: self
                .sema
                .rir
                .params(info.rir_params(self.sema.rir))
                .iter()
                .map(|p| p.ty)
                .collect(),
            return_type_sym: info.return_type_sym,
        })
    }

    fn build_method_sig(&self, key: (StructId, Spur)) -> Option<MethodSig> {
        // Anonymous-wins, matching `Sema::method_info` and the retired eager
        // projection's chain order. The keyspaces are disjoint today, so the
        // order is unobservable — but every consult site must agree on it.
        let info = self
            .sema
            .anonymous_methods
            .get(&key)
            .or_else(|| self.sema.methods.get(&key))?;
        Some(MethodSig {
            struct_type: info.struct_type,
            has_self: info.has_self,
            param_types: self
                .sema
                .param_arena
                .types(info.params)
                .iter()
                .map(|t| self.sema.type_to_infer_type(*t))
                .collect(),
            param_modes: self.sema.param_arena.modes(info.params).to_vec(),
            return_type: self.sema.type_to_infer_type(info.return_type),
        })
    }
}

impl<D: DeclarationPhase> LazyInferenceFacts for SemaInferenceFacts<'_, '_, D> {
    fn func_sig(&self, name: Spur) -> Option<Rc<FunctionSig>> {
        if let Some(cached) = self.ctx.func_sigs.borrow().get(&name) {
            return Some(Rc::clone(cached));
        }
        let sig = Rc::new(self.build_func_sig(name)?);
        self.ctx
            .func_sigs
            .borrow_mut()
            .insert(name, Rc::clone(&sig));
        Some(sig)
    }

    fn method_sig(&self, key: (StructId, Spur)) -> Option<Rc<MethodSig>> {
        if let Some(cached) = self.ctx.method_sigs.borrow().get(&key) {
            return Some(Rc::clone(cached));
        }
        let sig = Rc::new(self.build_method_sig(key)?);
        self.ctx
            .method_sigs
            .borrow_mut()
            .insert(key, Rc::clone(&sig));
        Some(sig)
    }

    fn builtin_struct_type(&self, name: Spur) -> Option<Type> {
        // Generated builtin overlays overwrote the base table in the eager
        // projection, so they win here.
        self.ctx
            .gen_builtin_struct_types
            .get(&name)
            .copied()
            .or_else(|| {
                self.sema
                    .builtin_structs
                    .get(&name)
                    .map(|id| Type::new_struct(*id))
            })
    }

    fn struct_type_by_file(&self, key: (FileId, Spur)) -> Option<Type> {
        // Base declarations win; generated overlays only filled gaps.
        self.sema
            .structs_by_file_name
            .get(&key)
            .map(|id| Type::new_struct(*id))
            .or_else(|| self.ctx.gen_struct_types_by_file.get(&key).copied())
    }

    fn builtin_enum_type(&self, name: Spur) -> Option<Type> {
        // The eager projection built builtin enum types from the base table
        // only (no generated overlay).
        self.sema
            .builtin_enums
            .get(&name)
            .map(|id| Type::new_enum(*id))
    }

    fn enum_type_by_file(&self, key: (FileId, Spur)) -> Option<Type> {
        self.sema
            .enums_by_file_name
            .get(&key)
            .map(|id| Type::new_enum(*id))
            .or_else(|| self.ctx.gen_enum_types_by_file.get(&key).copied())
    }

    fn const_type(&self, key: (FileId, Spur)) -> Option<Type> {
        self.sema.value_const(&key).map(|info| match info.value {
            ConstValue::Type(_) => Type::COMPTIME_TYPE,
            _ => info.ty,
        })
    }

    fn const_type_alias(&self, key: (FileId, Spur)) -> Option<Type> {
        self.sema
            .value_const(&key)
            .and_then(|info| match info.value {
                ConstValue::Type(ty) => Some(ty),
                _ => None,
            })
    }

    fn const_value(&self, key: (FileId, Spur)) -> Option<i128> {
        self.sema
            .value_const(&key)
            .and_then(|info| info.value.as_int_value())
    }

    fn const_function_alias(&self, key: (FileId, Spur)) -> Option<Spur> {
        self.sema
            .value_const(&key)
            .and_then(|info| info.value.as_function())
    }

    fn module_binding_type(&self, key: (FileId, Spur)) -> Option<Type> {
        self.sema.module_binding(&key).map(|info| info.ty)
    }

    fn module_file_id(&self, module: ModuleId) -> Option<FileId> {
        // The eager projection keyed every module id in `0..registry.len()`.
        if (module.index() as usize) < self.sema.module_registry.len() {
            Some(self.sema.module_registry.get_def(module).file_id)
        } else {
            None
        }
    }

    fn function_by_file(&self, key: (FileId, Spur)) -> Option<Spur> {
        self.sema.functions_by_file_name.get(&key).copied()
    }
}
