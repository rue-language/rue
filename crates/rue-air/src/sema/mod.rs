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
//! - [`Sema::gather_declarations`] - Gather declarations for incremental analysis

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

use crate::sema_context::{
    ArrayTypeRegistry, InferenceContext as SemaContextInferenceContext, SemaContext,
};
use crate::types::{ArrayTypeDef, ArrayTypeId, EnumDef, EnumId, StructDef, StructId, Type};

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
/// 3. **Foundation for incremental** - Can cache TypeContext across compilations
/// 4. **Better error recovery** - One function's error doesn't block others
///
/// # Usage
///
/// ```ignore
/// // Phase 1: Gather declarations (sequential)
/// let sema = Sema::new(rir, interner, preview);
/// let (type_ctx, gather_output) = sema.gather_declarations()?;
///
/// // Phase 2: Analyze function bodies
/// // Option A: Sequential (current)
/// let sema = gather_output.into_sema();
/// let output = sema.analyze_all_bodies()?;
///
/// // Option B: Parallel (future)
/// // let results: Vec<_> = functions.par_iter()
/// //     .map(|f| analyze_function_body(&type_ctx, &gather_output, f))
/// //     .collect();
/// ```
#[derive(Debug)]
pub struct GatherOutput<'a> {
    /// Reference to the RIR being analyzed.
    pub rir: &'a Rir,
    /// Reference to the string interner.
    pub interner: &'a ThreadedRodeo,
    /// Struct definitions indexed by StructId.
    pub struct_defs: Vec<StructDef>,
    /// Enum definitions indexed by EnumId.
    pub enum_defs: Vec<EnumDef>,
    /// Array type table: maps (element_type, length) to ArrayTypeId.
    /// Pre-populated during declaration gathering for array types in signatures.
    pub array_types: HashMap<(Type, u64), ArrayTypeId>,
    /// Array type definitions indexed by ArrayTypeId.
    pub array_type_defs: Vec<ArrayTypeDef>,
    /// Struct lookup: maps struct name symbol to StructId.
    pub structs: HashMap<Spur, StructId>,
    /// Enum lookup: maps enum name symbol to EnumId.
    pub enums: HashMap<Spur, EnumId>,
    /// Function lookup: maps function name to info.
    pub functions: HashMap<Spur, FunctionInfo>,
    /// Method lookup: maps (struct_name, method_name) to info.
    pub methods: HashMap<(Spur, Spur), MethodInfo>,
    /// Enabled preview features.
    pub preview_features: PreviewFeatures,
    /// StructId of the synthetic String type.
    pub builtin_string_id: Option<StructId>,
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
            struct_defs: self.struct_defs,
            enums: self.enums,
            enum_defs: self.enum_defs,
            array_types: self.array_types,
            array_type_defs: self.array_type_defs,
            methods: self.methods,
            preview_features: self.preview_features,
            builtin_string_id: self.builtin_string_id,
            known: KnownSymbols::new(self.interner),
        }
    }

    /// Consume the gather output and return ownership of struct and enum definitions.
    ///
    /// This is used after all function analysis is complete to build the final
    /// `SemaOutput`.
    pub fn into_type_defs(self) -> (Vec<StructDef>, Vec<EnumDef>) {
        (self.struct_defs, self.enum_defs)
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
    /// Struct definitions.
    pub struct_defs: Vec<StructDef>,
    /// Enum definitions.
    pub enum_defs: Vec<EnumDef>,
    /// Array type definitions.
    pub array_types: Vec<ArrayTypeDef>,
    /// String literals indexed by their AIR string_const index.
    pub strings: Vec<String>,
    /// Warnings collected during analysis.
    pub warnings: Vec<rue_error::CompileWarning>,
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
#[derive(Debug, Clone)]
pub struct FunctionInfo {
    /// Parameter types (in order)
    pub param_types: Vec<Type>,
    /// Parameter modes (in order)
    pub param_modes: Vec<rue_rir::RirParamMode>,
    /// Return type
    pub return_type: Type,
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

/// Semantic analyzer that converts RIR to AIR.
pub struct Sema<'a> {
    pub(crate) rir: &'a Rir,
    pub(crate) interner: &'a ThreadedRodeo,
    /// Function table: maps function name symbols to their info
    pub(crate) functions: HashMap<Spur, FunctionInfo>,
    /// Struct table: maps struct name symbols to their StructId
    pub(crate) structs: HashMap<Spur, StructId>,
    /// Struct definitions indexed by StructId
    pub(crate) struct_defs: Vec<StructDef>,
    /// Enum table: maps enum name symbols to their EnumId
    pub(crate) enums: HashMap<Spur, EnumId>,
    /// Enum definitions indexed by EnumId
    pub(crate) enum_defs: Vec<EnumDef>,
    /// Array type table: maps (element_type, length) to ArrayTypeId
    pub(crate) array_types: HashMap<(Type, u64), ArrayTypeId>,
    /// Array type definitions indexed by ArrayTypeId
    pub(crate) array_type_defs: Vec<ArrayTypeDef>,
    /// Method table: maps (struct_name, method_name) to method info
    /// Used for resolving method calls (receiver.method()) and associated
    /// function calls (Type::function())
    pub(crate) methods: HashMap<(Spur, Spur), MethodInfo>,
    /// Enabled preview features
    pub(crate) preview_features: PreviewFeatures,
    /// StructId of the synthetic String type.
    /// This is populated during `inject_builtin_types()` and used for quick lookups.
    pub(crate) builtin_string_id: Option<StructId>,
    /// Pre-interned known symbols for fast comparison.
    pub(crate) known: KnownSymbols,
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
            struct_defs: Vec::new(),
            enums: HashMap::new(),
            enum_defs: Vec::new(),
            array_types: HashMap::new(),
            array_type_defs: Vec::new(),
            methods: HashMap::new(),
            preview_features,
            builtin_string_id: None,
            known: KnownSymbols::new(interner),
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

        // Two-phase declaration gathering (see gather_declarations for details):
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
    /// This is Phase 2 of semantic analysis. It assumes that `gather_declarations`
    /// has already been called (or that this Sema was created from `GatherOutput::into_sema`).
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
    /// // Phase 1: Gather declarations
    /// let sema = Sema::new(rir, interner, preview);
    /// let (type_ctx, gather_output) = sema.gather_declarations()?;
    ///
    /// // Phase 2: Analyze function bodies
    /// let sema = gather_output.into_sema();
    /// let output = sema.analyze_all_bodies()?;
    /// ```
    pub fn analyze_all_bodies(self) -> MultiErrorResult<SemaOutput> {
        // Delegate to the analysis module
        analysis::analyze_all_function_bodies(self)
    }

    /// Gather all declarations from the RIR and build a TypeContext.
    ///
    /// This is Phase 1 of semantic analysis. It collects:
    /// - Enum definitions
    /// - Struct definitions
    /// - Function signatures
    /// - Method signatures
    ///
    /// The returned `TypeContext` is immutable and can be shared across
    /// threads for parallel function body analysis.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Phase 1: Gather declarations (sequential)
    /// let sema = Sema::new(rir, interner, preview);
    /// let (type_ctx, sema) = sema.gather_declarations()?;
    ///
    /// // Phase 2: Analyze function bodies (can be parallel)
    /// for fn_ref in rir.function_refs() {
    ///     let result = analyze_function_body(&type_ctx, ...)?;
    /// }
    /// ```
    pub fn gather_declarations(
        mut self,
    ) -> rue_error::CompileResult<(crate::type_context::TypeContext, GatherOutput<'a>)> {
        // Three-phase approach for correctness and performance:
        //
        // Phase 0: Inject built-in types (synthetic structs like String)
        // These must be registered before user code to enable collision detection.
        //
        // Phase 1: Register all type names (enum and struct IDs)
        // This allows types to reference each other in any order.
        //
        // Phase 2: Resolve all declarations in a single pass
        // Now that all type names are known, we can resolve field types,
        // validate @copy structs, and collect functions/methods together.
        self.inject_builtin_types();
        self.register_type_names()?;
        self.resolve_declarations()?;

        // Build the immutable type context
        let type_ctx = self.build_type_context();

        // Package up the remaining Sema state needed for function analysis
        let output = GatherOutput {
            rir: self.rir,
            interner: self.interner,
            struct_defs: self.struct_defs,
            enum_defs: self.enum_defs,
            array_types: self.array_types,
            array_type_defs: self.array_type_defs,
            structs: self.structs,
            enums: self.enums,
            functions: self.functions,
            methods: self.methods,
            preview_features: self.preview_features,
            builtin_string_id: self.builtin_string_id,
        };

        Ok((type_ctx, output))
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
            struct_defs: self.struct_defs.clone(),
            enum_defs: self.enum_defs.clone(),
            array_registry: ArrayTypeRegistry::from_existing(
                self.array_types.clone(),
                self.array_type_defs.clone(),
            ),
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
}

#[cfg(test)]
mod consistency_tests;
#[cfg(test)]
mod tests;
