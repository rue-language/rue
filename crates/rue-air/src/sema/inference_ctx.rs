//! Demand-populated type information for constraint generation.
//!
//! This module contains [`InferenceContext`], the per-analysis cache that backs
//! Hindley-Milner constraint generation, and [`HostInferenceFacts`], the
//! [`LazyInferenceFacts`] provider that materializes each consulted
//! function/struct/enum/method/const family on first lookup instead of eagerly
//! projecting the whole declaration universe (RUE-1091 slice r5b).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use lasso::Spur;
use rue_span::FileId;

use super::fact_mode::BodyAnalysisReadHost;
use super::{ConstValue, DeclarationPhase, Sema};
use crate::inference::{FunctionSig, LazyInferenceFacts, MethodSig};
use crate::types::{ModuleId, StructId, Type};

/// The generated-nominal overlay snapshot consumed by [`InferenceContext`].
/// It is a construction-time input, not a replay cache for semantic results.
pub(crate) struct InferenceGeneratedNominalOverlays {
    pub(crate) builtin_struct_types: HashMap<Spur, Type>,
    pub(crate) struct_types_by_file: HashMap<(FileId, Spur), Type>,
    pub(crate) enum_types_by_file: HashMap<(FileId, Spur), Type>,
}

/// Immutable declaration reads required by the inference fact facade.
///
/// This is deliberately narrower than body evaluation: it supplies only the
/// point queries and construction snapshot used by constraint generation.
pub(super) trait InferenceFactSource {
    fn inference_generated_nominal_overlays(&self) -> InferenceGeneratedNominalOverlays;
    fn uncached_function_sig(&self, name: Spur) -> Option<FunctionSig>;
    fn uncached_method_sig(&self, key: (StructId, Spur)) -> Option<MethodSig>;
    fn inference_builtin_struct_type(&self, name: Spur) -> Option<Type>;
    fn inference_struct_type_by_file(&self, key: (FileId, Spur)) -> Option<Type>;
    fn inference_builtin_enum_type(&self, name: Spur) -> Option<Type>;
    fn inference_enum_type_by_file(&self, key: (FileId, Spur)) -> Option<Type>;
    fn inference_const_type(&self, key: (FileId, Spur)) -> Option<Type>;
    fn inference_const_type_alias(&self, key: (FileId, Spur)) -> Option<Type>;
    fn inference_const_value(&self, key: (FileId, Spur)) -> Option<i128>;
    fn inference_const_function_alias(&self, key: (FileId, Spur)) -> Option<Spur>;
    fn inference_module_binding_type(&self, key: (FileId, Spur)) -> Option<Type>;
    fn inference_module_file_id(&self, module: ModuleId) -> Option<FileId>;
    fn inference_function_by_file(&self, key: (FileId, Spur)) -> Option<Spur>;
}

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
///   [`HostInferenceFacts`]; and
/// - **small frozen snapshots** of the generated (synthetic) nominal overlays
///   that the eager projection merged at build time, so a slice/`Str(N)` struct
///   generated while analyzing a *later* body cannot retroactively change an
///   earlier body's builtin/by-file type resolution.
///
/// Every other family is answered by a direct keyed lookup against the frozen
/// declaration state, which is already O(consumed).
///
/// The cache carries no borrow of its host: it is built once and shared across
/// every reached body's analysis. The immutable fact source is threaded
/// separately per body via [`HostInferenceFacts`], which is sound because
/// Phase-1 constraint generation only performs reads.
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
    pub(crate) fn new<H: BodyAnalysisReadHost>(host: &H) -> Self {
        let InferenceGeneratedNominalOverlays {
            builtin_struct_types: gen_builtin_struct_types,
            struct_types_by_file: gen_struct_types_by_file,
            enum_types_by_file: gen_enum_types_by_file,
        } = host.inference_generated_nominal_overlays();
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
/// from a body's frozen read host (RUE-1091 slice r5b).
///
/// Constructed fresh per `run_type_inference` call, holding the shared
/// [`InferenceContext`] cache plus an immutable borrow of the analyzing host.
/// The generator consults it through [`LazyInferenceFacts`]; every answer equals
/// the value the eager projection would have held for that key.
pub(crate) struct HostInferenceFacts<'a, H: BodyAnalysisReadHost> {
    ctx: &'a InferenceContext,
    host: &'a H,
}

