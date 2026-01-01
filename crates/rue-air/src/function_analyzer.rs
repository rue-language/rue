//! Per-function analysis state and analyzer.
//!
//! This module contains `FunctionAnalyzer`, which holds the mutable state
//! needed during function body analysis. Each function gets its own analyzer
//! instance, enabling future parallel analysis.
//!
//! # Architecture
//!
//! `FunctionAnalyzer` is designed to work with a shared immutable `SemaContext`.
//! The split enables:
//! - Parallel function body analysis (each function has independent mutable state)
//! - Better separation of concerns (immutable type info vs mutable analysis state)
//! - Post-analysis merging of results (strings, array types, warnings)

use std::collections::HashMap;

use lasso::Spur;
use rue_error::{CompileError, CompileResult, CompileWarning, ErrorKind, PreviewFeature};
use rue_span::Span;

use crate::inference::InferType;
use crate::sema_context::SemaContext;
use crate::types::{ArrayTypeDef, ArrayTypeId, Type};

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
/// - Post-analysis merging of results
#[derive(Debug)]
pub struct FunctionAnalyzer<'a, 'ctx> {
    /// Reference to the shared immutable context.
    pub ctx: &'a SemaContext<'ctx>,
    /// Array types created during this function's analysis.
    /// These are local to the function and merged post-analysis.
    array_types: HashMap<(Type, u64), ArrayTypeId>,
    /// Array type definitions in order of creation.
    array_type_defs: Vec<ArrayTypeDef>,
    /// String table for deduplication.
    string_table: HashMap<String, u32>,
    /// String literals in order of creation.
    strings: Vec<String>,
    /// Warnings collected during analysis.
    warnings: Vec<CompileWarning>,
}

/// Output from analyzing a single function.
#[derive(Debug)]
pub struct FunctionAnalyzerOutput {
    /// Array types created during analysis.
    pub array_types: HashMap<(Type, u64), ArrayTypeId>,
    /// Array type definitions.
    pub array_type_defs: Vec<ArrayTypeDef>,
    /// String literals (deduplicated).
    pub strings: Vec<String>,
    /// Warnings collected during analysis.
    pub warnings: Vec<CompileWarning>,
}

