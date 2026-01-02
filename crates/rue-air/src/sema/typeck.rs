//! Type checking and resolution helpers for semantic analysis.
//!
//! This module contains helper functions for:
//! - Resolving type symbols to concrete types
//! - Type checking (is_copy, format_type_name)
//! - ABI slot calculations
//! - Type conversions between AIR types and inference types

use lasso::Spur;
use rue_error::{CompileError, CompileResult, ErrorKind};
use rue_span::Span;

use super::Sema;
use crate::inference::InferType;
use crate::types::{ArrayTypeDef, ArrayTypeId, StructId, Type, parse_array_type_syntax};

impl<'a> Sema<'a> {
    /// Get a human-readable name for a type.
    pub(crate) fn format_type_name(&self, ty: Type) -> String {
        match ty {
            Type::I8 => "i8".to_string(),
            Type::I16 => "i16".to_string(),
            Type::I32 => "i32".to_string(),
            Type::I64 => "i64".to_string(),
            Type::U8 => "u8".to_string(),
            Type::U16 => "u16".to_string(),
            Type::U32 => "u32".to_string(),
            Type::U64 => "u64".to_string(),
            Type::Bool => "bool".to_string(),
            Type::Unit => "()".to_string(),
            Type::Never => "!".to_string(),
            Type::Error => "<error>".to_string(),
            // Note: String is now handled via Type::Struct with builtin_string_id
            Type::Struct(struct_id) => self.type_pool.struct_def(struct_id).name.clone(),
            Type::Enum(enum_id) => self.type_pool.enum_def(enum_id).name.clone(),
            Type::Array(array_id) => {
                let array_def = &self.array_type_defs[array_id.0 as usize];
                format!(
                    "[{}; {}]",
                    self.format_type_name(array_def.element_type),
                    array_def.length
                )
            }
            Type::Module(_) => "<module>".to_string(),
            Type::ComptimeType => "type".to_string(),
        }
    }

    /// Check if a type is a Copy type.
    /// This differs from Type::is_copy() because it can look up struct definitions
    /// to check if a struct is marked with @copy.
    pub(crate) fn is_type_copy(&self, ty: Type) -> bool {
        match ty {
            // Primitive Copy types
            Type::I8
            | Type::I16
            | Type::I32
            | Type::I64
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::Bool
            | Type::Unit => true,
            // Enum types are Copy (they're small discriminant values)
            Type::Enum(_) => true,
            // Never and Error are Copy for convenience
            Type::Never | Type::Error => true,
            // Struct types: check if marked with @copy
            Type::Struct(struct_id) => {
                let struct_def = self.type_pool.struct_def(struct_id);
                struct_def.is_copy
            }
            // Note: String is now handled via Type::Struct with is_builtin
            // Arrays are Copy if their element type is Copy
            Type::Array(array_id) => {
                let array_def = &self.array_type_defs[array_id.0 as usize];
                self.is_type_copy(array_def.element_type)
            }
            // Module types are Copy (they're just compile-time namespace references)
            Type::Module(_) => true,
            // ComptimeType is Copy (only exists at comptime anyway)
            Type::ComptimeType => true,
        }
    }

    /// Convert a fully-resolved InferType to a concrete Type.
    ///
    /// This handles the conversion of InferType::Array to Type::Array(id)
    /// by using the array type registry.
    pub(crate) fn infer_type_to_type(&mut self, ty: &InferType) -> Type {
        match ty {
            InferType::Concrete(t) => *t,
            InferType::Var(_) => Type::Error,   // Unbound variable
            InferType::IntLiteral => Type::I32, // Default (shouldn't happen after resolution)
            InferType::Array { element, length } => {
                // Recursively convert element type
                let elem_ty = self.infer_type_to_type(element);
                if elem_ty == Type::Error {
                    return Type::Error;
                }
                // Get or create the array type ID
                let array_type_id = self.get_or_create_array_type(elem_ty, *length);
                Type::Array(array_type_id)
            }
        }
    }

    /// Convert a concrete Type to InferType for use in constraint generation.
    ///
    /// This handles the conversion of Type::Array(id) to InferType::Array
    /// by looking up the array definition to get element type and length.
    pub(crate) fn type_to_infer_type(&self, ty: Type) -> InferType {
        match ty {
            Type::Array(array_id) => {
                let array_def = &self.array_type_defs[array_id.0 as usize];
                let element_infer = self.type_to_infer_type(array_def.element_type);
                InferType::Array {
                    element: Box::new(element_infer),
                    length: array_def.length,
                }
            }
            // All other types wrap directly
            _ => InferType::Concrete(ty),
        }
    }
    /// Resolve a type symbol to a Type.
    ///
    /// Handles array types with the syntax "[T; N]".
    pub(crate) fn resolve_type(&mut self, type_sym: Spur, span: Span) -> CompileResult<Type> {
        let type_name = self.interner.resolve(&type_sym);

        // Check primitive types first.
        // Note: String is handled below via struct lookup (it's a builtin struct).
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
            // The type of types - used for comptime type parameters
            "type" => return Ok(Type::ComptimeType),
            _ => {}
        }

