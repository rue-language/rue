//! Semantic analysis - RIR to AIR conversion.
//!
//! Sema performs type checking and converts untyped RIR to typed AIR.
//! This is analogous to Zig's Sema phase.
//!
//! # Module Organization
//!
//! The semantic analyzer is split into focused modules:
//!
//! - [`mod.rs`] (this file): Core Sema struct, public API, output types
//! - [`context`]: Analysis context types (LocalVar, ParamInfo, scope management)
//! - [`types_helper`]: Type resolution and helpers (resolve_type, is_type_copy, etc.)
//! - [`strings`]: String literal handling and deduplication
//! - [`builtins`]: Built-in type injection (String, etc.)
//! - [`items`]: Declaration gathering (type names, functions, methods)
//! - [`airgen`]: AIR generation from RIR

mod airgen;
mod builtins;
mod context;
mod items;
mod strings;
mod types_helper;

use std::collections::{HashMap, HashSet};

use lasso::{Spur, ThreadedRodeo};
use rue_error::{
    CompileError, CompileErrors, CompileResult, CompileWarning, ErrorKind, MultiErrorResult,
    PreviewFeature, PreviewFeatures,
};
use rue_rir::{InstData, InstRef, Rir, RirDirective, RirParamMode};
use rue_span::Span;

use crate::inference::{FunctionSig, InferType, MethodSig};
use crate::inst::Air;
use crate::type_context::{FunctionSignature, MethodSignature, TypeContext};
use crate::types::{ArrayTypeDef, ArrayTypeId, EnumDef, EnumId, StructDef, StructId, Type};

// Re-export context types that are used within the crate
pub(crate) use context::{
    AnalysisContext, AnalysisResult, ConstValue, FieldPath, LocalVar, MoveInfo, ParamInfo,
    StringReceiverStorage, VariableMoveState,
};

/// Result of analyzing a function.
#[derive(Debug)]
pub struct AnalyzedFunction {
    pub name: String,
    pub air: Air,
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
    pub warnings: Vec<CompileWarning>,
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
    pub func_sigs: HashMap<Spur, FunctionSig>,
    /// Struct types: name -> Type::Struct(id).
    pub struct_types: HashMap<Spur, Type>,
    /// Enum types: name -> Type::Enum(id).
    pub enum_types: HashMap<Spur, Type>,
    /// Method signatures with InferType: (struct_name, method_name) -> MethodSig.
    pub method_sigs: HashMap<(Spur, Spur), MethodSig>,
}

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
            string_table: HashMap::new(),
            strings: Vec::new(),
            methods: self.methods,
            preview_features: self.preview_features,
            builtin_string_id: self.builtin_string_id,
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

