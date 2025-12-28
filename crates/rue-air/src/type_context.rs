//! Type context for semantic analysis.
//!
//! This module contains the immutable type information that can be shared
//! across parallel function analysis. The `TypeContext` is built during the
//! collection phase and then shared (via `&` or `Arc`) during function analysis.

use std::collections::HashMap;

use rue_intern::Symbol;
use rue_rir::RirParamMode;

use crate::types::{EnumDef, EnumId, StructDef, StructId, Type};

/// Immutable type context for semantic analysis.
///
/// This struct contains all type information that is read-only after the
/// collection phase. It can be safely shared across threads during parallel
/// function analysis.
///
/// # Separation of Concerns
///
/// The `TypeContext` contains:
/// - Type definitions (structs, enums)
/// - Function and method signatures
/// - Type-to-ID mappings
///
/// It does NOT contain:
/// - Array type table (created during analysis)
/// - String table (created during analysis)
/// - Warnings (collected during analysis)
///
/// Those mutable items live in `FunctionAnalysisState` (per-function) and
/// are merged after parallel analysis completes.
#[derive(Debug, Clone)]
pub struct TypeContext {
    /// Function signatures: maps function name to signature info.
    pub func_sigs: HashMap<Symbol, FunctionSignature>,
    /// Method signatures: maps (type_name, method_name) to signature info.
    pub method_sigs: HashMap<(Symbol, Symbol), MethodSignature>,
    /// Struct lookup: maps struct name symbol to StructId.
    pub struct_by_name: HashMap<Symbol, StructId>,
    /// Struct definitions indexed by StructId.
    pub struct_defs: Vec<StructDef>,
    /// Enum lookup: maps enum name symbol to EnumId.
    pub enum_by_name: HashMap<Symbol, EnumId>,
    /// Enum definitions indexed by EnumId.
    pub enum_defs: Vec<EnumDef>,
}

/// Signature information for a function.
#[derive(Debug, Clone)]
pub struct FunctionSignature {
    /// Parameter types in order.
    pub param_types: Vec<Type>,
    /// Parameter passing modes in order.
    pub param_modes: Vec<RirParamMode>,
    /// Return type.
    pub return_type: Type,
}

/// Signature information for a method.
#[derive(Debug, Clone)]
pub struct MethodSignature {
    /// The type this method belongs to (as a StructId for lookup).
    pub struct_id: StructId,
    /// The struct type (Type::Struct(struct_id)).
    pub struct_type: Type,
    /// Whether this is a method (has self) or associated function (no self).
    pub has_self: bool,
    /// Parameter names (excluding self if present).
    pub param_names: Vec<Symbol>,
    /// Parameter types (excluding self if present).
    pub param_types: Vec<Type>,
    /// Return type.
    pub return_type: Type,
}

impl TypeContext {
    /// Create a new empty TypeContext.
    pub fn new() -> Self {
        Self {
            func_sigs: HashMap::new(),
            method_sigs: HashMap::new(),
            struct_by_name: HashMap::new(),
            struct_defs: Vec::new(),
            enum_by_name: HashMap::new(),
            enum_defs: Vec::new(),
        }
    }

    /// Look up a struct by name.
    pub fn get_struct(&self, name: Symbol) -> Option<StructId> {
        self.struct_by_name.get(&name).copied()
    }

    /// Get a struct definition by ID.
    pub fn get_struct_def(&self, id: StructId) -> &StructDef {
        &self.struct_defs[id.0 as usize]
    }

    /// Look up an enum by name.
    pub fn get_enum(&self, name: Symbol) -> Option<EnumId> {
        self.enum_by_name.get(&name).copied()
    }

    /// Get an enum definition by ID.
    pub fn get_enum_def(&self, id: EnumId) -> &EnumDef {
        &self.enum_defs[id.0 as usize]
    }

    /// Look up a function signature by name.
    pub fn get_function(&self, name: Symbol) -> Option<&FunctionSignature> {
        self.func_sigs.get(&name)
    }

    /// Look up a method signature by type and method name.
    pub fn get_method(&self, type_name: Symbol, method_name: Symbol) -> Option<&MethodSignature> {
        self.method_sigs.get(&(type_name, method_name))
    }
}

impl Default for TypeContext {
    fn default() -> Self {
        Self::new()
    }
}
