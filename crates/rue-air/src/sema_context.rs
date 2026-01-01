//! Immutable semantic analysis context.
//!
//! This module contains `SemaContext`, which holds all type information and
//! declarations that are immutable after the declaration gathering phase.
//! `SemaContext` is designed to be `Send + Sync` for parallel function analysis.
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
//! - Better cache locality (context can be shared across threads)
//! - Foundation for incremental compilation (can cache `SemaContext` across compilations)
//!
//! # Array Type Registry
//!
//! The array type registry is thread-safe to support parallel function analysis.
//! Array types can be created during function body analysis when type inference
//! resolves array literals like `[1, 2, 3]` without explicit type annotations.
//! The registry uses `RwLock` for concurrent access with the following pattern:
//! - Read lock for lookups (most common case)
//! - Write lock for insertions (rare, only for new array types)

use std::collections::HashMap;
use std::sync::RwLock;

use lasso::{Spur, ThreadedRodeo};
use rue_error::PreviewFeatures;
use rue_rir::Rir;

use crate::inference::{FunctionSig, InferType, MethodSig};
use crate::intern_pool::TypeInternPool;
// Import FunctionInfo, MethodInfo, and KnownSymbols from sema module to avoid duplication.
// FunctionInfo and MethodInfo are the canonical definitions; we re-export them for convenience.
pub use crate::sema::{FunctionInfo, KnownSymbols, MethodInfo};
use crate::types::{ArrayTypeDef, ArrayTypeId, EnumDef, EnumId, StructDef, StructId, Type};

/// Thread-safe registry for array types.
///
/// This registry allows concurrent lookups and insertions of array types during
/// parallel function analysis. It uses double-checked locking to minimize
/// contention: most operations only need a read lock.
#[derive(Debug)]
pub struct ArrayTypeRegistry {
    /// Maps (element_type, length) to ArrayTypeId.
    types: RwLock<HashMap<(Type, u64), ArrayTypeId>>,
    /// Array type definitions indexed by ArrayTypeId.
    defs: RwLock<Vec<ArrayTypeDef>>,
}

impl ArrayTypeRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            types: RwLock::new(HashMap::new()),
            defs: RwLock::new(Vec::new()),
        }
    }

    /// Create a registry pre-populated with existing types.
    pub fn from_existing(
        types: HashMap<(Type, u64), ArrayTypeId>,
        defs: Vec<ArrayTypeDef>,
    ) -> Self {
        Self {
            types: RwLock::new(types),
            defs: RwLock::new(defs),
        }
    }

    /// Look up an array type by element type and length.
    pub fn get(&self, element_type: Type, length: u64) -> Option<ArrayTypeId> {
        self.types
            .read()
            .expect("ArrayTypeRegistry lock poisoned")
            .get(&(element_type, length))
            .copied()
    }

    /// Get or create an array type. Thread-safe with double-checked locking.
    pub fn get_or_create(&self, element_type: Type, length: u64) -> ArrayTypeId {
        let key = (element_type, length);

        // Fast path: check with read lock
        {
            let types = self.types.read().expect("ArrayTypeRegistry lock poisoned");
            if let Some(&id) = types.get(&key) {
                return id;
            }
        }

        // Slow path: acquire write lock and double-check
        let mut types = self.types.write().expect("ArrayTypeRegistry lock poisoned");

        // Double-check after acquiring write lock (another thread may have inserted)
        if let Some(&id) = types.get(&key) {
            return id;
        }

        // Create new array type
        let mut defs = self.defs.write().expect("ArrayTypeRegistry lock poisoned");
        let id = ArrayTypeId(defs.len() as u32);
        defs.push(ArrayTypeDef {
            element_type,
            length,
        });
        types.insert(key, id);
        id
    }

    /// Get an array type definition by ID.
    pub fn get_def(&self, id: ArrayTypeId) -> ArrayTypeDef {
        self.defs.read().expect("ArrayTypeRegistry lock poisoned")[id.0 as usize]
    }

    /// Get the number of registered array types.
    pub fn len(&self) -> usize {
        self.defs
            .read()
            .expect("ArrayTypeRegistry lock poisoned")
            .len()
    }

    /// Check if the registry is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Extract the array type definitions (consumes the registry).
    /// Used when building the final SemaOutput.
    pub fn into_defs(self) -> Vec<ArrayTypeDef> {
        self.defs
            .into_inner()
            .expect("ArrayTypeRegistry lock poisoned")
    }

    /// Get a snapshot of all array type definitions.
    pub fn snapshot_defs(&self) -> Vec<ArrayTypeDef> {
        self.defs
            .read()
            .expect("ArrayTypeRegistry lock poisoned")
            .clone()
    }
}