/// Information about a function.
#[derive(Debug, Clone)]
pub struct FunctionInfo {
    /// Parameter types (in order)
    pub param_types: Vec<Type>,
    /// Parameter modes (in order)
    pub param_modes: Vec<RirParamMode>,
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
    pub body: InstRef,
    /// Span of the method declaration
    pub span: Span,
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
    /// String table: maps string content to index (for deduplication)
    pub(crate) string_table: HashMap<String, u32>,
    /// String data indexed by string table index
    pub(crate) strings: Vec<String>,
    /// Method table: maps (struct_name, method_name) to method info
    /// Used for resolving method calls (receiver.method()) and associated
    /// function calls (Type::function())
    pub(crate) methods: HashMap<(Spur, Spur), MethodInfo>,
    /// Enabled preview features
    pub(crate) preview_features: PreviewFeatures,
    /// StructId of the synthetic String type.
    /// This is populated during `inject_builtin_types()` and used for quick lookups.
    pub(crate) builtin_string_id: Option<StructId>,
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
            string_table: HashMap::new(),
            strings: Vec::new(),
            methods: HashMap::new(),
            preview_features,
            builtin_string_id: None,
        }
    }

    /// Build a `TypeContext` from the collected type information.
    ///
    /// This should be called after the declaration gathering phase (after calling
    /// `register_type_names` and `resolve_declarations`).
    ///
    /// The returned `TypeContext` is immutable and can be shared across
    /// threads for parallel function analysis.
    ///
    /// # Panics
    ///
    /// This method clones the type information, so it should only be called
    /// once per analysis to avoid unnecessary allocations.
    pub fn build_type_context(&self) -> TypeContext {
        // Build function signatures
        let func_sigs: HashMap<Spur, FunctionSignature> = self
            .functions
            .iter()
            .map(|(name, info)| {
                (
                    *name,
                    FunctionSignature {
                        param_types: info.param_types.clone(),
                        param_modes: info.param_modes.clone(),
                        return_type: info.return_type,
                    },
                )
            })
            .collect();

        // Build method signatures
        let method_sigs: HashMap<(Spur, Spur), MethodSignature> = self
            .methods
            .iter()
            .map(|((type_name, method_name), info)| {
                let struct_id = *self.structs.get(type_name).expect("method type must exist");
                (
                    (*type_name, *method_name),
                    MethodSignature {
                        struct_id,
                        struct_type: info.struct_type,
                        has_self: info.has_self,
                        param_names: info.param_names.clone(),
                        param_types: info.param_types.clone(),
                        return_type: info.return_type,
                    },
                )
            })
            .collect();

        TypeContext {
            func_sigs,
            method_sigs,
            struct_by_name: self.structs.clone(),
            struct_defs: self.struct_defs.clone(),
            enum_by_name: self.enums.clone(),
            enum_defs: self.enum_defs.clone(),
        }
    }

    /// Build an `InferenceContext` from the collected type information.
    ///
    /// This should be called after the collection phase and builds the
    /// pre-computed maps needed for Hindley-Milner type inference.
    /// Building this once and reusing for all function analyses avoids
    /// the O(n²) cost of rebuilding these maps per function.
    ///
    /// # Performance
    ///
    /// This converts all function/method signatures to use `InferType`
    /// (which handles arrays structurally rather than by ID). This conversion
    /// is done once instead of per-function.
    pub fn build_inference_context(&self) -> InferenceContext {
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

        InferenceContext {
            func_sigs,
            struct_types,
            enum_types,
            method_sigs,
        }
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
    pub fn gather_declarations(mut self) -> CompileResult<(TypeContext, GatherOutput<'a>)> {
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

    /// Check if a preview feature is enabled, returning an error if not.
    ///
    /// This is the gating mechanism for preview features. Call this method
    /// when semantic analysis encounters a feature that requires a preview flag.
    ///
    /// # Parameters
    /// - `feature`: The preview feature that is required
    /// - `what`: A description of what requires the feature (e.g., "string concatenation")
    /// - `span`: The source location where the feature is used
    ///
    /// # Returns
    /// - `Ok(())` if the feature is enabled
    /// - `Err(CompileError)` with a helpful message if not enabled
    pub(crate) fn require_preview(
        &self,
        feature: PreviewFeature,
        what: &str,
        span: Span,
    ) -> CompileResult<()> {
        if self.preview_features.contains(&feature) {
            Ok(())
        } else {
            Err(CompileError::new(
                ErrorKind::PreviewFeatureRequired {
                    feature,
                    what: what.to_string(),
                },
                span,
            )
            .with_help(format!(
                "use `--preview {}` to enable this feature ({})",
                feature.name(),
                feature.adr()
            )))
        }
    }

    /// Check if directives contain @allow for a specific warning name.
    pub(crate) fn has_allow_directive(
        &self,
        directives: &[RirDirective],
        warning_name: &str,
    ) -> bool {
        let allow_sym = self.interner.get("allow");
        let warning_sym = self.interner.get(warning_name);

        for directive in directives {
            if Some(directive.name) == allow_sym {
                for arg in &directive.args {
                    if Some(*arg) == warning_sym {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Analyze all functions in the RIR.
    ///
    /// Consumes the Sema and returns a [`SemaOutput`] containing all analyzed
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

        // Build inference context once - this contains pre-computed type information
        // (func_sigs, struct_types, enum_types, method_sigs) that would otherwise
        // be rebuilt for each function analysis.
        let infer_ctx = self.build_inference_context();

        // Now analyze function bodies - these can be analyzed independently
        // so we collect errors from all of them instead of stopping at the first
        let mut functions = Vec::new();
        let mut errors = CompileErrors::new();
        // Collect warnings from each function for parallel-safe warning collection
        let mut all_warnings = Vec::new();

        // Collect method refs from impl blocks so we can skip them in the first pass
        let mut method_refs: HashSet<InstRef> = HashSet::new();
        for (_, inst) in self.rir.iter() {
            if let InstData::ImplDecl {
                methods_start,
                methods_len,
                ..
            } = &inst.data
            {
                let methods = self.rir.get_inst_refs(*methods_start, *methods_len);
                for method_ref in methods {
                    method_refs.insert(method_ref);
                }
            }
        }

        // Analyze regular functions (not methods in impl blocks)
        for (inst_ref, inst) in self.rir.iter() {
            if let InstData::FnDecl {
                directives_start: _,
                directives_len: _,
                name,
                params_start,
                params_len,
                return_type,
                body,
                has_self: _,
            } = &inst.data
            {
                // Skip methods - they'll be analyzed separately with impl block context
                if method_refs.contains(&inst_ref) {
                    continue;
                }

                let fn_name = self.interner.resolve(&*name).to_string();
                let params = self.rir.get_params(*params_start, *params_len);

                // Try to analyze this function - on error, record it and continue
                match self.analyze_single_function(
                    &infer_ctx,
                    &fn_name,
                    *return_type,
                    &params,
                    *body,
                    inst.span,
                ) {
                    Ok((analyzed, warnings)) => {
                        functions.push(analyzed);
                        all_warnings.extend(warnings);
                    }
                    Err(e) => errors.push(e),
                }
            }
        }

        // Fourth pass: analyze method bodies from impl blocks
        for (_, inst) in self.rir.iter() {
            if let InstData::ImplDecl {
                type_name,
                methods_start,
                methods_len,
            } = &inst.data
            {
                let type_name_str = self.interner.resolve(&*type_name).to_string();
                let struct_id = *self.structs.get(type_name).unwrap();
                let struct_type = Type::Struct(struct_id);

                let methods = self.rir.get_inst_refs(*methods_start, *methods_len);
                for method_ref in methods {
                    let method_inst = self.rir.get(method_ref);
                    if let InstData::FnDecl {
                        name: method_name,
                        params_start,
                        params_len,
                        return_type,
                        body,
                        has_self,
                        ..
                    } = &method_inst.data
                    {
                        let method_name_str = self.interner.resolve(&*method_name).to_string();
                        let params = self.rir.get_params(*params_start, *params_len);

                        // Generate method name with struct prefix: "Type.method" or "Type::function"
                        let full_name = if *has_self {
                            format!("{}.{}", type_name_str, method_name_str)
                        } else {
                            format!("{}::{}", type_name_str, method_name_str)
                        };

                        // Try to analyze this method - on error, record it and continue
                        match self.analyze_method_function(
                            &infer_ctx,
                            &full_name,
                            *return_type,
                            &params,
                            *body,
                            method_inst.span,
                            struct_type,
                            *has_self,
                        ) {
                            Ok((analyzed, warnings)) => {
                                functions.push(analyzed);
                                all_warnings.extend(warnings);
                            }
                            Err(e) => errors.push(e),
                        }
                    }
                }
            }
        }

        // Fifth pass: analyze destructor bodies
        for (_, inst) in self.rir.iter() {
            if let InstData::DropFnDecl { type_name, body } = &inst.data {
                let type_name_str = self.interner.resolve(&*type_name).to_string();
                let struct_id = *self.structs.get(type_name).unwrap();
                let struct_type = Type::Struct(struct_id);

                // Generate destructor name: "TypeName.__drop"
                let full_name = format!("{}.__drop", type_name_str);

                // Try to analyze destructor - on error, record it and continue
                match self.analyze_destructor_function(
                    &infer_ctx,
                    &full_name,
                    *body,
                    inst.span,
                    struct_type,
                ) {
                    Ok((analyzed, warnings)) => {
                        functions.push(analyzed);
                        all_warnings.extend(warnings);
                    }
                    Err(e) => errors.push(e),
                }
            }
        }

        // Sort warnings by source location for deterministic output
        // (especially important when parallel analysis is enabled in the future)
        all_warnings.sort_by_key(|w| w.span().map(|s| s.start));

        // Return errors if any were collected
        errors.into_result_with(SemaOutput {
            functions,
            struct_defs: self.struct_defs,
            enum_defs: self.enum_defs,
            array_types: self.array_type_defs,
            strings: self.strings,
            warnings: all_warnings,
        })
    }

    /// Analyze all function bodies after declarations have been gathered.
    ///
    /// This is Phase 2 of a two-phase semantic analysis. It assumes that
    /// `gather_declarations()` has already been called (or that the Sema was
    /// created via `GatherOutput::into_sema()`).
    ///
    /// Unlike `analyze_all()`, this method does not re-gather declarations.
    /// It proceeds directly to analyzing function bodies.
    ///
    /// # Returns
    ///
    /// A `SemaOutput` containing all analyzed functions, or multiple errors
    /// if any function analysis fails.
    pub fn analyze_all_bodies(mut self) -> MultiErrorResult<SemaOutput> {
        // Build inference context once
        let infer_ctx = self.build_inference_context();

        let mut functions = Vec::new();
        let mut errors = CompileErrors::new();
        let mut all_warnings = Vec::new();

        // Collect method refs from impl blocks
        let mut method_refs: HashSet<InstRef> = HashSet::new();
        for (_, inst) in self.rir.iter() {
            if let InstData::ImplDecl {
                methods_start,
                methods_len,
                ..
            } = &inst.data
            {
                let methods = self.rir.get_inst_refs(*methods_start, *methods_len);
                for method_ref in methods {
                    method_refs.insert(method_ref);
                }
            }
        }

        // Analyze regular functions
        for (inst_ref, inst) in self.rir.iter() {
            if let InstData::FnDecl {
                name,
                params_start,
                params_len,
                return_type,
                body,
                ..
            } = &inst.data
            {
                if method_refs.contains(&inst_ref) {
                    continue;
                }

                let fn_name = self.interner.resolve(&*name).to_string();
                let params = self.rir.get_params(*params_start, *params_len);

                match self.analyze_single_function(
                    &infer_ctx,
                    &fn_name,
                    *return_type,
                    &params,
                    *body,
                    inst.span,
                ) {
                    Ok((analyzed, warnings)) => {
                        functions.push(analyzed);
                        all_warnings.extend(warnings);
                    }
                    Err(e) => errors.push(e),
                }
            }
        }

        // Analyze method bodies
        for (_, inst) in self.rir.iter() {
            if let InstData::ImplDecl {
                type_name,
                methods_start,
                methods_len,
            } = &inst.data
            {
                let type_name_str = self.interner.resolve(&*type_name).to_string();
                let struct_id = *self.structs.get(type_name).unwrap();
                let struct_type = Type::Struct(struct_id);

                let methods = self.rir.get_inst_refs(*methods_start, *methods_len);
                for method_ref in methods {
                    let method_inst = self.rir.get(method_ref);
                    if let InstData::FnDecl {
                        name: method_name,
                        params_start,
                        params_len,
                        return_type,
                        body,
                        has_self,
                        ..
                    } = &method_inst.data
                    {
                        let method_name_str = self.interner.resolve(&*method_name).to_string();
                        let params = self.rir.get_params(*params_start, *params_len);

                        let full_name = if *has_self {
                            format!("{}.{}", type_name_str, method_name_str)
                        } else {
                            format!("{}::{}", type_name_str, method_name_str)
                        };

                        match self.analyze_method_function(
                            &infer_ctx,
                            &full_name,
                            *return_type,
                            &params,
                            *body,
                            method_inst.span,
                            struct_type,
                            *has_self,
                        ) {
                            Ok((analyzed, warnings)) => {
                                functions.push(analyzed);
                                all_warnings.extend(warnings);
                            }
                            Err(e) => errors.push(e),
                        }
                    }
                }
            }
        }

        // Analyze destructor bodies
        for (_, inst) in self.rir.iter() {
            if let InstData::DropFnDecl { type_name, body } = &inst.data {
                let type_name_str = self.interner.resolve(&*type_name).to_string();
                let struct_id = *self.structs.get(type_name).unwrap();
                let struct_type = Type::Struct(struct_id);

                let full_name = format!("{}.__drop", type_name_str);

                match self.analyze_destructor_function(
                    &infer_ctx,
                    &full_name,
                    *body,
                    inst.span,
                    struct_type,
                ) {
                    Ok((analyzed, warnings)) => {
                        functions.push(analyzed);
                        all_warnings.extend(warnings);
                    }
                    Err(e) => errors.push(e),
                }
            }
        }

        all_warnings.sort_by_key(|w| w.span().map(|s| s.start));

        errors.into_result_with(SemaOutput {
            functions,
            struct_defs: self.struct_defs,
            enum_defs: self.enum_defs,
            array_types: self.array_type_defs,
            strings: self.strings,
            warnings: all_warnings,
        })
    }
}

#[cfg(test)]
mod tests;
