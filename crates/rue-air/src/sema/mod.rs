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
//!
//! The main entry points are:
//! - [`Sema::new`] - Create a new semantic analyzer
//! - [`Sema::analyze_all`] - Perform full semantic analysis
//! - [`Sema::analyze_all_bodies`] - Analyze function bodies after declarations

mod airgen;
mod analysis;
mod analyze_ops;
mod builtins;
mod context;
mod declarations;
mod known_symbols;
mod typeck;

pub use known_symbols::KnownSymbols;

use std::collections::HashMap;

use lasso::{Spur, ThreadedRodeo};
use rue_error::{CompileErrors, MultiErrorResult, PreviewFeatures};
use rue_rir::Rir;
use rue_span::FileId;

use crate::intern_pool::TypeInternPool;
use crate::param_arena::{ParamArena, ParamRange};
use crate::sema_context::{InferenceContext as SemaContextInferenceContext, SemaContext};
use crate::types::{ArrayTypeId, EnumDef, EnumId, StructDef, StructField, StructId, Type};

// Internal types are used via pub(crate) within submodules
// No re-exports needed for context types as they're internal

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
    /// Method lookup: maps (struct_name, method_name) to info.
    pub methods: HashMap<(Spur, Spur), MethodInfo>,
    /// Constant lookup: maps const name to info.
    pub constants: HashMap<Spur, ConstInfo>,
    /// Enabled preview features.
    pub preview_features: PreviewFeatures,
    /// StructId of the synthetic String type.
    pub builtin_string_id: Option<StructId>,
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
            known: KnownSymbols::new(self.interner),
            type_pool: self.type_pool,
            module_registry: crate::sema_context::ModuleRegistry::new(),
            file_paths: HashMap::new(),
            param_arena: self.param_arena,
        }
    }
}

/// Result of analyzing a function.
#[derive(Debug)]
pub struct AnalyzedFunction {
    pub name: String,
    pub air: crate::inst::Air,
    /// Number of local variable slots needed
    pub num_locals: u32,
    /// Number of ABI slots used by parameters.
    /// For scalar types (i32, bool), each parameter uses 1 slot.
    /// For struct types, each field uses 1 slot (flattened ABI).
    pub num_param_slots: u32,
    /// Whether each parameter slot is passed as inout (by reference).
    /// Length matches num_param_slots - for struct params, all slots share
    /// the same mode as the original parameter.
    pub param_modes: Vec<bool>,
}

/// Output from semantic analysis.
///
/// Contains all analyzed functions, struct definitions, enum definitions, and any warnings
/// generated during analysis.
#[derive(Debug)]
pub struct SemaOutput {
    /// Analyzed functions with typed IR.
    pub functions: Vec<AnalyzedFunction>,
    /// String literals indexed by their AIR string_const index.
    pub strings: Vec<String>,
    /// Warnings collected during analysis.
    pub warnings: Vec<rue_error::CompileWarning>,
    /// Type intern pool (contains all types including arrays).
    pub type_pool: TypeInternPool,
}

/// Pre-computed type information for constraint generation.
///
/// This struct holds the function, struct, enum, and method signature maps
/// converted to `InferType` format for use in Hindley-Milner type inference.
/// Building this once and reusing it for all function analyses avoids the
/// O(n²) cost of rebuilding these maps for each function.
///
/// # Performance
///
/// For a program with 100 functions and 50 structs:
/// - **Before**: 100 × (HashMap rebuild + InferType conversions) per analysis
/// - **After**: 1 × (HashMap build + InferType conversions) total
#[derive(Debug)]
pub struct InferenceContext {
    /// Function signatures with InferType (for constraint generation).
    pub func_sigs: HashMap<Spur, crate::inference::FunctionSig>,
    /// Struct types: name -> Type::Struct(id).
    pub struct_types: HashMap<Spur, Type>,
    /// Enum types: name -> Type::Enum(id).
    pub enum_types: HashMap<Spur, Type>,
    /// Method signatures with InferType: (struct_name, method_name) -> MethodSig.
    pub method_sigs: HashMap<(Spur, Spur), crate::inference::MethodSig>,
}

/// Information about a function.
#[derive(Debug, Clone, Copy)]
pub struct FunctionInfo {
    /// Parameter data (names, types, modes, comptime flags) stored in arena.
    /// Access via `arena.names(params)`, `arena.types(params)`, etc.
    pub params: ParamRange,
    /// Return type
    pub return_type: Type,
    /// The return type symbol (before resolution) - needed for generic function specialization
    pub return_type_sym: Spur,
    /// The RIR instruction ref for the function body - needed for generic function specialization
    pub body: rue_rir::InstRef,
    /// Span of the function declaration
    pub span: rue_span::Span,
    /// Whether this function has any comptime type parameters
    pub is_generic: bool,
    /// Whether this function is public (visible outside its directory)
    pub is_pub: bool,
    /// File ID this function was declared in (for visibility checking)
    pub file_id: rue_span::FileId,
}

