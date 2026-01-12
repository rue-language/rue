//! Per-function analysis state and analyzer.
//!
//! This module contains `FunctionAnalyzer`, which holds the mutable state
//! needed during function body analysis. Each function gets its own analyzer
//! instance, enabling parallel analysis.
//!
//! # Architecture
//!
//! `FunctionAnalyzer` is designed to work with a shared immutable `SemaContext`.
//! The split enables:
//! - Parallel function body analysis (each function has independent mutable state)
//! - Better separation of concerns (immutable type info vs mutable analysis state)
//! - Post-analysis merging of results (strings, warnings)
//!
//! Array types are managed by the thread-safe `TypeInternPool` in `SemaContext`,
//! allowing parallel creation without local buffering or post-merge remapping.

use std::collections::HashMap;

use lasso::Spur;
use rue_error::{CompileError, CompileResult, CompileWarning, ErrorKind, PreviewFeature};
use rue_span::Span;

use crate::inference::InferType;
use crate::sema_context::SemaContext;
use crate::types::{ArrayTypeId, Type};

/// Per-function mutable state during semantic analysis.
///
/// This struct contains all mutable state that is modified during function
/// body analysis. For parallel analysis, each function gets its own instance,
/// and results are merged afterward.
///
/// # Separation from SemaContext
///
/// `FunctionAnalyzer` holds mutable state while `SemaContext` holds immutable
/// type information. This separation enables:
/// - Sharing `SemaContext` across parallel function analyses
/// - Independent mutable state per function
/// - Post-analysis merging of results (strings, warnings)
///
/// Array types are created via the thread-safe `TypeInternPool` in `SemaContext`,
/// so they don't require local buffering or post-merge remapping.
#[derive(Debug)]
pub struct FunctionAnalyzer<'a, 'ctx> {
    /// Reference to the shared immutable context.
    pub ctx: &'a SemaContext<'ctx>,
    /// String table for deduplication.
    string_table: HashMap<String, u32>,
    /// String literals in order of creation.
    strings: Vec<String>,
    /// Warnings collected during analysis.
    warnings: Vec<CompileWarning>,
    /// The `Self` type for methods.
    ///
    /// When analyzing methods (both instance methods with `self` and associated functions),
    /// this field contains the struct type that the method belongs to. This allows resolving
    /// `Self` type annotations in method signatures and bodies.
    ///
    /// `None` for regular functions (not methods).
    #[allow(dead_code)] // TODO: Use this for Self type resolution
    self_type: Option<Type>,
}

/// Output from analyzing a single function.
#[derive(Debug)]
pub struct FunctionAnalyzerOutput {
    /// String literals (deduplicated).
    pub strings: Vec<String>,
    /// Warnings collected during analysis.
    pub warnings: Vec<CompileWarning>,
}

impl<'a, 'ctx> FunctionAnalyzer<'a, 'ctx> {
    /// Create a new function analyzer with a reference to the shared context.
    ///
    /// # Parameters
    /// - `ctx`: The shared semantic analysis context
    /// - `self_type`: The `Self` type for methods, or `None` for regular functions
    pub fn new(ctx: &'a SemaContext<'ctx>, self_type: Option<Type>) -> Self {
        Self {
            ctx,
            string_table: HashMap::new(),
            strings: Vec::new(),
            warnings: Vec::new(),
            self_type,
        }
    }

    /// Consume the analyzer and return its output.
    pub fn into_output(self) -> FunctionAnalyzerOutput {
        FunctionAnalyzerOutput {
            strings: self.strings,
            warnings: self.warnings,
        }
    }

    /// Add a string to the string table, returning its index.
    /// Deduplicates identical strings.
    pub fn add_string(&mut self, content: String) -> u32 {
        use std::collections::hash_map::Entry;
        match self.string_table.entry(content) {
            Entry::Occupied(e) => *e.get(),
            Entry::Vacant(e) => {
                let id = self.strings.len() as u32;
                self.strings.push(e.key().clone());
                e.insert(id);
                id
            }
        }
    }

    /// Add a warning.
    pub fn add_warning(&mut self, warning: CompileWarning) {
        self.warnings.push(warning);
    }

