//! Semantic analysis - RIR to AIR conversion.
//!
//! Sema performs type checking and converts untyped RIR to typed AIR.
//! This is analogous to Zig's Sema phase.
//!
//! # Module Organization
//!
//! This module is split into several submodules for maintainability:
//!
//! - [`context`] - Analysis context and helper types (LocalVar, AnalysisContext, etc.)
//! - [`declarations`] - Declaration gathering (register_type_names, resolve_declarations)
//! - [`builtins`] - Built-in type injection (String, etc.)
//! - [`typeck`] - Type resolution and checking helpers
//! - [`analysis`] - Function analysis, type inference coordination, and RIR-to-AIR lowering
//! - [`info`] - Function, method, and constant info types
//! - [`gather`] - Declaration gathering output
//! - [`output`] - Semantic analysis output types
//! - [`inference_ctx`] - Pre-computed type information for inference
//! - [`visibility`] - Module visibility checking
//! - [`anon_structs`] - Anonymous struct structural equality
//! - [`file_paths`] - File path management for multi-file compilation
//!
//! The main entry points are:
//! - [`Sema::new`] - Create a new semantic analyzer
//! - [`Sema::analyze_all`] - Perform full semantic analysis
//! - [`Sema::analyze_all_bodies`] - Analyze function bodies after declarations

mod analysis;
mod analyze_ops;
mod anon_structs;
mod builtins;
mod comptime_eval;
mod context;
mod declarations;
mod file_paths;
mod gather;
mod inference_ctx;
mod info;
mod known_symbols;
mod module_path;
mod output;
mod sema_ctx_builder;
mod typeck;
mod visibility;

// Public re-exports
pub use context::ConstValue;
pub use gather::GatherOutput;
pub use inference_ctx::InferenceContext;
pub use info::{AnonMethodSig, ConstInfo, FunctionInfo, MethodInfo};
pub use known_symbols::KnownSymbols;
pub use output::{AnalyzedFunction, SemaOutput};

use std::collections::HashMap;

use lasso::{Spur, ThreadedRodeo};
use rue_error::{CompileErrors, MultiErrorResult, PreviewFeatures};
use rue_rir::Rir;
use rue_span::{FileId, Span};
use rue_target::Target;

use crate::intern_pool::TypeInternPool;
use crate::param_arena::ParamArena;
use crate::types::{EnumId, StructId, Type};

/// Semantic analyzer that converts RIR to AIR.
pub struct Sema<'a> {
    pub(crate) rir: &'a Rir,
    pub(crate) interner: &'a ThreadedRodeo,
    /// Function table: maps internal function name symbols to their info.
    ///
    /// The internal key is normally the source name, but functions with the
    /// same source name in distinct files get deterministic module-qualified
    /// keys so they can coexist without colliding in AIR/codegen.
    pub(crate) functions: HashMap<Spur, FunctionInfo>,
    /// Source-level function lookup keyed by defining file and source name.
    pub(crate) functions_by_file_name: HashMap<(FileId, Spur), Spur>,
    /// Internal function key -> source-level function name.
    pub(crate) function_source_names: HashMap<Spur, Spur>,
    /// Struct table: maps struct name symbols to their StructId
    pub(crate) structs: HashMap<Spur, StructId>,
    /// Enum table: maps enum name symbols to their EnumId
    pub(crate) enums: HashMap<Spur, EnumId>,
    /// Method table: maps (struct_id, method_name) to method info
    pub(crate) methods: HashMap<(StructId, Spur), MethodInfo>,
    /// Constant table: maps const name symbol to const info.
    /// Holds value constants only (e.g. `const MAX: i32 = 10`); module bindings
    /// live in [`Self::module_bindings`].
    pub(crate) constants: HashMap<Spur, ConstInfo>,
    /// Module-binding constants (`const utils = @import("...")`), keyed by
    /// the declaring file. Unlike value constants, module bindings are
    /// per-file scoped (ADR-0026): every file writes its own imports, so two
    /// files binding the same name — even to different modules — must not
    /// collide (RUE-113).
    pub(crate) module_bindings: HashMap<(FileId, Spur), ConstInfo>,
    /// Enabled preview features
    pub(crate) preview_features: PreviewFeatures,
    /// Requested compilation target.
    pub(crate) target: Target,
    /// StructId of the synthetic String type.
    pub(crate) builtin_string_id: Option<StructId>,
    /// EnumId of the synthetic Arch enum (for @target_arch intrinsic).
    pub(crate) builtin_arch_id: Option<EnumId>,
    /// EnumId of the synthetic Os enum (for @target_os intrinsic).
    pub(crate) builtin_os_id: Option<EnumId>,
    /// Pre-interned known symbols for fast comparison.
    pub(crate) known: KnownSymbols,
    /// Type intern pool for unified type representation (ADR-0024 Phase 1).
    pub(crate) type_pool: TypeInternPool,
    /// Module registry for tracking imported modules (Phase 1 modules).
    pub(crate) module_registry: crate::module_registry::ModuleRegistry,
    /// Maps FileId to source file paths (for module resolution).
    pub(crate) file_paths: HashMap<FileId, String>,
    /// Arena storage for function/method parameter data.
    pub(crate) param_arena: ParamArena,
    /// Method signatures for anonymous structs, used for structural equality comparison.
    pub(crate) anon_struct_method_sigs: HashMap<StructId, Vec<AnonMethodSig>>,
    /// Captured comptime values for anonymous structs.
    /// When an anonymous struct with methods is created inside a comptime function,
    /// the comptime parameter values (e.g., N=42 in FixedBuffer(comptime N: i32)) are
    /// stored here, keyed by StructId. These values become part of type identity:
    /// FixedBuffer(42) and FixedBuffer(100) are different types.
    pub(crate) anon_struct_captured_values: HashMap<StructId, HashMap<Spur, ConstValue>>,
    /// Captured comptime *type* substitution for anonymous structs produced by
    /// a `-> type` comptime constructor. When `Vec(comptime T: type)` is
    /// instantiated as `Vec(i32)`, this stores `T -> i32` keyed by the
    /// resulting StructId, so the enclosing type parameter resolves not just in
    /// field/method *signatures* (done at registration) but throughout every
    /// method *body* at analysis time (RUE-313). Empty for non-generic anon
    /// structs.
    pub(crate) anon_struct_type_subst: HashMap<StructId, HashMap<Spur, Type>>,
    /// Span of each user-defined `drop fn` declaration, keyed by the struct
    /// it destructs. Used by diagnostics that point at the destructor
    /// (E0456 field-move-out-of-destructor-type, E0457 @copy-with-destructor).
    /// Builtin destructors (e.g. String's) have no entry.
    pub(crate) destructor_spans: HashMap<StructId, Span>,
    /// Structs that became linear via infectious linearity (RUE-40): not
    /// declared `linear`, but containing a field that carries a linear value.
    /// Maps the struct to the (field name, field type name) that caused it,
    /// for diagnostics explaining why the container is linear.
    pub(crate) infectious_linear: HashMap<StructId, (String, String)>,
    /// Current recursion depth of `-> type` comptime-function reduction
    /// (`eval_comptime_type_call`). Unlike value functions — whose comptime
    /// recursion is bounded by the specialization-round limit — a `-> type`
    /// function reduces eagerly on the host stack, so an unbounded
    /// self-recursive type constructor (`fn Bad() -> type { Bad() }`) would
    /// overflow that stack (SIGABRT) without this guard. Bounded at
    /// `MAX_SPECIALIZATION_ROUNDS`, emitting the same E1200 (RUE-261).
    pub(crate) comptime_type_call_depth: usize,
}

