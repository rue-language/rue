//! Immutable semantic analysis context.
//!
//! This module contains `SemaContext`, which holds all type information and
//! declarations that are immutable after the declaration gathering phase.
//! `SemaContext` is designed to be `Send + Sync` for future parallel function analysis.
//!
//! # Architecture
//!
//! The semantic analysis pipeline is split into two phases:
//!
//! 1. **Declaration gathering** (sequential): Builds the `SemaContext` with all
//!    type definitions, function signatures, and method signatures.
//!
//! 2. **Function body analysis** (parallelizable): Each function is analyzed
//!    using a `FunctionAnalyzer` that holds a reference to the shared `SemaContext`.
//!
//! This separation enables:
//! - Parallel type checking (each function can be analyzed independently)
//! - Better cache locality (immutable context can be shared)
//! - Foundation for incremental compilation (can cache `SemaContext` across compilations)

use std::collections::HashMap;

use lasso::{Spur, ThreadedRodeo};
use rue_error::PreviewFeatures;
use rue_rir::{Rir, RirParamMode};

use crate::inference::{FunctionSig, InferType, MethodSig};
use crate::types::{ArrayTypeDef, ArrayTypeId, EnumDef, EnumId, StructDef, StructId, Type};

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
    pub body: rue_rir::InstRef,
    /// Span of the method declaration
    pub span: rue_span::Span,
}

/// Pre-computed type information for constraint generation.
///
/// This struct holds the function, struct, enum, and method signature maps
/// converted to `InferType` format for use in Hindley-Milner type inference.
/// Building this once and reusing it for all function analyses avoids the
/// O(n²) cost of rebuilding these maps for each function.
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

/// Immutable context for semantic analysis.
///
/// This struct contains all type information and declarations that are
/// read-only after the declaration gathering phase. It is designed to be
/// `Send + Sync` so it can be shared across threads during parallel function
/// body analysis.
///
/// # Contents
///
/// - Struct and enum definitions
/// - Function and method signatures
/// - Array type registry
/// - Pre-computed inference context
/// - Built-in type IDs
///
/// # Thread Safety
///
/// `SemaContext` is `Send + Sync` because:
/// - All contained data is immutable after construction
/// - References to RIR and interner are shared immutably
/// - No `Cell`, `RefCell`, or other interior mutability types are used
#[derive(Debug)]
pub struct SemaContext<'a> {
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
    /// Pre-computed inference context for HM type inference.
    pub inference_ctx: InferenceContext,
}

// SAFETY: SemaContext contains only immutable data after construction.
// The references to RIR and ThreadedRodeo are shared immutably.
// ThreadedRodeo is designed to be thread-safe.
unsafe impl<'a> Send for SemaContext<'a> {}
unsafe impl<'a> Sync for SemaContext<'a> {}

impl<'a> SemaContext<'a> {
    /// Get the builtin String type as a Type::Struct.
    pub fn builtin_string_type(&self) -> Type {
        self.builtin_string_id
            .map(Type::Struct)
            .expect("String type should be registered during builtin injection")
    }

    /// Look up a struct by name.
    pub fn get_struct(&self, name: Spur) -> Option<StructId> {
        self.structs.get(&name).copied()
    }

    /// Get a struct definition by ID.
    pub fn get_struct_def(&self, id: StructId) -> &StructDef {
        &self.struct_defs[id.0 as usize]
    }

    /// Look up an enum by name.
    pub fn get_enum(&self, name: Spur) -> Option<EnumId> {
        self.enums.get(&name).copied()
    }

    /// Get an enum definition by ID.
    pub fn get_enum_def(&self, id: EnumId) -> &EnumDef {
        &self.enum_defs[id.0 as usize]
    }

    /// Look up a function by name.
    pub fn get_function(&self, name: Spur) -> Option<&FunctionInfo> {
        self.functions.get(&name)
    }

    /// Look up a method by type and method name.
    pub fn get_method(&self, type_name: Spur, method_name: Spur) -> Option<&MethodInfo> {
        self.methods.get(&(type_name, method_name))
    }

    /// Get an array type definition by ID.
    pub fn get_array_type_def(&self, id: ArrayTypeId) -> &ArrayTypeDef {
        &self.array_type_defs[id.0 as usize]
    }

    /// Look up an array type by element type and length.
    pub fn get_array_type(&self, element_type: Type, length: u64) -> Option<ArrayTypeId> {
        self.array_types.get(&(element_type, length)).copied()
    }