/// Information about a method in an impl block.
#[derive(Debug, Clone)]
pub struct MethodInfo {
    /// The struct type this method belongs to
    pub struct_type: Type,
    /// Whether this is a method (has self) or associated function (no self)
    pub has_self: bool,
    /// Parameter names (excluding self if present)
    pub param_names: Vec<Spur>,
    /// Parameter types (excluding self if present)
    pub param_types: Vec<Type>,
    /// Return type
    pub return_type: Type,
    /// The RIR instruction ref for the method body
    pub body: rue_rir::InstRef,
    /// Span of the method declaration
    pub span: rue_span::Span,
}

/// Information about a constant declaration.
///
/// Constants are compile-time values. In the module system, they're primarily
/// used for re-exports:
/// ```rue
/// pub const strings = @import("utils/strings.rue");
/// pub const helper = @import("utils/internal.rue").helper;
/// ```
#[derive(Debug, Clone)]
pub struct ConstInfo {
    /// Whether this constant is public
    pub is_pub: bool,
    /// The type of the constant value (e.g., Type::Module for imports)
    pub ty: Type,
    /// The RIR instruction ref for the initializer
    pub init: rue_rir::InstRef,
    /// Span of the const declaration
    pub span: rue_span::Span,
}

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
    /// Method table: maps (struct_name, method_name) to method info
    /// Used for resolving method calls (receiver.method()) and associated
    /// function calls (Type::function())
    pub(crate) methods: HashMap<(Spur, Spur), MethodInfo>,
    /// Constant table: maps const name symbol to const info
    /// Used for module re-exports: `pub const strings = @import("utils/strings");`
    pub(crate) constants: HashMap<Spur, ConstInfo>,
    /// Enabled preview features
    pub(crate) preview_features: PreviewFeatures,
    /// StructId of the synthetic String type.
    /// This is populated during `inject_builtin_types()` and used for quick lookups.
    pub(crate) builtin_string_id: Option<StructId>,
    /// Pre-interned known symbols for fast comparison.
    pub(crate) known: KnownSymbols,
    /// Type intern pool for unified type representation (ADR-0024 Phase 1).
    ///
    /// During Phase 1, the pool coexists with the existing type registries.
    /// It is populated during declaration collection but not yet used for
    /// type operations. Later phases will migrate to using the pool exclusively.
    pub(crate) type_pool: TypeInternPool,
    /// Module registry for tracking imported modules (Phase 1 modules).
    pub(crate) module_registry: crate::sema_context::ModuleRegistry,
    /// Maps FileId to source file paths (for module resolution).
    /// Used to resolve relative imports when compiling multiple files.
    pub(crate) file_paths: HashMap<FileId, String>,
    /// Arena storage for function/method parameter data.
    /// FunctionInfo and MethodInfo store ParamRange handles into this arena.
    pub(crate) param_arena: ParamArena,
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
            known: KnownSymbols::new(interner),
            type_pool: TypeInternPool::new(),
            module_registry: crate::sema_context::ModuleRegistry::new(),
            file_paths: HashMap::new(),
            param_arena: ParamArena::new(),
        }
    }

    /// Set file paths for module resolution in multi-file compilation.
    ///
    /// This maps FileIds to their corresponding source file paths,
    /// enabling relative import resolution during @import.
    pub fn set_file_paths(&mut self, file_paths: HashMap<FileId, String>) {
        self.file_paths = file_paths;
    }

    /// Get the source file path for a span.
    ///
    /// Looks up the file path using the span's file_id.
    pub(crate) fn get_source_path(&self, span: rue_span::Span) -> Option<&str> {
        self.file_paths.get(&span.file_id).map(|s| s.as_str())
    }

    /// Get the file path for a given FileId.
    pub(crate) fn get_file_path(&self, file_id: FileId) -> Option<&str> {
        self.file_paths.get(&file_id).map(|s| s.as_str())
    }

    /// Check if the accessing file can see a private item from the target file.
    ///
    /// Visibility rules (per ADR-0026):
    /// - `pub` items are always accessible
    /// - Private items are accessible if the files are in the same directory
    ///
    /// Returns true if the item is accessible.
    pub(crate) fn is_accessible(
        &self,
        accessing_file_id: FileId,
        target_file_id: FileId,
        is_pub: bool,
    ) -> bool {
        // Public items are always accessible
        if is_pub {
            return true;
        }

        // Get paths for both files
        let accessing_path = self.get_file_path(accessing_file_id);
        let target_path = self.get_file_path(target_file_id);

        // If we can't determine the paths, be permissive (for single-file mode or tests)
        match (accessing_path, target_path) {
            (Some(acc), Some(tgt)) => {
                use std::path::Path;
                let acc_dir = Path::new(acc).parent();
                let tgt_dir = Path::new(tgt).parent();
                // Same directory means accessible
                acc_dir == tgt_dir
            }
            // If either path is unknown, allow access (e.g., synthetic types, single-file mode)
            _ => true,
        }
    }

    /// Perform semantic analysis on the RIR.
    ///
    /// This is the main entry point for semantic analysis. It returns analyzed
    /// functions, struct definitions, enum definitions, and any warnings generated during analysis.
    ///
    /// This function collects errors from multiple functions instead of stopping at the
    /// first error, allowing users to see all issues at once. Errors within type/struct
    /// definitions still cause early termination since they affect all subsequent analysis.
    pub fn analyze_all(mut self) -> MultiErrorResult<SemaOutput> {
        // Phase 0: Inject built-in types (String, etc.) before user code
        // This must happen first so builtins are registered when resolving types.
        self.inject_builtin_types();

        // Two-phase declaration gathering:
        // Phase 1: Register type names
        // Phase 2: Resolve all declarations
        // These are critical and must succeed before we can analyze functions
        self.register_type_names().map_err(CompileErrors::from)?;
        self.resolve_declarations().map_err(CompileErrors::from)?;

        // Delegate to the analysis module for function body analysis
        analysis::analyze_all_function_bodies(self)
    }

    /// Analyze all function bodies, assuming declarations are already collected.
    ///
    /// This is Phase 2 of semantic analysis. It assumes that declaration gathering
    /// has already been performed (either by `analyze_all()` internally, or manually
    /// via `inject_builtin_types()`, `register_type_names()`, and `resolve_declarations()`).
    ///
    /// # Architecture
    ///
    /// This method is the entry point for function body analysis after declaration
    /// gathering. Currently it runs sequentially, but the design allows for future
    /// parallelization since each function body can be analyzed independently.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Manual phase approach (for testing or custom pipelines):
    /// let mut sema = Sema::new(rir, interner, preview);
    /// sema.inject_builtin_types();
    /// sema.register_type_names()?;
    /// sema.resolve_declarations()?;
    /// let output = sema.analyze_all_bodies()?;
    /// ```
    pub fn analyze_all_bodies(self) -> MultiErrorResult<SemaOutput> {
        // Delegate to the analysis module
        analysis::analyze_all_function_bodies(self)
    }

    /// Build a SemaContext from the current Sema state.
    ///
    /// This creates an immutable context that can be shared across threads
    /// for parallel function body analysis. The SemaContext contains all
    /// type definitions, function signatures, and method signatures.
    ///
    /// The returned SemaContext borrows functions and methods from Sema,
    /// so Sema must outlive the SemaContext. This is reflected in the
    /// lifetime relationship: the returned context lives for `'s` (the
    /// borrow of self), not `'a` (the lifetime of the RIR/interner).
    ///
    /// # Usage
    ///
    /// ```ignore
    /// let mut sema = Sema::new(rir, interner, preview);
    /// sema.inject_builtin_types();
    /// sema.register_type_names()?;
    /// sema.resolve_declarations()?;
    /// let ctx = sema.build_sema_context();
    /// // Now ctx can be shared across threads for parallel analysis
    /// // (while sema is borrowed)
    /// ```
    pub fn build_sema_context<'s>(&'s self) -> SemaContext<'s>
    where
        'a: 's,
    {
        // Build the inference context
        let inference_ctx = self.build_sema_context_inference();

        SemaContext {
            rir: self.rir,
            interner: self.interner,
            structs: self.structs.clone(),
            enums: self.enums.clone(),
            // Pass references to functions/methods instead of cloning.
            // This is safe because after declaration gathering, these HashMaps are immutable.
            functions: &self.functions,
            methods: &self.methods,
            preview_features: self.preview_features.clone(),
            builtin_string_id: self.builtin_string_id,
            inference_ctx,
            known: KnownSymbols::new(self.interner),
            type_pool: self.type_pool.clone(),
            module_registry: crate::sema_context::ModuleRegistry::new(),
            source_file_path: None, // Will be set when analyzing specific files
            file_paths: self.file_paths.clone(), // Copy file paths for module resolution
            param_arena: &self.param_arena,
        }
    }

    /// Build the inference context portion of SemaContext.
    fn build_sema_context_inference(&self) -> SemaContextInferenceContext {
        use crate::inference::{FunctionSig, MethodSig};

        // Build function signatures with InferType for constraint generation
        let func_sigs: HashMap<Spur, FunctionSig> = self
            .functions
            .iter()
            .map(|(name, info)| {
                (
                    *name,
                    FunctionSig {
                        param_types: self
                            .param_arena
                            .types(info.params)
                            .iter()
                            .map(|t| self.type_to_infer_type(*t))
                            .collect(),
                        return_type: self.type_to_infer_type(info.return_type),
                        is_generic: info.is_generic,
                        param_modes: self.param_arena.modes(info.params).to_vec(),
                        param_comptime: self.param_arena.comptime(info.params).to_vec(),
                        param_names: self.param_arena.names(info.params).to_vec(),
                        return_type_sym: info.return_type_sym,
                    },
                )
            })
            .collect();

        // Build struct types map (name -> Type::Struct(id))
        let struct_types: HashMap<Spur, Type> = self
            .structs
            .iter()
            .map(|(name, id)| (*name, Type::Struct(*id)))
            .collect();

        // Build enum types map (name -> Type::Enum(id))
        let enum_types: HashMap<Spur, Type> = self
            .enums
            .iter()
            .map(|(name, id)| (*name, Type::Enum(*id)))
            .collect();

        // Build method signatures with InferType for constraint generation
        let method_sigs: HashMap<(Spur, Spur), MethodSig> = self
            .methods
            .iter()
            .map(|((type_name, method_name), info)| {
                (
                    (*type_name, *method_name),
                    MethodSig {
                        struct_type: info.struct_type,
                        has_self: info.has_self,
                        param_types: info
                            .param_types
                            .iter()
                            .map(|t| self.type_to_infer_type(*t))
                            .collect(),
                        return_type: self.type_to_infer_type(info.return_type),
                    },
                )
            })
            .collect();

        SemaContextInferenceContext {
            func_sigs,
            struct_types,
            enum_types,
            method_sigs,
        }
    }

    /// Find an existing anonymous struct with the same fields, or create a new one.
    ///
    /// This implements structural type equality for anonymous structs: two anonymous
    /// structs with the same field names and types (in the same order) are the same type.
    pub(crate) fn find_or_create_anon_struct(&mut self, fields: &[StructField]) -> Type {
        // Check if an equivalent anonymous struct already exists
        // Anonymous structs have names starting with "__anon_struct_"
        for struct_id in self.type_pool.all_struct_ids() {
            let struct_def = self.type_pool.struct_def(struct_id);
            if struct_def.name.starts_with("__anon_struct_") {
                if struct_def.fields.len() == fields.len() {
                    let mut all_match = true;
                    for (def_field, new_field) in struct_def.fields.iter().zip(fields.iter()) {
                        if def_field.name != new_field.name || def_field.ty != new_field.ty {
                            all_match = false;
                            break;
                        }
                    }
                    if all_match {
                        return Type::Struct(struct_id);
                    }
                }
            }
        }

        // No matching struct found - create a new one
        let anon_name_temp = format!("__anon_struct_temp_{}", self.type_pool.len());
        let name_spur = self.interner.get_or_intern(&anon_name_temp);

        // Determine if the struct is Copy (all fields are Copy)
        let is_copy = fields.iter().all(|f| f.ty.is_copy_in_pool(&self.type_pool));

        let struct_def = StructDef {
            name: anon_name_temp.clone(),
            fields: fields.to_vec(),
            is_copy,
            is_handle: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,                     // Anonymous structs are private
            file_id: rue_span::FileId::new(0), // Anonymous, no source file
        };

        let (struct_id, _) = self.type_pool.register_struct(name_spur, struct_def);

        // Register in struct lookup
        self.structs.insert(name_spur, struct_id);

        // Update the name now that we have the ID
        let final_name = format!("__anon_struct_{}", struct_id.0);
        let final_name_spur = self.interner.get_or_intern(&final_name);

        // Update the struct definition with the correct name
        let mut updated_def = self.type_pool.struct_def(struct_id);
        updated_def.name = final_name.clone();
        self.type_pool.update_struct_def(struct_id, updated_def);

        // Update the struct lookup
        self.structs.remove(&name_spur);
        self.structs.insert(final_name_spur, struct_id);

        Type::Struct(struct_id)
    }
}

#[cfg(test)]
mod consistency_tests;
#[cfg(test)]
mod tests;