    /// Get or create an array type, returning its ID.
    ///
    /// Delegates to the thread-safe `TypeInternPool` in `SemaContext`.
    pub fn get_or_create_array_type(&self, element_type: Type, length: u64) -> ArrayTypeId {
        self.ctx.get_or_create_array_type(element_type, length)
    }

    /// Get an array type definition by ID.
    ///
    /// Returns `(element_type, length)` for the array.
    pub fn get_array_type_def(&self, id: ArrayTypeId) -> (Type, u64) {
        self.ctx.get_array_type_def(id)
    }

    /// Pre-create array types from a resolved InferType.
    ///
    /// This walks the InferType recursively and ensures all array types that will
    /// be needed during `infer_type_to_type` conversion are created beforehand.
    ///
    /// With the thread-safe `TypeInternPool`, this is no longer strictly necessary
    /// since `infer_type_to_type` can create array types on-demand. However, it's
    /// kept for explicit documentation of intent and potential future optimizations.
    pub fn pre_create_array_types_from_infer_type(&self, ty: &InferType) {
        match ty {
            InferType::Array { element, length } => {
                // First recursively process nested array types
                self.pre_create_array_types_from_infer_type(element);

                // Convert the element type to get the concrete Type
                let elem_ty = self.infer_type_to_type(element);
                if elem_ty != Type::ERROR {
                    // Pre-create this array type
                    self.get_or_create_array_type(elem_ty, *length);
                }
            }
            InferType::Concrete(_) | InferType::Var(_) | InferType::IntLiteral => {
                // Non-array types don't need pre-creation
            }
        }
    }

    /// Convert a fully-resolved InferType to a concrete Type.
    pub fn infer_type_to_type(&self, ty: &InferType) -> Type {
        match ty {
            InferType::Concrete(t) => *t,
            InferType::Var(_) => Type::ERROR,
            InferType::IntLiteral => Type::I32,
            InferType::Array { element, length } => {
                let elem_ty = self.infer_type_to_type(element);
                if elem_ty == Type::ERROR {
                    return Type::ERROR;
                }
                // Use the thread-safe registry to get or create the array type
                let id = self.get_or_create_array_type(elem_ty, *length);
                Type::new_array(id)
            }
        }
    }