impl Default for ArrayTypeRegistry {
    fn default() -> Self {
        Self::new()
    }
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

/// Context for semantic analysis, designed for parallel function analysis.
///
/// This struct contains all type information and declarations needed during
/// function body analysis. It is designed to be `Send + Sync` so it can be
/// shared across threads during parallel function analysis.
///
/// # Contents
///
/// - Struct and enum definitions (immutable)
/// - Function and method signatures (references to immutable data in Sema)
/// - Array type registry (thread-safe, allows concurrent insertions)
/// - Pre-computed inference context (immutable)
/// - Built-in type IDs (immutable)
///
/// # Thread Safety
///
/// `SemaContext` is `Send + Sync` because:
/// - Most fields are immutable after construction
/// - The array type registry uses `RwLock` for thread-safe mutations
/// - References to RIR and interner are shared immutably
/// - References to functions/methods HashMaps are immutable after declaration gathering
/// - ThreadedRodeo is designed to be thread-safe
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
    /// Thread-safe array type registry.
    /// Supports concurrent lookups and insertions during parallel analysis.
    pub array_registry: ArrayTypeRegistry,
    /// Struct lookup: maps struct name symbol to StructId.
    pub structs: HashMap<Spur, StructId>,
    /// Enum lookup: maps enum name symbol to EnumId.
    pub enums: HashMap<Spur, EnumId>,
    /// Function lookup: reference to Sema's function map (immutable after declaration gathering).
    pub functions: &'a HashMap<Spur, FunctionInfo>,
    /// Method lookup: reference to Sema's method map (immutable after declaration gathering).
    pub methods: &'a HashMap<(Spur, Spur), MethodInfo>,
    /// Enabled preview features.
    pub preview_features: PreviewFeatures,
    /// StructId of the synthetic String type.
    pub builtin_string_id: Option<StructId>,
    /// Pre-computed inference context for HM type inference.
    pub inference_ctx: InferenceContext,
    /// Pre-interned known symbols for fast comparison.
    pub known: KnownSymbols,
    /// Type intern pool for unified type representation (ADR-0024 Phase 1).
    ///
    /// During Phase 1, the pool coexists with the existing type registries.
    /// It can be used for lookups but the canonical type representation
    /// remains the old `Type` enum. Later phases will migrate to using
    /// the pool exclusively.
    pub type_pool: TypeInternPool,
}

// SAFETY: SemaContext is Send + Sync because:
// - Immutable fields (struct_defs, enum_defs, structs, enums, etc.) are trivially thread-safe
// - ArrayTypeRegistry uses RwLock for interior mutability
// - TypeInternPool uses RwLock for interior mutability
// - References to RIR and ThreadedRodeo are shared immutably
// - References to functions/methods HashMaps are shared immutably (read-only after declaration gathering)
// - ThreadedRodeo is designed to be thread-safe
// - &HashMap<K, V> is Send + Sync when the HashMap is (immutable references are always safe)
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
    pub fn get_array_type_def(&self, id: ArrayTypeId) -> ArrayTypeDef {
        self.array_registry.get_def(id)
    }

    /// Look up an array type by element type and length.
    pub fn get_array_type(&self, element_type: Type, length: u64) -> Option<ArrayTypeId> {
        self.array_registry.get(element_type, length)
    }

    /// Get or create an array type. Thread-safe.
    pub fn get_or_create_array_type(&self, element_type: Type, length: u64) -> ArrayTypeId {
        self.array_registry.get_or_create(element_type, length)
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
                let array_def = self.array_registry.get_def(array_id);
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
                let array_def = self.array_registry.get_def(array_id);
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
                let array_def = self.array_registry.get_def(array_type_id);
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
                let array_def = self.array_registry.get_def(array_id);
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
