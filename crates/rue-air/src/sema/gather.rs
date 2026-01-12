//! Output from the declaration gathering phase.
//!
//! This module contains the [`GatherOutput`] struct which holds the state built
//! during declaration gathering that is needed for function body analysis.

use std::collections::HashMap;

use lasso::{Spur, ThreadedRodeo};
use rue_error::PreviewFeatures;
use rue_rir::Rir;

use crate::intern_pool::TypeInternPool;
use crate::param_arena::ParamArena;
use crate::types::{EnumId, StructId};

use super::info::{ConstInfo, FunctionInfo, MethodInfo};
use super::{KnownSymbols, Sema};

/// Output from the declaration gathering phase.
///
/// This contains the state built during declaration gathering that is needed
/// for function body analysis. After gathering, this can be converted back
/// into a `Sema` for sequential analysis, or used to drive parallel analysis.
///
/// # Architecture
///
/// The separation of declaration gathering from body analysis enables:
/// 1. **Parallel type checking** - Each function can be analyzed independently
/// 2. **Clearer architecture** - Separation of concerns
/// 3. **Foundation for incremental** - Can cache SemaContext across compilations
/// 4. **Better error recovery** - One function's error doesn't block others
///
/// # Usage
///
/// ```ignore
/// // Option A: Simple path - all-in-one analysis
/// let sema = Sema::new(rir, interner, preview);
/// let output = sema.analyze_all()?;
///
/// // Option B: Parallel path (work in progress)
/// // Build SemaContext and analyze in parallel
/// let ctx = sema.build_sema_context();
/// let results: Vec<_> = functions.par_iter()
///     .map(|f| analyze_function_body(&ctx, f))
///     .collect();
/// ```
#[derive(Debug)]
pub struct GatherOutput<'a> {
    /// Reference to the RIR being analyzed.
    pub rir: &'a Rir,
    /// Reference to the string interner.
    pub interner: &'a ThreadedRodeo,
    /// Struct lookup: maps struct name symbol to StructId.
    pub structs: HashMap<Spur, StructId>,
    /// Enum lookup: maps enum name symbol to EnumId.
    pub enums: HashMap<Spur, EnumId>,
    /// Function lookup: maps function name to info.
    pub functions: HashMap<Spur, FunctionInfo>,
    /// Method lookup: maps (struct_id, method_name) to info.
    pub methods: HashMap<(StructId, Spur), MethodInfo>,
    /// Constant lookup: maps const name to info.
    pub constants: HashMap<Spur, ConstInfo>,
    /// Enabled preview features.
    pub preview_features: PreviewFeatures,
    /// StructId of the synthetic String type.
    pub builtin_string_id: Option<StructId>,
    /// EnumId of the synthetic Arch enum (for @target_arch intrinsic).
    pub builtin_arch_id: Option<EnumId>,
    /// EnumId of the synthetic Os enum (for @target_os intrinsic).
    pub builtin_os_id: Option<EnumId>,
    /// Type intern pool (ADR-0024 Phase 1).
    pub type_pool: TypeInternPool,
    /// Arena storage for function/method parameter data.
    pub param_arena: ParamArena,
}

impl<'a> GatherOutput<'a> {
    /// Convert this gather output back into a Sema for function body analysis.
    ///
    /// This is used for sequential analysis. The returned Sema has all
    /// declarations already collected and is ready to analyze function bodies.
    pub fn into_sema(self) -> Sema<'a> {
        Sema {
            rir: self.rir,
            interner: self.interner,
            functions: self.functions,
            structs: self.structs,
            enums: self.enums,
            methods: self.methods,
            constants: self.constants,
            preview_features: self.preview_features,
            builtin_string_id: self.builtin_string_id,
            builtin_arch_id: self.builtin_arch_id,
            builtin_os_id: self.builtin_os_id,
            known: KnownSymbols::new(self.interner),
            type_pool: self.type_pool,
            module_registry: crate::sema_context::ModuleRegistry::new(),
            file_paths: HashMap::new(),
            param_arena: self.param_arena,
            anon_struct_method_sigs: HashMap::new(),
            anon_struct_captured_values: HashMap::new(),
        }
    }
}