    /// Check that a preview feature is enabled.
    pub fn require_preview(
        &self,
        feature: PreviewFeature,
        what: &str,
        span: Span,
    ) -> CompileResult<()> {
        if self.ctx.preview_features.contains(&feature) {
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

    /// Get a human-readable name for a type.
    /// Delegates to context for most types but handles local array types.
    pub fn format_type_name(&self, ty: Type) -> String {
        if let Some(array_id) = ty.as_array() {
            let (element_type, length) = self.get_array_type_def(array_id);
            format!("[{}; {}]", self.format_type_name(element_type), length)
        } else {
            self.ctx.format_type_name(ty)
        }
    }

    /// Check if a type is a Copy type.
    /// Delegates to context for most types but handles local array types.
    pub fn is_type_copy(&self, ty: Type) -> bool {
        if let Some(array_id) = ty.as_array() {
            let (element_type, _length) = self.get_array_type_def(array_id);
            self.is_type_copy(element_type)
        } else {
            self.ctx.is_type_copy(ty)
        }
    }

    /// Get the number of ABI slots required for a type.
    /// Delegates to context for most types but handles local array types.
    pub fn abi_slot_count(&self, ty: Type) -> u32 {
        if let Some(array_id) = ty.as_array() {
            let (element_type, length) = self.get_array_type_def(array_id);
            let element_slots = self.abi_slot_count(element_type);
            element_slots * length as u32
        } else {
            self.ctx.abi_slot_count(ty)
        }
    }

    /// Resolve a type symbol to a Type.
    pub fn resolve_type(&mut self, type_sym: Spur, span: Span) -> CompileResult<Type> {
        let type_name = self.ctx.interner.resolve(&type_sym);

        // Check primitive types
        match type_name {
            "i8" => return Ok(Type::I8),
            "i16" => return Ok(Type::I16),
            "i32" => return Ok(Type::I32),
            "i64" => return Ok(Type::I64),
            "u8" => return Ok(Type::U8),
            "u16" => return Ok(Type::U16),
            "u32" => return Ok(Type::U32),
            "u64" => return Ok(Type::U64),
            "bool" => return Ok(Type::BOOL),
            "()" => return Ok(Type::UNIT),
            "!" => return Ok(Type::NEVER),
            _ => {}
        }

        if let Some(struct_id) = self.ctx.get_struct(type_sym) {
            Ok(Type::new_struct(struct_id))
        } else if let Some(enum_id) = self.ctx.get_enum(type_sym) {
            Ok(Type::new_enum(enum_id))
        } else {
            // Check for array type syntax: [T; N]
            if let Some((element_type, length)) = crate::types::parse_array_type_syntax(type_name) {
                // Resolve the element type first
                let element_sym = self.ctx.interner.get_or_intern(&element_type);
                let element_ty = self.resolve_type(element_sym, span)?;
                // Get or create the array type
                let array_type_id = self.get_or_create_array_type(element_ty, length);
                Ok(Type::new_array(array_type_id))
            } else if let Some((pointee_type, mutability)) =
                crate::types::parse_pointer_type_syntax(type_name)
            {
                // Resolve the pointee type first
                let pointee_sym = self.ctx.interner.get_or_intern(&pointee_type);
                let pointee_ty = self.resolve_type(pointee_sym, span)?;
                // Create the pointer type
                match mutability {
                    crate::types::PtrMutability::Const => {
                        let ptr_id = self.ctx.get_or_create_ptr_const_type(pointee_ty);
                        Ok(Type::new_ptr_const(ptr_id))
                    }
                    crate::types::PtrMutability::Mut => {
                        let ptr_id = self.ctx.get_or_create_ptr_mut_type(pointee_ty);
                        Ok(Type::new_ptr_mut(ptr_id))
                    }
                }
            } else {
                Err(CompileError::new(
                    ErrorKind::UnknownType(type_name.to_string()),
                    span,
                ))
            }
        }
    }

    /// Access the warnings collected during analysis.
    pub fn warnings(&self) -> &[CompileWarning] {
        &self.warnings
    }

    /// Access the strings collected during analysis.
    pub fn strings(&self) -> &[String] {
        &self.strings
    }
}

/// Merge multiple function analyzer outputs into a single result.
///
/// This is used after parallel analysis to combine results from all functions.
/// Array types are managed by the thread-safe `TypeInternPool` in `SemaContext`,
/// so only strings and warnings need merging.
#[derive(Debug, Default)]
pub struct MergedFunctionOutput {
    /// All string literals (deduplicated).
    pub strings: Vec<String>,
    /// Mapping from string content to final index.
    pub string_map: HashMap<String, u32>,
    /// All warnings from all functions.
    pub warnings: Vec<CompileWarning>,
}

impl MergedFunctionOutput {
    /// Create a new empty merged output.
    pub fn new() -> Self {
        Self::default()
    }

    /// Merge a function's output into this merged result.
    ///
    /// Returns a remapping for string indices so the function's AIR
    /// can be updated with the final IDs.
    pub fn merge_function_output(
        &mut self,
        output: FunctionAnalyzerOutput,
    ) -> FunctionOutputRemapping {
        let mut string_remap = HashMap::new();

        // Merge strings (deduplicate by content)
        for (idx, content) in output.strings.into_iter().enumerate() {
            let old_id = idx as u32;
            let new_id = if let Some(&id) = self.string_map.get(&content) {
                id
            } else {
                let id = self.strings.len() as u32;
                self.strings.push(content.clone());
                self.string_map.insert(content, id);
                id
            };
            if old_id != new_id {
                string_remap.insert(old_id, new_id);
            }
        }

        // Merge warnings
        self.warnings.extend(output.warnings);

        FunctionOutputRemapping { string_remap }
    }
}

/// Remapping information for updating AIR after merging.
#[derive(Debug, Default)]
pub struct FunctionOutputRemapping {
    /// Mapping from old string index to new string index.
    pub string_remap: HashMap<u32, u32>,
}

impl FunctionOutputRemapping {
    /// Check if any remapping is needed.
    pub fn is_empty(&self) -> bool {
        self.string_remap.is_empty()
    }

    /// Remap a string index if needed.
    pub fn remap_string(&self, id: u32) -> u32 {
        self.string_remap.get(&id).copied().unwrap_or(id)
    }
}
