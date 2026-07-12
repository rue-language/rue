//! Output from the declaration gathering phase.
//!
//! This module contains the [`GatherOutput`] struct which holds the state built
//! during declaration gathering that is needed for function body analysis.

use std::collections::HashMap;

use lasso::{Spur, ThreadedRodeo};
use rue_error::PreviewFeatures;
use rue_rir::{InstData, Rir};
use rue_span::FileId;
use rue_target::Target;

use crate::intern_pool::TypeInternPool;
use crate::param_arena::ParamArena;
use crate::types::{EnumId, StructId};

use super::info::{ConstInfo, FunctionInfo, MethodInfo};
use super::{KnownSymbols, Sema};

/// Rebuild private named-method declaration identities for the legacy split
/// gather/analysis adapter. The adapter has no public constructor today, but
/// keeping this deterministic RIR-order rebuild prevents its public
/// `into_sema` compatibility path from falling back to body-time scans.
pub(super) fn rebuild_named_method_declarations(
    rir: &Rir,
    structs: &HashMap<Spur, StructId>,
    structs_by_file_name: &HashMap<(FileId, Spur), StructId>,
) -> HashMap<(StructId, Spur), rue_rir::InstRef> {
    let mut declarations = HashMap::new();
    for (_, inst) in rir.iter() {
        let InstData::StructDecl {
            name,
            methods_start,
            methods_len,
            ..
        } = &inst.data
        else {
            continue;
        };
        let Some(&struct_id) = structs_by_file_name
            .get(&(inst.span.file_id, *name))
            .or_else(|| structs.get(name))
        else {
            continue;
        };
        for method_ref in rir.get_inst_refs(*methods_start, *methods_len) {
            if let InstData::FnDecl { name, .. } = rir.get(method_ref).data {
                declarations.insert((struct_id, name), method_ref);
            }
        }
    }
    declarations
}

/// Output from the declaration gathering phase.
///
/// Compatibility carrier for state produced by a declaration-gathering phase.
///
/// Rue's live compiler currently uses [`Sema::analyze_all`] and does not expose
/// a public producer for this type or a public body-only analysis entry point.
/// `GatherOutput` remains public for API compatibility and as the intended
/// boundary for a future split pipeline; [`Self::into_sema`] preserves its
/// stored declaration state without exposing snapshot-local instruction IDs.
///
/// # Architecture
///
/// The separation of declaration gathering from body analysis enables:
/// 1. **Independent body analysis** - Each function body has an explicit analysis boundary
/// 2. **Clearer architecture** - Separation of concerns
/// 3. **Foundation for incremental** - Gathered declarations can become cacheable inputs
/// 4. **Better error recovery** - One function's error doesn't block others
///
/// Callers should use [`Sema::analyze_all`] for the supported end-to-end path.
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
    /// Convert this compatibility state back into a semantic analyzer.
    ///
    /// The returned analyzer retains already-collected declarations. Rue does
    /// not currently expose a public body-only driver, so this conversion is
    /// not itself a supported replacement for [`Sema::analyze_all`].
    pub fn into_sema(self) -> Sema<'a> {
        let named_method_declarations =
            rebuild_named_method_declarations(self.rir, &self.structs, &self.structs_by_file_name);
        Sema {
            rir: self.rir,
            interner: self.interner,
            declaration_index: super::declaration_index::RirDeclarationIndex::new(self.rir),
            functions: self.functions,
            functions_by_file_name: self.functions_by_file_name,
            function_source_names: self.function_source_names,
            structs: self.structs,
            structs_by_file_name: self.structs_by_file_name,
            enums: self.enums,
            enums_by_file_name: self.enums_by_file_name,
            methods: self.methods,
            named_method_declarations,
            body_analysis_work: super::BodyAnalysisWork::default(),
            ordinary_free_function_dependencies: Vec::new(),
            specialized_free_function_origins: Vec::new(),
            specialized_free_function_dependencies: Vec::new(),
            named_method_dependencies: Vec::new(),
            non_generic_named_method_dependencies_complete: true,
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
            symbol_paths: HashMap::new(),
            root_file_id: None,
            param_arena: self.param_arena,
            anon_struct_method_sigs: HashMap::new(),
            anon_struct_captured_values: HashMap::new(),
            anon_struct_type_subst: HashMap::new(),
            destructor_spans: HashMap::new(),
            infectious_linear: HashMap::new(),
            comptime_type_call_depth: 0,
            fn_signatures_in_flight: std::collections::HashSet::new(),
            ctor_type_displays: HashMap::new(),
        }
    }
}
