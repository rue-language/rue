//! Information types for functions, methods, and constants.
//!
//! These types store metadata about declarations gathered during the first
//! phase of semantic analysis. They are used to resolve function calls,
//! method calls, and constant references during function body analysis.

use lasso::Spur;
use rue_span::{FileId, Span};

use crate::param_arena::ParamRange;
use crate::types::Type;

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
    pub span: Span,
    /// Whether this function has any comptime type parameters
    pub is_generic: bool,
    /// Whether this function is public (visible outside its directory)
    pub is_pub: bool,
    /// File ID this function was declared in (for visibility checking)
    pub file_id: FileId,
}

/// Information about a method in an impl block.
///
/// Note: Captured comptime values for anonymous struct methods are stored at the
/// struct level in `Sema::anon_struct_captured_values`, not per-method. This ensures
/// that different instantiations with different captured values create different types.
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
    pub span: Span,
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
    pub span: Span,
}