        if let Some(&struct_id) = self.structs.get(&type_sym) {
            Ok(Type::Struct(struct_id))
        } else if let Some(&enum_id) = self.enums.get(&type_sym) {
            Ok(Type::Enum(enum_id))
        } else {
            // Check for array type syntax: [T; N]
            if let Some((element_type, length)) = parse_array_type_syntax(type_name) {
                // Resolve the element type first
                let element_sym = self.interner.get_or_intern(&element_type);
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

    /// Get or create an array type for the given element type and length.
    pub(crate) fn get_or_create_array_type(
        &mut self,
        element_type: Type,
        length: u64,
    ) -> ArrayTypeId {
        let key = (element_type, length);
        if let Some(&id) = self.array_types.get(&key) {
            return id;
        }

        let id = ArrayTypeId(self.array_type_defs.len() as u32);
        self.array_type_defs.push(ArrayTypeDef {
            element_type,
            length,
        });
        self.array_types.insert(key, id);
        id
    }

    /// Pre-create array types from a resolved InferType.
    ///
    /// This walks the InferType recursively and ensures all array types that will
    /// be needed during `infer_type_to_type` conversion are created beforehand.
    /// This separation enables future parallelization of function analysis, where
    /// all mutations happen in this pre-collection phase.
    pub(crate) fn pre_create_array_types_from_infer_type(&mut self, ty: &InferType) {
        match ty {
            InferType::Array { element, length } => {
                // First recursively process nested array types (e.g., [[i32; 3]; 4])
                self.pre_create_array_types_from_infer_type(element);

                // Convert the element type to get the concrete Type
                // (This is safe because we processed nested arrays first)
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
    ///
    /// This is a helper for `pre_create_array_types_from_infer_type` that converts
    /// the element type without mutating `self.array_types` (since we're in a
    /// pre-creation context where the array type may not exist yet).
    pub(crate) fn infer_type_to_concrete_type_for_key(&self, ty: &InferType) -> Type {
        match ty {
            InferType::Concrete(t) => *t,
            InferType::Var(_) => Type::Error,   // Unbound variable
            InferType::IntLiteral => Type::I32, // Default
            InferType::Array { element, length } => {
                // For nested arrays, look up the already-created array type
                let elem_ty = self.infer_type_to_concrete_type_for_key(element);
                if elem_ty == Type::Error {
                    return Type::Error;
                }
                // The array type should already exist from the recursive call
                let key = (elem_ty, *length);
                if let Some(&id) = self.array_types.get(&key) {
                    Type::Array(id)
                } else {
                    // This shouldn't happen if we process depth-first, but handle gracefully
                    debug_assert!(
                        false,
                        "Array type not found during pre-creation: ({:?}, {})",
                        elem_ty, length
                    );
                    Type::Error
                }
            }
        }
    }

    /// Get the number of ABI slots required for a type.
    /// Scalar types (i8, i16, i32, i64, u8, u16, u32, u64, bool) use 1 slot,
    /// structs use 1 slot per field, arrays use 1 slot per element.
    /// Zero-sized types (unit, never, empty structs, zero-length arrays) use 0 slots.
    pub(crate) fn abi_slot_count(&self, ty: Type) -> u32 {
        match ty {
            Type::I8
            | Type::I16
            | Type::I32
            | Type::I64
            | Type::U8
            | Type::U16
            | Type::U32
            | Type::U64
            | Type::Bool
            | Type::Error => 1,
            // Zero-sized types use 0 slots
            // ComptimeType is comptime-only and uses 0 runtime slots
            Type::Unit | Type::Never | Type::ComptimeType => 0,
            // Enums are represented as their discriminant type (a scalar), so 1 slot
            Type::Enum(_) => 1,
            // Struct uses sum of all field slots (includes builtin String with 3 fields)
            Type::Struct(struct_id) => {
                // Sum the slot counts of all fields (handles arrays, nested structs, and builtins)
                // Empty structs naturally get 0 slots here
                let struct_def = self.type_pool.struct_def(struct_id);
                struct_def
                    .fields
                    .iter()
                    .map(|f| self.abi_slot_count(f.ty))
                    .sum()
            }
            Type::Array(array_type_id) => {
                // Zero-length arrays naturally get 0 slots (0 * element_slots)
                let array_def = &self.array_type_defs[array_type_id.0 as usize];
                let element_slots = self.abi_slot_count(array_def.element_type);
                element_slots * array_def.length as u32
            }
            // Module types don't take ABI slots (they're compile-time only)
            Type::Module(_) => 0,
        }
    }

    /// Get the slot offset of a field within a struct.
    /// Returns the number of slots before the field starts.
    pub(crate) fn field_slot_offset(&self, struct_id: StructId, field_index: usize) -> u32 {
        let struct_def = self.type_pool.struct_def(struct_id);
        struct_def.fields[..field_index]
            .iter()
            .map(|f| self.abi_slot_count(f.ty))
            .sum()
    }
}