impl<'a, H: BodyAnalysisReadHost> HostInferenceFacts<'a, H> {
    pub(crate) fn new(ctx: &'a InferenceContext, host: &'a H) -> Self {
        Self { ctx, host }
    }

    fn build_func_sig(&self, name: Spur) -> Option<FunctionSig> {
        self.host.uncached_function_sig(name)
    }

    fn build_method_sig(&self, key: (StructId, Spur)) -> Option<MethodSig> {
        self.host.uncached_method_sig(key)
    }
}

impl<H: BodyAnalysisReadHost> LazyInferenceFacts for HostInferenceFacts<'_, H> {
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
            .or_else(|| self.host.inference_builtin_struct_type(name))
    }

    fn struct_type_by_file(&self, key: (FileId, Spur)) -> Option<Type> {
        // Base declarations win; generated overlays only filled gaps.
        self.host
            .inference_struct_type_by_file(key)
            .or_else(|| self.ctx.gen_struct_types_by_file.get(&key).copied())
    }

    fn builtin_enum_type(&self, name: Spur) -> Option<Type> {
        // The eager projection built builtin enum types from the base table
        // only (no generated overlay).
        self.host.inference_builtin_enum_type(name)
    }

    fn enum_type_by_file(&self, key: (FileId, Spur)) -> Option<Type> {
        self.host
            .inference_enum_type_by_file(key)
            .or_else(|| self.ctx.gen_enum_types_by_file.get(&key).copied())
    }

    fn const_type(&self, key: (FileId, Spur)) -> Option<Type> {
        self.host.inference_const_type(key)
    }

    fn const_type_alias(&self, key: (FileId, Spur)) -> Option<Type> {
        self.host.inference_const_type_alias(key)
    }

    fn const_value(&self, key: (FileId, Spur)) -> Option<i128> {
        self.host.inference_const_value(key)
    }

    fn const_function_alias(&self, key: (FileId, Spur)) -> Option<Spur> {
        self.host.inference_const_function_alias(key)
    }

    fn module_binding_type(&self, key: (FileId, Spur)) -> Option<Type> {
        self.host.inference_module_binding_type(key)
    }

    fn module_file_id(&self, module: ModuleId) -> Option<FileId> {
        self.host.inference_module_file_id(module)
    }

    fn function_by_file(&self, key: (FileId, Spur)) -> Option<Spur> {
        self.host.inference_function_by_file(key)
    }
}

