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
//! - [`analysis`] - Function analysis and type inference coordination
//! - [`airgen`] - RIR instruction to AIR instruction lowering
//! - [`info`] - Function, method, and constant info types
//! - [`gather`] - Declaration gathering output
//! - [`output`] - Semantic analysis output types
//! - [`inference_ctx`] - Pre-computed type information for inference
//! - [`visibility`] - Module visibility checking
//! - [`imports`] - Import resolution and const evaluation
//! - [`anon_structs`] - Anonymous struct structural equality
//! - [`sema_ctx_builder`] - SemaContext builder for parallel analysis
//! - [`file_paths`] - File path management for multi-file compilation
//!
//! The main entry points are:
//! - [`Sema::new`] - Create a new semantic analyzer
//! - [`Sema::analyze_all`] - Perform full semantic analysis
//! - [`Sema::analyze_all_bodies`] - Analyze function bodies after declarations

mod airgen;
mod analysis;
mod analyze_ops;
mod anon_structs;
mod builtins;
mod context;
mod declarations;
mod file_paths;
mod gather;
mod imports;
mod inference_ctx;
mod info;
mod known_symbols;
mod output;
mod sema_ctx_builder;
mod typeck;
mod visibility;

// Public re-exports
pub use gather::GatherOutput;
pub use inference_ctx::InferenceContext;
pub use info::{AnonMethodSig, ConstInfo, FunctionInfo, MethodInfo};
pub use known_symbols::KnownSymbols;
pub use output::{AnalyzedFunction, SemaOutput};

use std::collections::HashMap;

use lasso::{Spur, ThreadedRodeo};
use rue_error::{CompileErrors, MultiErrorResult, PreviewFeatures};
use rue_rir::Rir;
use rue_span::FileId;

use crate::intern_pool::TypeInternPool;
use crate::param_arena::ParamArena;
use crate::types::{EnumId, StructId};

/// Semantic analyzer that converts RIR to AIR.
pub struct Sema<'a> {
    pub(crate) rir: &'a Rir,
    pub(crate) interner: &'a ThreadedRodeo,
    /// Function table: maps function name symbols to their info
    pub(crate) functions: HashMap<Spur, FunctionInfo>,
    /// Struct table: maps struct name symbols to their StructId
    pub(crate) structs: HashMap<Spur, StructId>,
    /// Enum table: maps enum name symbols to their EnumId
    pub(crate) enums: HashMap<Spur, EnumId>,
    /// Method table: maps (struct_id, method_name) to method info
    pub(crate) methods: HashMap<(StructId, Spur), MethodInfo>,
    /// Constant table: maps const name symbol to const info
    pub(crate) constants: HashMap<Spur, ConstInfo>,
    /// Enabled preview features
    pub(crate) preview_features: PreviewFeatures,
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
    pub(crate) module_registry: crate::sema_context::ModuleRegistry,
    /// Maps FileId to source file paths (for module resolution).
    pub(crate) file_paths: HashMap<FileId, String>,
    /// Arena storage for function/method parameter data.
    pub(crate) param_arena: ParamArena,
    /// Method signatures for anonymous structs, used for structural equality comparison.
    pub(crate) anon_struct_method_sigs: HashMap<StructId, Vec<AnonMethodSig>>,
}

impl<'a> Sema<'a> {
    /// Create a new semantic analyzer.
    pub fn new(
        rir: &'a Rir,
        interner: &'a ThreadedRodeo,
        preview_features: PreviewFeatures,
    ) -> Self {
        Self {
            rir,
            interner,
            functions: HashMap::new(),
            structs: HashMap::new(),
            enums: HashMap::new(),
            methods: HashMap::new(),
            constants: HashMap::new(),
            preview_features,
            builtin_string_id: None,
            builtin_arch_id: None,
            builtin_os_id: None,
            known: KnownSymbols::new(interner),
            type_pool: TypeInternPool::new(),
            module_registry: crate::sema_context::ModuleRegistry::new(),
            file_paths: HashMap::new(),
            param_arena: ParamArena::new(),
            anon_struct_method_sigs: HashMap::new(),
        }
    }

    /// Perform semantic analysis on the RIR.
    ///
    /// This is the main entry point for semantic analysis. It returns analyzed
    /// functions, struct definitions, enum definitions, and any warnings.
    pub fn analyze_all(mut self) -> MultiErrorResult<SemaOutput> {
        // Phase 0: Inject built-in types (String, etc.) before user code
        self.inject_builtin_types();

        // Phase 1: Register type names
        // Phase 2: Resolve all declarations
        self.register_type_names().map_err(CompileErrors::from)?;
        self.resolve_declarations().map_err(CompileErrors::from)?;

        // Phase 2.5: Evaluate const initializers (e.g., const x = @import(...))
        self.evaluate_const_initializers()
            .map_err(CompileErrors::from)?;

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