impl<'a> Sema<'a> {
    /// Create a new semantic analyzer.
    pub fn new(
        rir: &'a Rir,
        interner: &'a ThreadedRodeo,
        preview_features: PreviewFeatures,
    ) -> Self {
        Self::new_for_target(
            rir,
            interner,
            preview_features,
            Target::host()
                .expect("Rue cannot choose a default sema target on this unsupported host"),
        )
    }

    /// Create a new semantic analyzer for an explicit compilation target.
    pub fn new_for_target(
        rir: &'a Rir,
        interner: &'a ThreadedRodeo,
        preview_features: PreviewFeatures,
        target: Target,
    ) -> Self {
        Self {
            rir,
            interner,
            functions: HashMap::new(),
            functions_by_file_name: HashMap::new(),
            function_source_names: HashMap::new(),
            structs: HashMap::new(),
            enums: HashMap::new(),
            methods: HashMap::new(),
            constants: HashMap::new(),
            module_bindings: HashMap::new(),
            preview_features,
            target,
            builtin_string_id: None,
            builtin_arch_id: None,
            builtin_os_id: None,
            known: KnownSymbols::new(interner),
            type_pool: TypeInternPool::new(),
            module_registry: crate::module_registry::ModuleRegistry::new(),
            file_paths: HashMap::new(),
            param_arena: ParamArena::new(),
            anon_struct_method_sigs: HashMap::new(),
            anon_struct_captured_values: HashMap::new(),
            anon_struct_type_subst: HashMap::new(),
            destructor_spans: HashMap::new(),
            infectious_linear: HashMap::new(),
            comptime_type_call_depth: 0,
        }
    }

    /// Perform semantic analysis on the RIR.
    ///
    /// This is the main entry point for semantic analysis. It returns analyzed
    /// functions, struct definitions, enum definitions, and any warnings.
    pub fn analyze_all(mut self) -> MultiErrorResult<SemaOutput> {
        // Phase 0a: Reject a top-level name claimed by two of function /
        // struct / enum, order-independently (spec 10.3:1, 10.5:1, RUE-239).
        // Runs before builtin injection so it scans only user RIR. Value
        // constants are folded in later, after collection separates them from
        // per-file module bindings (see `check_const_cross_kind_collisions`).
        self.check_top_level_name_collisions()
            .map_err(CompileErrors::from)?;

        // Phase 0b: Inject built-in types (String, etc.) before user code
        self.inject_builtin_types();

        // Phase 1: Register type names
        // Phase 2: Resolve all declarations (const initializers — including
        // `const x = @import(...)` module bindings — are evaluated as they
        // are collected, see `collect_const_by_key`)
        self.register_type_names().map_err(CompileErrors::from)?;
        self.resolve_declarations().map_err(CompileErrors::from)?;

        // Delegate to the analysis module for function body analysis
        analysis::analyze_all_function_bodies(self)
    }

    /// Analyze all function bodies, assuming declarations are already collected.
    pub fn analyze_all_bodies(self) -> MultiErrorResult<SemaOutput> {
        analysis::analyze_all_function_bodies(self)
    }
}

#[cfg(test)]
mod consistency_tests;
#[cfg(test)]
mod tests;