impl<D: DeclarationPhase> InferenceFactSource for Sema<'_, D> {
    fn inference_generated_nominal_overlays(&self) -> InferenceGeneratedNominalOverlays {
        let mut builtin_struct_types = HashMap::new();
        let mut struct_types_by_file = HashMap::new();
        for (name, id) in &self.generated_structs {
            let def = self.type_pool.struct_def(*id);
            if def.is_builtin {
                builtin_struct_types.insert(*name, Type::new_struct(*id));
            }
            struct_types_by_file
                .entry((def.file_id, *name))
                .or_insert_with(|| Type::new_struct(*id));
        }
        let mut enum_types_by_file = HashMap::new();
        for (name, id) in &self.generated_enums {
            let def = self.type_pool.enum_def(*id);
            enum_types_by_file
                .entry((def.file_id, *name))
                .or_insert_with(|| Type::new_enum(*id));
        }
        InferenceGeneratedNominalOverlays {
            builtin_struct_types,
            struct_types_by_file,
            enum_types_by_file,
        }
    }

    fn uncached_function_sig(&self, name: Spur) -> Option<FunctionSig> {
        let info = self.functions.get(&name)?;
        Some(FunctionSig {
            param_types: self
                .param_arena
                .types(info.params)
                .iter()
                .map(|ty| self.type_to_infer_type(*ty))
                .collect(),
            return_type: self.type_to_infer_type(info.return_type),
            is_generic: info.is_generic,
            param_modes: self.param_arena.modes(info.params).to_vec(),
            param_comptime: self.param_arena.comptime(info.params).to_vec(),
            param_comptime_type: self.comptime_type_param_flags(info),
            param_names: self.param_arena.names(info.params).to_vec(),
            param_type_syntax: self
                .rir
                .params(info.rir_params(self.rir))
                .iter()
                .map(|param| {
                    Some(super::StructuredTypeSyntax {
                        arena: self.rir.type_syntax().clone(),
                        root: param.ty,
                    })
                })
                .collect(),
            return_type_syntax: match &self.rir.get(info.declaration).data {
                rue_rir::InstData::FnDecl { return_type, .. } => {
                    Some(super::StructuredTypeSyntax {
                        arena: self.rir.type_syntax().clone(),
                        root: *return_type,
                    })
                }
                _ => None,
            },
        })
    }

    fn uncached_method_sig(&self, key: (StructId, Spur)) -> Option<MethodSig> {
        let info = self
            .anonymous_methods
            .get(&key)
            .or_else(|| self.methods.get(&key))?;
        Some(MethodSig {
            struct_type: info.struct_type,
            has_self: info.has_self,
            param_types: self
                .param_arena
                .types(info.params)
                .iter()
                .map(|ty| self.type_to_infer_type(*ty))
                .collect(),
            param_modes: self.param_arena.modes(info.params).to_vec(),
            return_type: self.type_to_infer_type(info.return_type),
        })
    }

    fn inference_builtin_struct_type(&self, name: Spur) -> Option<Type> {
        self.builtin_structs
            .get(&name)
            .map(|id| Type::new_struct(*id))
    }

    fn inference_struct_type_by_file(&self, key: (FileId, Spur)) -> Option<Type> {
        self.structs_by_file_name
            .get(&key)
            .map(|id| Type::new_struct(*id))
    }

    fn inference_builtin_enum_type(&self, name: Spur) -> Option<Type> {
        self.builtin_enums.get(&name).map(|id| Type::new_enum(*id))
    }

    fn inference_enum_type_by_file(&self, key: (FileId, Spur)) -> Option<Type> {
        self.enums_by_file_name
            .get(&key)
            .map(|id| Type::new_enum(*id))
    }

    fn inference_const_type(&self, key: (FileId, Spur)) -> Option<Type> {
        self.value_const(&key).map(|info| match info.value {
            ConstValue::Type(_) => Type::COMPTIME_TYPE,
            _ => info.ty,
        })
    }

    fn inference_const_type_alias(&self, key: (FileId, Spur)) -> Option<Type> {
        self.value_const(&key).and_then(|info| match info.value {
            ConstValue::Type(ty) => Some(ty),
            _ => None,
        })
    }

    fn inference_const_value(&self, key: (FileId, Spur)) -> Option<i128> {
        self.value_const(&key)
            .and_then(|info| info.value.as_int_value())
    }

    fn inference_const_function_alias(&self, key: (FileId, Spur)) -> Option<Spur> {
        self.value_const(&key)
            .and_then(|info| info.value.as_function())
    }

    fn inference_module_binding_type(&self, key: (FileId, Spur)) -> Option<Type> {
        self.module_binding(&key).map(|info| info.ty)
    }

    fn inference_module_file_id(&self, module: ModuleId) -> Option<FileId> {
        ((module.index() as usize) < self.module_registry.len())
            .then(|| self.module_registry.get_def(module).file_id)
    }

    fn inference_function_by_file(&self, key: (FileId, Spur)) -> Option<Spur> {
        self.functions_by_file_name.get(&key).copied()
    }
}