    /// Get a human-readable name for a type.
    pub fn format_type_name(&self, ty: Type) -> String {
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
            Type::Struct(struct_id) => self.struct_defs[struct_id.0 as usize].name.clone(),
            Type::Enum(enum_id) => self.enum_defs[enum_id.0 as usize].name.clone(),
            Type::Array(array_id) => {
                let array_def = &self.array_type_defs[array_id.0 as usize];
                format!(
                    "[{}; {}]",
                    self.format_type_name(array_def.element_type),
                    array_def.length
                )
            }
        }
    }

    /// Check if a type is a Copy type.
    pub fn is_type_copy(&self, ty: Type) -> bool {
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
                let struct_def = &self.struct_defs[struct_id.0 as usize];
                struct_def.is_copy
            }
            // Arrays are Copy if their element type is Copy
            Type::Array(array_id) => {
                let array_def = &self.array_type_defs[array_id.0 as usize];
                self.is_type_copy(array_def.element_type)
            }
        }
    }

    /// Get the number of ABI slots required for a type.
    pub fn abi_slot_count(&self, ty: Type) -> u32 {
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
            Type::Unit | Type::Never => 0,
            Type::Enum(_) => 1,
            Type::Struct(struct_id) => {
                let struct_def = &self.struct_defs[struct_id.0 as usize];
                struct_def
                    .fields
                    .iter()
                    .map(|f| self.abi_slot_count(f.ty))
                    .sum()
            }
            Type::Array(array_type_id) => {
                let array_def = &self.array_type_defs[array_type_id.0 as usize];
                let element_slots = self.abi_slot_count(array_def.element_type);
                element_slots * array_def.length as u32
            }
        }
    }

    /// Get the slot offset of a field within a struct.
    pub fn field_slot_offset(&self, struct_id: StructId, field_index: usize) -> u32 {
        let struct_def = &self.struct_defs[struct_id.0 as usize];
        struct_def.fields[..field_index]
            .iter()
            .map(|f| self.abi_slot_count(f.ty))
            .sum()
    }

    /// Convert a concrete Type to InferType for use in constraint generation.
    pub fn type_to_infer_type(&self, ty: Type) -> InferType {
        match ty {
            Type::Array(array_id) => {
                let array_def = &self.array_type_defs[array_id.0 as usize];
                let element_infer = self.type_to_infer_type(array_def.element_type);
                InferType::Array {
                    element: Box::new(element_infer),
                    length: array_def.length,
                }
            }
            _ => InferType::Concrete(ty),
        }
    }

    // ========================================================================
    // Builtin type helpers (duplicated from Sema for parallel analysis)
    // ========================================================================

    /// Check if a type is the builtin String type.
    pub fn is_builtin_string(&self, ty: Type) -> bool {
        match ty {
            Type::Struct(struct_id) => Some(struct_id) == self.builtin_string_id,
            _ => false,
        }
    }

    /// Get the builtin type definition for a struct if it's a builtin type.
    pub fn get_builtin_type_def(
        &self,
        struct_id: StructId,
    ) -> Option<&'static rue_builtins::BuiltinTypeDef> {
        let struct_def = &self.struct_defs[struct_id.0 as usize];
        if struct_def.is_builtin {
            rue_builtins::get_builtin_type(&struct_def.name)
        } else {
            None
        }
    }

    /// Check if a method name is a builtin mutation method.
    pub fn is_builtin_mutation_method(&self, method_name: &str) -> bool {
        use rue_builtins::{BUILTIN_TYPES, ReceiverMode};

        for builtin in BUILTIN_TYPES {
            if let Some(method) = builtin.find_method(method_name) {
                if method.receiver_mode == ReceiverMode::ByMutRef {
                    return true;
                }
            }
        }
        false
    }

    /// Get the AIR output type for a builtin struct.
    pub fn builtin_air_type(&self, struct_id: StructId) -> Type {
        Type::Struct(struct_id)
    }

    /// Check if a type is a linear type.
    pub fn is_type_linear(&self, ty: Type) -> bool {
        match ty {
            Type::Struct(struct_id) => {
                let struct_def = &self.struct_defs[struct_id.0 as usize];
                struct_def.is_linear
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time assertion that SemaContext is Send + Sync.
    /// This is critical for parallel function body analysis.
    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn test_sema_context_is_send_sync() {
        assert_send_sync::<SemaContext<'_>>();
    }
}
