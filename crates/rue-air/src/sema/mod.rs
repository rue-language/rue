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
    /// Method lookup: maps (struct_id, method_name) to info.
    pub methods: HashMap<(StructId, Spur), MethodInfo>,
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
            anon_struct_method_sigs: HashMap::new(),
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
    /// Method signatures with InferType: (struct_id, method_name) -> MethodSig.
    pub method_sigs: HashMap<(StructId, Spur), crate::inference::MethodSig>,
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
#[derive(Debug, Clone, Copy)]
pub struct MethodInfo {
    /// The struct type this method belongs to
    pub struct_type: Type,
    /// Whether this is a method (has self) or associated function (no self)
    pub has_self: bool,
    /// Parameter data (names, types, modes, comptime flags) stored in arena.
    /// Access via `arena.names(params)`, `arena.types(params)`, etc.
    /// Note: This excludes `self` if present - only explicit parameters.
    pub params: ParamRange,
    /// Return type
    pub return_type: Type,
    /// The RIR instruction ref for the method body
    pub body: rue_rir::InstRef,
    /// Span of the method declaration
    pub span: rue_span::Span,
}

/// Method signature for anonymous struct structural equality comparison.
///
/// This captures only the parts of a method that affect structural equality:
/// method name, whether it has self, parameter types (as symbols), and return type.
/// Method bodies do NOT affect structural equality - only signatures matter.
///
/// Type symbols are stored as Spur (interned strings) rather than resolved Types
/// because at comparison time, `Self` hasn't been resolved to a concrete StructId yet.
/// Two methods using `Self` in the same positions are considered structurally equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnonMethodSig {
    /// Method name
    pub name: Spur,
    /// Whether this is a method (has self) or associated function (no self)
    pub has_self: bool,
    /// Parameter type symbols (excluding self parameter)
    pub param_types: Vec<Spur>,
    /// Return type symbol
    pub return_type: Spur,
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
    /// Method table: maps (struct_id, method_name) to method info
    /// Used for resolving method calls (receiver.method()) and associated
    /// function calls (Type::function())
    /// Using StructId as key enables method lookup for anonymous structs
    /// which don't have stable name symbols.
    pub(crate) methods: HashMap<(StructId, Spur), MethodInfo>,
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
    /// Method signatures for anonymous structs, used for structural equality comparison.
    /// Maps StructId to the list of method signatures (as type symbols, not resolved Types).
    /// This enables comparing anonymous structs by their method signatures before methods
    /// are fully registered.
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
            known: KnownSymbols::new(interner),
            type_pool: TypeInternPool::new(),
            module_registry: crate::sema_context::ModuleRegistry::new(),
            file_paths: HashMap::new(),
            param_arena: ParamArena::new(),
            anon_struct_method_sigs: HashMap::new(),
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

    /// Check if the compilation involves imports (multi-file compilation).
    ///
    /// When imports are present, lazy analysis is used to only analyze
    /// functions reachable from main(). For single-file compilation,
    /// eager analysis is used for backwards compatibility.
    pub(crate) fn has_imports(&self) -> bool {
        !self.module_registry.is_empty()
    }

    /// Check if the accessing file can see a private item from the target file.
    ///
    /// Visibility rules (per ADR-0026):
    /// - `pub` items are always accessible
    /// - Private items are accessible if the files are in the same directory module
    ///
    /// Directory module membership includes:
    /// - Files directly in the directory (e.g., `utils/strings.rue` is in `utils`)
    /// - Facade files for the directory (e.g., `_utils.rue` is in `utils` module)
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

                // Get the "module identity" for each file.
                // For a regular file like `utils/strings.rue`, the module is `utils/`
                // For a facade file like `_utils.rue`, the module is `utils/` (the directory it represents)
                let acc_module = Self::get_module_identity(Path::new(acc));
                let tgt_module = Self::get_module_identity(Path::new(tgt));

                acc_module == tgt_module
            }
            // If either path is unknown, allow access (e.g., synthetic types, single-file mode)
            _ => true,
        }
    }

    /// Get the module identity for a file path.
    ///
    /// - For regular files: returns the parent directory
    /// - For facade files (`_foo.rue`): returns the corresponding directory (`foo/`)
    ///
    /// This allows facade files to be treated as part of their corresponding directory module.
    fn get_module_identity(path: &std::path::Path) -> Option<std::path::PathBuf> {
        let parent = path.parent()?;
        let file_stem = path.file_stem()?.to_str()?;

        // Check if this is a facade file (starts with underscore)
        if file_stem.starts_with('_') {
            // Facade file: _utils.rue -> parent/utils
            let module_name = &file_stem[1..]; // Strip the leading underscore
            Some(parent.join(module_name))
        } else {
            // Regular file: the module is just the parent directory
            Some(parent.to_path_buf())
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

        // Phase 2.5: Evaluate const initializers (e.g., const x = @import(...))
        // This determines the types of constants before function body analysis.
        self.evaluate_const_initializers()
            .map_err(CompileErrors::from)?;

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
            // Pass references to functions/methods/constants instead of cloning.
            // This is safe because after declaration gathering, these HashMaps are immutable.
            functions: &self.functions,
            methods: &self.methods,
            constants: &self.constants,
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
        let method_sigs: HashMap<(StructId, Spur), MethodSig> = self
            .methods
            .iter()
            .map(|((struct_id, method_name), info)| {
                (
                    (*struct_id, *method_name),
                    MethodSig {
                        struct_type: info.struct_type,
                        has_self: info.has_self,
                        param_types: self
                            .param_arena
                            .types(info.params)
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

    /// Find an existing anonymous struct with the same fields and methods, or create a new one.
    ///
    /// This implements structural type equality for anonymous structs: two anonymous
    /// structs with the same field names/types (in the same order) AND the same method
    /// signatures are the same type. Method bodies do NOT affect structural equality.
    ///
    /// Returns a tuple of (Type, is_new) where is_new indicates whether the struct was
    /// newly created (true) or an existing match was found (false). Callers should only
    /// register methods for newly created structs.
    pub(crate) fn find_or_create_anon_struct(
        &mut self,
        fields: &[StructField],
        method_sigs: &[AnonMethodSig],
    ) -> (Type, bool) {
        // Check if an equivalent anonymous struct already exists
        // Anonymous structs have names starting with "__anon_struct_"
        for struct_id in self.type_pool.all_struct_ids() {
            let struct_def = self.type_pool.struct_def(struct_id);
            if struct_def.name.starts_with("__anon_struct_") {
                // Check fields match
                if struct_def.fields.len() != fields.len() {
                    continue;
                }
                let mut fields_match = true;
                for (def_field, new_field) in struct_def.fields.iter().zip(fields.iter()) {
                    if def_field.name != new_field.name || def_field.ty != new_field.ty {
                        fields_match = false;
                        break;
                    }
                }
                if !fields_match {
                    continue;
                }

                // Check method signatures match
                let existing_sigs = self
                    .anon_struct_method_sigs
                    .get(&struct_id)
                    .map(|v| v.as_slice())
                    .unwrap_or(&[]);
                if existing_sigs.len() != method_sigs.len() {
                    continue;
                }
                let mut methods_match = true;
                for (existing, new) in existing_sigs.iter().zip(method_sigs.iter()) {
                    if existing != new {
                        methods_match = false;
                        break;
                    }
                }
                if methods_match {
                    // Found a matching struct - return it with is_new=false
                    return (Type::Struct(struct_id), false);
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

        // Store method signatures for future structural equality checks
        if !method_sigs.is_empty() {
            self.anon_struct_method_sigs
                .insert(struct_id, method_sigs.to_vec());
        }

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

        // Return with is_new=true
        (Type::Struct(struct_id), true)
    }

    /// Resolve an enum type through a module reference.
    ///
    /// Used for qualified enum paths like `module.EnumName::Variant` in match patterns.
    /// Checks visibility: private enums are only accessible from the same directory.
    pub fn resolve_enum_through_module(
        &self,
        _module_ref: rue_rir::InstRef,
        type_name: lasso::Spur,
        span: rue_span::Span,
    ) -> rue_error::CompileResult<EnumId> {
        use rue_error::{CompileError, ErrorKind};

        let type_name_str = self.interner.resolve(&type_name);

        // Try to find the enum globally
        let enum_id = self.enums.get(&type_name).copied().ok_or_else(|| {
            CompileError::new(ErrorKind::UnknownEnumType(type_name_str.to_string()), span)
        })?;

        // Check visibility
        let enum_def = self.type_pool.enum_def(enum_id);
        let accessing_file_id = span.file_id;
        let target_file_id = enum_def.file_id;

        if !self.is_accessible(accessing_file_id, target_file_id, enum_def.is_pub) {
            return Err(CompileError::new(
                ErrorKind::PrivateMemberAccess {
                    item_kind: "enum".to_string(),
                    name: type_name_str.to_string(),
                },
                span,
            ));
        }

        Ok(enum_id)
    }

    /// Evaluate const initializers to determine their types.
    ///
    /// This is Phase 2.5 of semantic analysis, called after declaration gathering
    /// but before function body analysis. It handles:
    ///
    /// - `const x = @import("module")` - evaluates to Type::Module
    /// - Other const initializers are left with placeholder types for now
    ///
    /// This enables module re-exports where a const holds an imported module
    /// that can be accessed via dot notation.
    pub fn evaluate_const_initializers(&mut self) -> rue_error::CompileResult<()> {
        use rue_error::{CompileError, ErrorKind};

        // Collect const names to iterate (avoid borrowing issues)
        let const_names: Vec<lasso::Spur> = self.constants.keys().copied().collect();

        for name in const_names {
            let const_info = self.constants.get(&name).unwrap();
            let init_ref = const_info.init;
            let span = const_info.span;

            // Check if the init expression is an @import intrinsic
            let inst = self.rir.get(init_ref);
            if let rue_rir::InstData::Intrinsic {
                name: intrinsic_name,
                args_start,
                args_len,
            } = &inst.data
            {
                let intrinsic_name_str = self.interner.resolve(intrinsic_name);
                if intrinsic_name_str == "import" {
                    // This is an @import - evaluate it at compile time
                    let result = self.evaluate_import_intrinsic(*args_start, *args_len, span)?;

                    // Update the const type to the module type
                    if let Some(const_info_mut) = self.constants.get_mut(&name) {
                        const_info_mut.ty = result;
                    }
                }
            }
        }

        Ok(())
    }

    /// Evaluate an @import intrinsic call at compile time.
    ///
    /// This is used during const initializer evaluation to resolve module imports.
    fn evaluate_import_intrinsic(
        &mut self,
        args_start: u32,
        args_len: u32,
        span: rue_span::Span,
    ) -> rue_error::CompileResult<Type> {
        use rue_error::{CompileError, ErrorKind};

        // @import takes exactly one argument
        if args_len != 1 {
            return Err(CompileError::new(
                ErrorKind::IntrinsicWrongArgCount {
                    name: "import".to_string(),
                    expected: 1,
                    found: args_len as usize,
                },
                span,
            ));
        }

        // Get the argument from extra data (intrinsics use inst_refs, not call_args)
        let arg_refs = self.rir.get_inst_refs(args_start, args_len);
        let arg_inst = self.rir.get(arg_refs[0]);

        // The argument must be a string literal
        let import_path = match &arg_inst.data {
            rue_rir::InstData::StringConst(path_spur) => {
                self.interner.resolve(path_spur).to_string()
            }
            _ => {
                return Err(CompileError::new(
                    ErrorKind::ImportRequiresStringLiteral,
                    arg_inst.span,
                ));
            }
        };

        // Resolve the import path
        let resolved_path = self.resolve_import_path_for_const(&import_path, span)?;

        // Register the module
        let (module_id, _is_new) = self
            .module_registry
            .get_or_create(import_path, resolved_path);

        Ok(Type::Module(module_id))
    }

    /// Resolve an import path for const evaluation.
    ///
    /// This is a simplified version of `resolve_import_path` that works
    /// during the const evaluation phase before full analysis.
    fn resolve_import_path_for_const(
        &self,
        import_path: &str,
        span: rue_span::Span,
    ) -> rue_error::CompileResult<String> {
        use rue_error::{CompileError, ErrorKind};
        use std::path::Path;

        // Check for standard library import
        if import_path == "std" {
            // For now, std is not supported during const eval
            return Err(CompileError::new(
                ErrorKind::ModuleNotFound {
                    path: import_path.to_string(),
                    candidates: vec![],
                },
                span,
            ));
        }

        // Check if the import path matches an already-loaded file
        let import_base = import_path.strip_suffix(".rue").unwrap_or(import_path);
        let import_with_rue = format!("{}.rue", import_base);

        for (_file_id, file_path) in &self.file_paths {
            // Check for exact match
            if file_path == import_path {
                return Ok(file_path.clone());
            }

            // Check if file path ends with import_path.rue (e.g., "utils/strings" matches ".../utils/strings.rue")
            if file_path.ends_with(&import_with_rue) {
                return Ok(file_path.clone());
            }

            // Check if the file path ends with the import path (e.g., "utils/strings.rue" matches)
            if file_path.ends_with(import_path) {
                return Ok(file_path.clone());
            }

            // For imports like "math" or "math.rue", check if the file is named accordingly
            let file_name = Path::new(file_path).file_stem().and_then(|s| s.to_str());
            if let Some(name) = file_name {
                if name == import_base {
                    return Ok(file_path.clone());
                }
                // Also check for _foo.rue (directory module entry point)
                if name == format!("_{}", import_base) {
                    return Ok(file_path.clone());
                }
            }
        }

        // Module not found - collect candidates for error message
        let candidates: Vec<String> = self.file_paths.values().map(|p| p.clone()).collect();
        Err(CompileError::new(
            ErrorKind::ModuleNotFound {
                path: import_path.to_string(),
                candidates,
            },
            span,
        ))
    }
}

#[cfg(test)]
mod consistency_tests;
#[cfg(test)]
mod tests;