impl<'a, 'ctx> FunctionAnalyzer<'a, 'ctx> {
    /// Create a new function analyzer with a reference to the shared context.
    pub fn new(ctx: &'a SemaContext<'ctx>) -> Self {
        Self {
            ctx,
            array_types: HashMap::new(),
            array_type_defs: Vec::new(),
            string_table: HashMap::new(),
            strings: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Create a new function analyzer initialized with pre-existing array types.
    ///
    /// This is used when we need to inherit array types from the context
    /// (e.g., array types discovered during declaration gathering).
    pub fn with_array_types(
        ctx: &'a SemaContext<'ctx>,
        array_types: HashMap<(Type, u64), ArrayTypeId>,
        array_type_defs: Vec<ArrayTypeDef>,
    ) -> Self {
        Self {
            ctx,
            array_types,
            array_type_defs,
            string_table: HashMap::new(),
            strings: Vec::new(),
            warnings: Vec::new(),
        }
    }

    /// Consume the analyzer and return its output.
    pub fn into_output(self) -> FunctionAnalyzerOutput {
        FunctionAnalyzerOutput {
            array_types: self.array_types,
            array_type_defs: self.array_type_defs,
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
    pub fn get_or_create_array_type(&mut self, element_type: Type, length: u64) -> ArrayTypeId {
        let key = (element_type, length);

        // First check if it exists in the shared context
        if let Some(id) = self.ctx.get_array_type(element_type, length) {
            return id;
        }

        // Then check local cache
        if let Some(&id) = self.array_types.get(&key) {
            return id;
        }

        // Create new array type locally
        // Use an offset based on context's array types to avoid ID collisions
        let base_id = self.ctx.array_type_defs.len() as u32;
        let id = ArrayTypeId(base_id + self.array_type_defs.len() as u32);
        self.array_type_defs.push(ArrayTypeDef {
            element_type,
            length,
        });
        self.array_types.insert(key, id);
        id
    }

    /// Get an array type definition by ID.
    ///
    /// First checks the shared context, then the local definitions.
    pub fn get_array_type_def(&self, id: ArrayTypeId) -> &ArrayTypeDef {
        let base_id = self.ctx.array_type_defs.len() as u32;
        if id.0 < base_id {
            // From shared context
            &self.ctx.array_type_defs[id.0 as usize]
        } else {
            // From local definitions
            let local_idx = (id.0 - base_id) as usize;
            &self.array_type_defs[local_idx]
        }
    }

    /// Pre-create array types from a resolved InferType.
    ///
    /// This walks the InferType recursively and ensures all array types that will
    /// be needed during `infer_type_to_type` conversion are created beforehand.
    pub fn pre_create_array_types_from_infer_type(&mut self, ty: &InferType) {
        match ty {
            InferType::Array { element, length } => {
                // First recursively process nested array types
                self.pre_create_array_types_from_infer_type(element);

                // Convert the element type to get the concrete Type
                let elem_ty = self.infer_type_to_concrete_type_for_key(element);
                if elem_ty != Type::Error {
                    // Pre-create this array type
                    self.get_or_create_array_type(elem_ty, *length);
                }
            }
            InferType::Concrete(_) | InferType::Var(_) | InferType::IntLiteral => {
                // Non-array types don't need pre-creation
            }
        }
    }

    /// Convert an InferType to a concrete Type for use as an array element key.
    fn infer_type_to_concrete_type_for_key(&self, ty: &InferType) -> Type {
        match ty {
            InferType::Concrete(t) => *t,
            InferType::Var(_) => Type::Error,
            InferType::IntLiteral => Type::I32,
            InferType::Array { element, length } => {
                let elem_ty = self.infer_type_to_concrete_type_for_key(element);
                if elem_ty == Type::Error {
                    return Type::Error;
                }
                // Check shared context first
                if let Some(id) = self.ctx.get_array_type(elem_ty, *length) {
                    return Type::Array(id);
                }
                // Then check local cache
                let key = (elem_ty, *length);
                if let Some(&id) = self.array_types.get(&key) {
                    Type::Array(id)
                } else {
                    Type::Error
                }
            }
        }
    }

    /// Convert a fully-resolved InferType to a concrete Type.
    pub fn infer_type_to_type(&self, ty: &InferType) -> Type {
        match ty {
            InferType::Concrete(t) => *t,
            InferType::Var(_) => Type::Error,
            InferType::IntLiteral => Type::I32,
            InferType::Array { element, length } => {
                let elem_ty = self.infer_type_to_type(element);
                if elem_ty == Type::Error {
                    return Type::Error;
                }
                // Check shared context first
                if let Some(id) = self.ctx.get_array_type(elem_ty, *length) {
                    return Type::Array(id);
                }
                // Then check local cache
                let key = (elem_ty, *length);
                if let Some(&id) = self.array_types.get(&key) {
                    Type::Array(id)
                } else {
                    // Should have been pre-created
                    debug_assert!(false, "Array type not found: ({:?}, {})", elem_ty, length);
                    Type::Error
                }
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
        match ty {
            Type::Array(array_id) => {
                let array_def = self.get_array_type_def(array_id);
                format!(
                    "[{}; {}]",
                    self.format_type_name(array_def.element_type),
                    array_def.length
                )
            }
            _ => self.ctx.format_type_name(ty),
        }
    }

    /// Check if a type is a Copy type.
    /// Delegates to context for most types but handles local array types.
    pub fn is_type_copy(&self, ty: Type) -> bool {
        match ty {
            Type::Array(array_id) => {
                let array_def = self.get_array_type_def(array_id);
                self.is_type_copy(array_def.element_type)
            }
            _ => self.ctx.is_type_copy(ty),
        }
    }

    /// Get the number of ABI slots required for a type.
    /// Delegates to context for most types but handles local array types.
    pub fn abi_slot_count(&self, ty: Type) -> u32 {
        match ty {
            Type::Array(array_id) => {
                let array_def = self.get_array_type_def(array_id);
                let element_slots = self.abi_slot_count(array_def.element_type);
                element_slots * array_def.length as u32
            }
            _ => self.ctx.abi_slot_count(ty),
        }
    }

    /// Resolve a type symbol to a Type.
    pub fn resolve_type(&mut self, type_sym: Spur, span: Span) -> CompileResult<Type> {
        let type_name = self.ctx.interner.resolve(&type_sym);

        // Check primitive types first
        match type_name {
            "i8" => return Ok(Type::I8),
            "i16" => return Ok(Type::I16),
            "i32" => return Ok(Type::I32),
            "i64" => return Ok(Type::I64),
            "u8" => return Ok(Type::U8),
            "u16" => return Ok(Type::U16),
            "u32" => return Ok(Type::U32),
            "u64" => return Ok(Type::U64),
            "bool" => return Ok(Type::Bool),
            "()" => return Ok(Type::Unit),
            "!" => return Ok(Type::Never),
            _ => {}
        }

        if let Some(struct_id) = self.ctx.get_struct(type_sym) {
            Ok(Type::Struct(struct_id))
        } else if let Some(enum_id) = self.ctx.get_enum(type_sym) {
            Ok(Type::Enum(enum_id))
        } else {
            // Check for array type syntax: [T; N]
            if let Some((element_type, length)) = crate::types::parse_array_type_syntax(type_name) {
                // Resolve the element type first
                let element_sym = self.ctx.interner.get_or_intern(&element_type);
                let element_ty = self.resolve_type(element_sym, span)?;
                // Get or create the array type
                let array_type_id = self.get_or_create_array_type(element_ty, length);
                Ok(Type::Array(array_type_id))
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
#[derive(Debug, Default)]
pub struct MergedFunctionOutput {
    /// All array type definitions (deduplicated).
    pub array_type_defs: Vec<ArrayTypeDef>,
    /// Mapping from original (element_type, length) to final ArrayTypeId.
    pub array_type_map: HashMap<(Type, u64), ArrayTypeId>,
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

    /// Initialize with array types from the context.
    pub fn with_context_arrays(ctx: &SemaContext) -> Self {
        let array_type_map: HashMap<(Type, u64), ArrayTypeId> = ctx.array_types.clone();
        let array_type_defs = ctx.array_type_defs.clone();
        Self {
            array_type_defs,
            array_type_map,
            strings: Vec::new(),
            string_map: HashMap::new(),
            warnings: Vec::new(),
        }
    }

    /// Merge a function's output into this merged result.
    ///
    /// Returns a remapping for array type IDs and string indices so the
    /// function's AIR can be updated with the final IDs.
    pub fn merge_function_output(
        &mut self,
        output: FunctionAnalyzerOutput,
    ) -> FunctionOutputRemapping {
        let mut array_remap = HashMap::new();
        let mut string_remap = HashMap::new();

        // Merge array types (deduplicate by (element_type, length))
        for (key, old_id) in output.array_types {
            let new_id = if let Some(&id) = self.array_type_map.get(&key) {
                id
            } else {
                let id = ArrayTypeId(self.array_type_defs.len() as u32);
                self.array_type_defs.push(ArrayTypeDef {
                    element_type: key.0,
                    length: key.1,
                });
                self.array_type_map.insert(key, id);
                id
            };
            if old_id != new_id {
                array_remap.insert(old_id, new_id);
            }
        }

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

        FunctionOutputRemapping {
            array_remap,
            string_remap,
        }
    }
}

/// Remapping information for updating AIR after merging.
#[derive(Debug, Default)]
pub struct FunctionOutputRemapping {
    /// Mapping from old ArrayTypeId to new ArrayTypeId.
    pub array_remap: HashMap<ArrayTypeId, ArrayTypeId>,
    /// Mapping from old string index to new string index.
    pub string_remap: HashMap<u32, u32>,
}

impl FunctionOutputRemapping {
    /// Check if any remapping is needed.
    pub fn is_empty(&self) -> bool {
        self.array_remap.is_empty() && self.string_remap.is_empty()
    }

    /// Remap an array type ID if needed.
    pub fn remap_array_type(&self, id: ArrayTypeId) -> ArrayTypeId {
        self.array_remap.get(&id).copied().unwrap_or(id)
    }

    /// Remap a string index if needed.
    pub fn remap_string(&self, id: u32) -> u32 {
        self.string_remap.get(&id).copied().unwrap_or(id)
    }
}
