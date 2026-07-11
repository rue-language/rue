//! Output from the declaration gathering phase.
//!
//! This module contains the [`GatherOutput`] struct which holds the state built
//! during declaration gathering that is needed for function body analysis.

use std::collections::HashMap;

use lasso::{Spur, ThreadedRodeo};
use rue_error::PreviewFeatures;
use rue_rir::Rir;
use rue_span::FileId;
use rue_target::Target;

use crate::intern_pool::TypeInternPool;
use crate::param_arena::ParamArena;
use crate::types::{EnumId, StructId};

use super::info::{ConstInfo, FunctionInfo, MethodInfo};
use super::{KnownSymbols, Sema};

/// Output from the declaration gathering phase.
///
/// This contains the state built during declaration gathering that is needed
/// for function body analysis. After gathering, this can be converted back
/// into a `Sema` for demand-driven function-body analysis.
///
/// # Architecture
///
/// The separation of declaration gathering from body analysis enables:
/// 1. **Independent body analysis** - Each function body has an explicit analysis boundary
/// 2. **Clearer architecture** - Separation of concerns
/// 3. **Foundation for incremental** - Gathered declarations can become cacheable inputs
/// 4. **Better error recovery** - One function's error doesn't block others
///
/// # Usage
///
/// ```ignore
/// // Option A: Simple path - all-in-one analysis
/// let sema = Sema::new(rir, interner, preview);
/// let output = sema.analyze_all()?;
///
/// // Option B: Split path - gather declarations, then analyze bodies
/// let gather = sema.gather_declarations()?;
/// let sema = gather.into_sema();
/// let output = sema.analyze_all()?;
/// ```
#[derive(Debug)]
pub struct GatherOutput<'a> {
    /// Reference to the RIR being analyzed.
    pub rir: &'a Rir,
    /// Reference to the string interner.
    pub interner: &'a ThreadedRodeo,
    /// Compatibility struct lookup: maps globally unique struct name symbols to StructId.
    pub structs: HashMap<Spur, StructId>,
    /// Module-local struct lookup: maps (defining file, source name) to StructId.
    pub structs_by_file_name: HashMap<(FileId, Spur), StructId>,
    /// Compatibility enum lookup: maps globally unique enum name symbols to EnumId.
    pub enums: HashMap<Spur, EnumId>,
    /// Module-local enum lookup: maps (defining file, source name) to EnumId.
    pub enums_by_file_name: HashMap<(FileId, Spur), EnumId>,
    /// Function lookup: maps function name to info.
    pub functions: HashMap<Spur, FunctionInfo>,
    /// Source-level function lookup keyed by defining file and source name.
    pub functions_by_file_name: HashMap<(rue_span::FileId, Spur), Spur>,
    /// Internal function key -> source-level function name.
    pub function_source_names: HashMap<Spur, Spur>,
    /// Method lookup: maps (struct_id, method_name) to info.
    pub methods: HashMap<(StructId, Spur), MethodInfo>,
    /// Compatibility value-constant lookup for globally unique bare names.
    pub constants: HashMap<Spur, ConstInfo>,
    /// File-qualified value-constant lookup.
    pub constants_by_file_name: HashMap<(rue_span::FileId, Spur), ConstInfo>,
    /// Module-binding constants (`const m = @import(...)`), keyed by the
    /// declaring file — per-file scoped (RUE-113).
    pub module_bindings: HashMap<(rue_span::FileId, Spur), ConstInfo>,
    /// Enabled preview features.
    pub preview_features: PreviewFeatures,
    /// Requested compilation target.
    pub target: Target,
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
            functions_by_file_name: self.functions_by_file_name,
            function_source_names: self.function_source_names,
            structs: self.structs,
            structs_by_file_name: self.structs_by_file_name,
            enums: self.enums,
            enums_by_file_name: self.enums_by_file_name,
            methods: self.methods,
            constants: self.constants,
            constants_by_file_name: self.constants_by_file_name,
            module_bindings: self.module_bindings,
            preview_features: self.preview_features,
            target: self.target,
            builtin_string_id: self.builtin_string_id,
            builtin_arch_id: self.builtin_arch_id,
            builtin_os_id: self.builtin_os_id,
            known: KnownSymbols::new(self.interner),
            type_pool: self.type_pool,
            module_registry: crate::module_registry::ModuleRegistry::new(),
            file_paths: HashMap::new(),
            param_arena: self.param_arena,
            anon_struct_method_sigs: HashMap::new(),
            anon_struct_captured_values: HashMap::new(),
            anon_struct_type_subst: HashMap::new(),
            destructor_spans: HashMap::new(),
            infectious_linear: HashMap::new(),
            comptime_type_call_depth: 0,
            fn_signatures_in_flight: std::collections::HashSet::new(),
        }
    }
}
