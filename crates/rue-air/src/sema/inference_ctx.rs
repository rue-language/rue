//! Pre-computed type information for constraint generation.
//!
//! This module contains [`InferenceContext`] which holds function, struct, enum,
//! and method signature maps in `InferType` format for use in Hindley-Milner
//! type inference.

use std::collections::HashMap;

use lasso::Spur;
use rue_span::FileId;

use crate::inference::{FunctionSig, MethodSig};
use crate::types::{StructId, Type};

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
    /// Struct types: name -> Type::new_struct(id).
    pub struct_types: HashMap<Spur, Type>,
    /// Enum types: name -> Type::new_enum(id).
    pub enum_types: HashMap<Spur, Type>,
    /// Method signatures with InferType: (struct_id, method_name) -> MethodSig.
    pub method_sigs: HashMap<(StructId, Spur), MethodSig>,
    /// File-level constant types: name -> declared type (e.g. `Type::I32` for
    /// `const N: i32 = 41`, a module type for `const m = @import(...)`). Constant
    /// types are fully resolved during declaration gathering, before any
    /// function body is inferred, so they can be consulted like params here.
    /// Without this map a const reference inferred to `<error>` and poisoned
    /// every expression it touched (RUE-142).
    pub const_types: HashMap<Spur, Type>,
    /// Module-binding types (`const utils = @import(...)`): (declaring file,
    /// name) -> module type. Module bindings are per-file scoped (RUE-113),
    /// so they're keyed by file rather than living in `const_types`.
    pub module_binding_types: HashMap<(FileId, Spur), Type>,
}
