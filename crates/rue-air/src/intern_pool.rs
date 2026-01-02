//! Type intern pool for efficient type representation.
//!
//! This module implements a unified type interning system inspired by Zig's `InternPool`.
//! All types become 32-bit indices into a canonical pool, enabling:
//!
//! - O(1) type equality (u32 comparison)
//! - Efficient memory usage
//! - Clean parallel compilation (no per-function type merging)
//! - Foundation for future generics
//!
//! # Architecture
//!
//! The `TypeInternPool` serves as a canonical repository for all composite types:
//! - **Structs and enums** are nominal types (same name = same type)
//! - **Arrays** are structural types (same element type + length = same type)
//!
//! Primitive types (i8-i64, u8-u64, bool, unit, never, error) are encoded directly
//! in the `InternedType` index using reserved indices 0-15, requiring no pool lookup.
//!
//! # Migration Strategy (ADR-0024)
//!
//! This module is part of Phase 1 of the Type Intern Pool migration:
//! - Phase 1: Introduce pool alongside existing system (this module)
//! - Phase 2: Migrate array types to the pool
//! - Phase 3: Migrate struct/enum IDs to pool indices
//! - Phase 4: Unify Type representation to `InternedType(u32)`
//!
//! During Phase 1, the pool coexists with the existing `Type` enum, `StructId`,
//! `EnumId`, and `ArrayTypeId`. The pool is populated during declaration collection
//! but not yet used for type operations.
//!
//! # Thread Safety
//!
//! The pool uses `RwLock` for thread-safe access during parallel compilation:
//! - Read lock for lookups (common case)
//! - Write lock for insertions (rare, during declaration gathering)

use std::collections::HashMap;
use std::sync::RwLock;

use lasso::Spur;

use crate::types::{ArrayTypeId, EnumDef, EnumId, StructDef, StructId, Type, TypeKind};

/// Interned type index - 32 bits, Copy, cheap comparison.
///
/// Reserved indices 0-15 are primitives (no lookup needed).
/// Index 16+ are composite types stored in the pool.
///
/// # Primitive Encoding
///
/// The following indices are reserved for primitive types:
/// - 0: i8
/// - 1: i16
/// - 2: i32
/// - 3: i64
/// - 4: u8
/// - 5: u16
/// - 6: u32
/// - 7: u64
/// - 8: bool
/// - 9: unit
/// - 10: never
/// - 11: error
/// - 12-15: reserved for future primitives
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct InternedType(u32);

impl InternedType {
    // Reserved indices for primitives
    pub const I8: InternedType = InternedType(0);
    pub const I16: InternedType = InternedType(1);
    pub const I32: InternedType = InternedType(2);
    pub const I64: InternedType = InternedType(3);
    pub const U8: InternedType = InternedType(4);
    pub const U16: InternedType = InternedType(5);
    pub const U32: InternedType = InternedType(6);
    pub const U64: InternedType = InternedType(7);
    pub const BOOL: InternedType = InternedType(8);
    pub const UNIT: InternedType = InternedType(9);
    pub const NEVER: InternedType = InternedType(10);
    pub const ERROR: InternedType = InternedType(11);

    const PRIMITIVE_COUNT: u32 = 16;

    /// Check if this is a primitive type (no pool lookup needed).
    #[inline]
    pub fn is_primitive(self) -> bool {
        self.0 < Self::PRIMITIVE_COUNT
    }

    /// Get the raw index value.
    #[inline]
    pub fn index(self) -> u32 {
        self.0
    }

    /// Create an InternedType from a raw index.
    ///
    /// # Safety
    ///
    /// The caller must ensure the index is valid (either a primitive index 0-15,
    /// or a composite index that exists in the pool).
    #[inline]
    pub fn from_raw(index: u32) -> Self {
        InternedType(index)
    }

    /// Create an InternedType for a composite type from its pool index.
    ///
    /// The pool index is offset by `PRIMITIVE_COUNT` to produce the final index.
    #[inline]
    fn from_pool_index(pool_index: u32) -> Self {
        InternedType(pool_index + Self::PRIMITIVE_COUNT)
    }

    /// Get the pool index for a composite type.
    ///
    /// Returns `None` for primitive types.
    #[inline]
    pub fn pool_index(self) -> Option<u32> {
        if self.is_primitive() {
            None
        } else {
            Some(self.0 - Self::PRIMITIVE_COUNT)
        }
    }
}

impl std::fmt::Debug for InternedType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_primitive() {
            let name = match self.0 {
                0 => "i8",
                1 => "i16",
                2 => "i32",
                3 => "i64",
                4 => "u8",
                5 => "u16",
                6 => "u32",
                7 => "u64",
                8 => "bool",
                9 => "()",
                10 => "!",
                11 => "<error>",
                _ => "<reserved>",
            };
            write!(f, "InternedType({name})")
        } else {
            write!(f, "InternedType(pool:{})", self.0 - Self::PRIMITIVE_COUNT)
        }
    }
}

/// Type data stored in the intern pool.
///
/// This is NOT Copy - it lives in the pool. You work with `InternedType` indices.
///
/// # Type Categories
///
/// - **Struct** and **Enum** are nominal types: identity comes from the name
/// - **Array** is a structural type: identity comes from element type + length
#[derive(Debug, Clone)]
pub enum TypeData {
    /// User-defined struct (nominal type).
    ///
    /// Two structs with the same fields but different names are different types.
    Struct(StructData),

    /// User-defined enum (nominal type).
    ///
    /// Two enums with the same variants but different names are different types.
    Enum(EnumData),

    /// Fixed-size array (structural type).
    ///
    /// Arrays with the same element type and length are the same type,
    /// regardless of where they were defined.
    Array { element: InternedType, len: u64 },
}

/// Data for a struct type in the intern pool.
///
/// During Phase 1, this mirrors the existing `StructDef` to verify correctness.
/// In later phases, `StructDef` will be replaced by this.
#[derive(Debug, Clone)]
pub struct StructData {
    /// The name symbol (interned string).
    pub name: Spur,
    /// Reference to the full struct definition.
    /// During Phase 1, we keep a clone of the StructDef for verification.
    /// In later phases, the pool will be the canonical source.
    pub def: StructDef,
}

/// Data for an enum type in the intern pool.
///
/// During Phase 1, this mirrors the existing `EnumDef` to verify correctness.
/// In later phases, `EnumDef` will be replaced by this.
#[derive(Debug, Clone)]
pub struct EnumData {
    /// The name symbol (interned string).
    pub name: Spur,
    /// Reference to the full enum definition.
    /// During Phase 1, we keep a clone of the EnumDef for verification.
    /// In later phases, the pool will be the canonical source.
    pub def: EnumDef,
}

/// Thread-safe intern pool for all composite types.
///
/// The pool is designed to be built during declaration gathering (sequential)
/// and then queried during function body analysis (potentially parallel).
///
/// # Thread Safety
///
/// Uses `RwLock` for interior mutability:
/// - Read lock for lookups (most common)
/// - Write lock for insertions (only during declaration gathering)
///
/// # Usage
///
/// ```ignore
/// let pool = TypeInternPool::new();
///
/// // Register nominal types (structs/enums)
/// let (struct_type, is_new) = pool.register_struct(name_spur, struct_def);
///
/// // Intern structural types (arrays)
/// let array_type = pool.intern_array(element_type, 10);
///
/// // Look up type data
/// if let Some(data) = pool.try_get(some_type) {
///     match data {
///         TypeData::Struct(s) => println!("struct {}", s.def.name),
///         TypeData::Enum(e) => println!("enum {}", e.def.name),
///         TypeData::Array { element, len } => println!("array of {:?}; {}", element, len),
///     }
/// }
/// ```
#[derive(Debug)]
pub struct TypeInternPool {
    inner: RwLock<TypeInternPoolInner>,
}

#[derive(Debug)]
struct TypeInternPoolInner {
    /// All composite type data, indexed by (InternedType.0 - PRIMITIVE_COUNT).
    types: Vec<TypeData>,

    /// Structural type deduplication: (element, len) -> InternedType for arrays.
    array_map: HashMap<(InternedType, u64), InternedType>,

    /// Nominal type lookup: name -> InternedType for structs.
    struct_by_name: HashMap<Spur, InternedType>,

    /// Nominal type lookup: name -> InternedType for enums.
    enum_by_name: HashMap<Spur, InternedType>,
}

impl TypeInternPool {
    /// Create a new empty pool.
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(TypeInternPoolInner {
                types: Vec::new(),
                array_map: HashMap::new(),
                struct_by_name: HashMap::new(),
                enum_by_name: HashMap::new(),
            }),
        }
    }

    /// Register a new struct (nominal - no deduplication).
    ///
    /// Returns the `StructId` (containing the pool index) and whether it was newly inserted.
    /// If a struct with this name already exists, returns the existing StructId.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned.
    pub fn register_struct(&self, name: Spur, def: StructDef) -> (StructId, bool) {
        // Fast path: check with read lock
        {
            let inner = self.inner.read().expect("TypeInternPool lock poisoned");
            if let Some(&existing) = inner.struct_by_name.get(&name) {
                // Convert InternedType back to StructId via pool_index
                let pool_index = existing.pool_index().expect("struct must have pool index");
                return (StructId::from_pool_index(pool_index), false);
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().expect("TypeInternPool lock poisoned");

        // Double-check after acquiring write lock
        if let Some(&existing) = inner.struct_by_name.get(&name) {
            let pool_index = existing.pool_index().expect("struct must have pool index");
            return (StructId::from_pool_index(pool_index), false);
        }

        // Create new struct type
        let pool_index = inner.types.len() as u32;
        let interned = InternedType::from_pool_index(pool_index);

        inner.types.push(TypeData::Struct(StructData { name, def }));
        inner.struct_by_name.insert(name, interned);

        (StructId::from_pool_index(pool_index), true)
    }

    /// Register a new enum (nominal - no deduplication).
    ///
    /// Returns the `EnumId` (containing the pool index) and whether it was newly inserted.
    /// If an enum with this name already exists, returns the existing EnumId.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned.
    pub fn register_enum(&self, name: Spur, def: EnumDef) -> (EnumId, bool) {
        // Fast path: check with read lock
        {
            let inner = self.inner.read().expect("TypeInternPool lock poisoned");
            if let Some(&existing) = inner.enum_by_name.get(&name) {
                let pool_index = existing.pool_index().expect("enum must have pool index");
                return (EnumId::from_pool_index(pool_index), false);
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().expect("TypeInternPool lock poisoned");

        // Double-check after acquiring write lock
        if let Some(&existing) = inner.enum_by_name.get(&name) {
            let pool_index = existing.pool_index().expect("enum must have pool index");
            return (EnumId::from_pool_index(pool_index), false);
        }

        // Create new enum type
        let pool_index = inner.types.len() as u32;
        let interned = InternedType::from_pool_index(pool_index);

        inner.types.push(TypeData::Enum(EnumData { name, def }));
        inner.enum_by_name.insert(name, interned);

        (EnumId::from_pool_index(pool_index), true)
    }

    /// Intern an array type (structural - deduplicates).
    ///
    /// Returns the canonical `InternedType` for arrays with this element type and length.
    /// If an identical array type already exists, returns the existing type.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned.
    pub fn intern_array(&self, element: InternedType, len: u64) -> InternedType {
        let key = (element, len);

        // Fast path: check with read lock
        {
            let inner = self.inner.read().expect("TypeInternPool lock poisoned");
            if let Some(&existing) = inner.array_map.get(&key) {
                return existing;
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().expect("TypeInternPool lock poisoned");

        // Double-check after acquiring write lock
        if let Some(&existing) = inner.array_map.get(&key) {
            return existing;
        }

        // Create new array type
        let pool_index = inner.types.len() as u32;
        let interned = InternedType::from_pool_index(pool_index);

        inner.types.push(TypeData::Array { element, len });
        inner.array_map.insert(key, interned);

        interned
    }

    /// Look up a struct by name.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned.
    pub fn get_struct_by_name(&self, name: Spur) -> Option<InternedType> {
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        inner.struct_by_name.get(&name).copied()
    }

    /// Look up an enum by name.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned.
    pub fn get_enum_by_name(&self, name: Spur) -> Option<InternedType> {
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        inner.enum_by_name.get(&name).copied()
    }

    /// Look up an array type by element and length.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned.
    pub fn get_array(&self, element: InternedType, len: u64) -> Option<InternedType> {
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        inner.array_map.get(&(element, len)).copied()
    }

    /// Get type data for a composite type.
    ///
    /// Returns `None` for primitive types (use `InternedType::is_primitive()` first).
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned or if the index is invalid.
    pub fn get(&self, ty: InternedType) -> Option<TypeData> {
        if ty.is_primitive() {
            return None;
        }

        let pool_index = ty.pool_index().expect("non-primitive must have pool index");
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        Some(inner.types[pool_index as usize].clone())
    }

    /// Check if this is a struct type.
    pub fn is_struct(&self, ty: InternedType) -> bool {
        if ty.is_primitive() {
            return false;
        }
        matches!(self.get(ty), Some(TypeData::Struct(_)))
    }

    /// Check if this is an enum type.
    pub fn is_enum(&self, ty: InternedType) -> bool {
        if ty.is_primitive() {
            return false;
        }
        matches!(self.get(ty), Some(TypeData::Enum(_)))
    }

    /// Check if this is an array type.
    pub fn is_array(&self, ty: InternedType) -> bool {
        if ty.is_primitive() {
            return false;
        }
        matches!(self.get(ty), Some(TypeData::Array { .. }))
    }

    /// Get the struct definition if this is a struct type.
    pub fn get_struct_def(&self, ty: InternedType) -> Option<StructDef> {
        match self.get(ty)? {
            TypeData::Struct(data) => Some(data.def),
            _ => None,
        }
    }

    /// Get the enum definition if this is an enum type.
    pub fn get_enum_def(&self, ty: InternedType) -> Option<EnumDef> {
        match self.get(ty)? {
            TypeData::Enum(data) => Some(data.def),
            _ => None,
        }
    }

    /// Get array info (element type, length) if this is an array type.
    pub fn get_array_info(&self, ty: InternedType) -> Option<(InternedType, u64)> {
        match self.get(ty)? {
            TypeData::Array { element, len } => Some((element, len)),
            _ => None,
        }
    }

    // ========================================================================
    // Phase 3 helpers: Direct StructId/EnumId access
    // ========================================================================
    //
    // These methods allow accessing struct and enum definitions directly via
    // StructId/EnumId, which now store pool indices instead of vector indices.

    /// Get a struct definition by StructId.
    ///
    /// The StructId contains a pool index. This method looks up the struct
    /// in the pool and returns a clone of its definition.
    ///
    /// # Panics
    ///
    /// Panics if the StructId doesn't correspond to a struct in the pool.
    pub fn struct_def(&self, struct_id: StructId) -> StructDef {
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        let pool_index = struct_id.0 as usize;
        match &inner.types[pool_index] {
            TypeData::Struct(data) => data.def.clone(),
            other => panic!(
                "Expected struct at pool index {}, got {:?}",
                pool_index, other
            ),
        }
    }

    /// Get an enum definition by EnumId.
    ///
    /// The EnumId contains a pool index. This method looks up the enum
    /// in the pool and returns a clone of its definition.
    ///
    /// # Panics
    ///
    /// Panics if the EnumId doesn't correspond to an enum in the pool.
    pub fn enum_def(&self, enum_id: EnumId) -> EnumDef {
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        let pool_index = enum_id.0 as usize;
        match &inner.types[pool_index] {
            TypeData::Enum(data) => data.def.clone(),
            other => panic!(
                "Expected enum at pool index {}, got {:?}",
                pool_index, other
            ),
        }
    }

    /// Update a struct definition in the pool.
    ///
    /// This is used during semantic analysis when struct fields are resolved
    /// after the struct is initially registered.
    ///
    /// # Panics
    ///
    /// Panics if the StructId doesn't correspond to a struct in the pool.
    pub fn update_struct_def(&self, struct_id: StructId, new_def: StructDef) {
        let mut inner = self.inner.write().expect("TypeInternPool lock poisoned");
        let pool_index = struct_id.0 as usize;
        match &mut inner.types[pool_index] {
            TypeData::Struct(data) => data.def = new_def,
            other => panic!(
                "Expected struct at pool index {}, got {:?}",
                pool_index, other
            ),
        }
    }

    /// Update an enum definition in the pool.
    ///
    /// This is used during semantic analysis when enum variants are resolved
    /// after the enum is initially registered.
    ///
    /// # Panics
    ///
    /// Panics if the EnumId doesn't correspond to an enum in the pool.
    pub fn update_enum_def(&self, enum_id: EnumId, new_def: EnumDef) {
        let mut inner = self.inner.write().expect("TypeInternPool lock poisoned");
        let pool_index = enum_id.0 as usize;
        match &mut inner.types[pool_index] {
            TypeData::Enum(data) => data.def = new_def,
            other => panic!(
                "Expected enum at pool index {}, got {:?}",
                pool_index, other
            ),
        }
    }

    /// Convert a StructId to an InternedType.
    ///
    /// Since StructId now contains a pool index, we just add the primitive offset.
    #[inline]
    pub fn struct_id_to_interned(&self, struct_id: StructId) -> InternedType {
        InternedType::from_pool_index(struct_id.0)
    }

    /// Convert an EnumId to an InternedType.
    ///
    /// Since EnumId now contains a pool index, we just add the primitive offset.
    #[inline]
    pub fn enum_id_to_interned(&self, enum_id: EnumId) -> InternedType {
        InternedType::from_pool_index(enum_id.0)
    }

    /// Get an array type definition by ArrayTypeId.
    ///
    /// The ArrayTypeId contains a pool index. This method looks up the array
    /// in the pool and returns its element type and length as a tuple.
    ///
    /// # Returns
    ///
    /// Returns `(element_type, length)` where `element_type` is the array's element type
    /// and `length` is the array's fixed size.
    ///
    /// # Panics
    ///
    /// Panics if the ArrayTypeId doesn't correspond to an array in the pool.
    pub fn array_def(&self, array_id: ArrayTypeId) -> (Type, u64) {
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        let pool_index = array_id.0 as usize;
        match &inner.types[pool_index] {
            TypeData::Array { element, len } => {
                // Convert InternedType back to Type
                let element_type = Self::interned_to_type_recursive(*element, &inner);
                (element_type, *len)
            }
            other => panic!(
                "Expected array at pool index {}, got {:?}",
                pool_index, other
            ),
        }
    }

    /// Intern an array type from a Type element (for Phase 2B migration).
    ///
    /// This is a helper method that converts the Type to InternedType
    /// and then interns the array. Used during migration from ArrayTypeRegistry.
    ///
    /// # Panics
    ///
    /// Panics if the element type contains a struct/enum that isn't in the pool.
    pub fn intern_array_from_type(&self, element_type: Type, len: u64) -> ArrayTypeId {
        let element_interned = Self::type_to_interned_recursive(element_type);
        let array_interned = self.intern_array(element_interned, len);
        ArrayTypeId::from_pool_index(
            array_interned
                .pool_index()
                .expect("array must have pool index"),
        )
    }

    /// Look up an array type by Type element and length (for Phase 2B migration).
    ///
    /// Returns None if no such array exists in the pool.
    pub fn get_array_by_type(&self, element_type: Type, len: u64) -> Option<ArrayTypeId> {
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        let element_interned = Self::type_to_interned_recursive(element_type);
        let array_interned = inner.array_map.get(&(element_interned, len))?;
        Some(ArrayTypeId::from_pool_index(
            array_interned
                .pool_index()
                .expect("array must have pool index"),
        ))
    }

    /// Convert InternedType to Type recursively (handles composite types).
    ///
    /// This is used to convert array element types from InternedType back to Type.
    fn interned_to_type_recursive(ty: InternedType, inner: &TypeInternPoolInner) -> Type {
        if ty.is_primitive() {
            return match ty.0 {
                0 => Type::I8,
                1 => Type::I16,
                2 => Type::I32,
                3 => Type::I64,
                4 => Type::U8,
                5 => Type::U16,
                6 => Type::U32,
                7 => Type::U64,
                8 => Type::Bool,
                9 => Type::Unit,
                10 => Type::Never,
                11 => Type::Error,
                _ => panic!("Unknown primitive index: {}", ty.0),
            };
        }

        let pool_index = ty.pool_index().expect("non-primitive must have pool index");
        match &inner.types[pool_index as usize] {
            TypeData::Struct(_) => Type::Struct(StructId::from_pool_index(pool_index)),
            TypeData::Enum(_) => Type::Enum(EnumId::from_pool_index(pool_index)),
            TypeData::Array { .. } => Type::Array(ArrayTypeId::from_pool_index(pool_index)),
        }
    }

    /// Convert Type to InternedType recursively (handles composite types).
    ///
    /// This is used during Phase 2B migration to convert Type to InternedType
    /// for array interning.
    fn type_to_interned_recursive(ty: Type) -> InternedType {
        match ty.kind() {
            TypeKind::I8 => InternedType::I8,
            TypeKind::I16 => InternedType::I16,
            TypeKind::I32 => InternedType::I32,
            TypeKind::I64 => InternedType::I64,
            TypeKind::U8 => InternedType::U8,
            TypeKind::U16 => InternedType::U16,
            TypeKind::U32 => InternedType::U32,
            TypeKind::U64 => InternedType::U64,
            TypeKind::Bool => InternedType::BOOL,
            TypeKind::Unit => InternedType::UNIT,
            TypeKind::Never => InternedType::NEVER,
            TypeKind::Error => InternedType::ERROR,
            TypeKind::Struct(id) => InternedType::from_pool_index(id.pool_index()),
            TypeKind::Enum(id) => InternedType::from_pool_index(id.pool_index()),
            TypeKind::Array(id) => InternedType::from_pool_index(id.pool_index()),
            TypeKind::Module(_) => panic!("Cannot intern module types"),
            TypeKind::ComptimeType => panic!("Cannot intern comptime types"),
        }
    }

    /// Convert an ArrayTypeId to an InternedType.
    ///
    /// Since ArrayTypeId now contains a pool index, we just add the primitive offset.
    #[inline]
    pub fn array_id_to_interned(&self, array_id: ArrayTypeId) -> InternedType {
        InternedType::from_pool_index(array_id.0)
    }

    /// Get all struct IDs registered in the pool.
    ///
    /// Returns a vector of all StructId values, useful for iterating over all
    /// structs (e.g., for drop glue synthesis).
    pub fn all_struct_ids(&self) -> Vec<StructId> {
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        inner
            .struct_by_name
            .values()
            .map(|interned| {
                let pool_index = interned.pool_index().expect("struct must have pool index");
                StructId::from_pool_index(pool_index)
            })
            .collect()
    }

    /// Get all enum IDs registered in the pool.
    ///
    /// Returns a vector of all EnumId values, useful for iterating over all
    /// enums.
    pub fn all_enum_ids(&self) -> Vec<EnumId> {
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        inner
            .enum_by_name
            .values()
            .map(|interned| {
                let pool_index = interned.pool_index().expect("enum must have pool index");
                EnumId::from_pool_index(pool_index)
            })
            .collect()
    }

    /// Get all array IDs registered in the pool.
    ///
    /// Returns a vector of all ArrayTypeId values, useful for iterating over all
    /// arrays (e.g., for drop glue synthesis).
    pub fn all_array_ids(&self) -> Vec<ArrayTypeId> {
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        inner
            .types
            .iter()
            .enumerate()
            .filter_map(|(idx, data)| match data {
                TypeData::Array { .. } => Some(ArrayTypeId(idx as u32)),
                _ => None,
            })
            .collect()
    }

    /// Get the number of composite types in the pool.
    pub fn len(&self) -> usize {
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        inner.types.len()
    }

    /// Check if the pool is empty (no composite types).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get statistics about the pool contents.
    pub fn stats(&self) -> TypeInternPoolStats {
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        let mut struct_count = 0;
        let mut enum_count = 0;
        let mut array_count = 0;

        for data in &inner.types {
            match data {
                TypeData::Struct(_) => struct_count += 1,
                TypeData::Enum(_) => enum_count += 1,
                TypeData::Array { .. } => array_count += 1,
            }
        }

        TypeInternPoolStats {
            struct_count,
            enum_count,
            array_count,
            total: inner.types.len(),
        }
    }

    // ========================================================================
    // Conversion helpers for migration (Phase 1)
    // ========================================================================

    /// Convert an old-style `Type` to an `InternedType`.
    ///
    /// This is a temporary helper for Phase 1 migration. It converts the
    /// existing `Type` enum to the new interned representation.
    ///
    /// # Note
    ///
    /// For struct/enum types, the corresponding type must already be registered
    /// in the pool. For array types, this returns an error since array interning
    /// requires the pool to already have the element type interned.
    pub fn type_to_interned(&self, ty: Type) -> Option<InternedType> {
        match ty.kind() {
            TypeKind::I8 => Some(InternedType::I8),
            TypeKind::I16 => Some(InternedType::I16),
            TypeKind::I32 => Some(InternedType::I32),
            TypeKind::I64 => Some(InternedType::I64),
            TypeKind::U8 => Some(InternedType::U8),
            TypeKind::U16 => Some(InternedType::U16),
            TypeKind::U32 => Some(InternedType::U32),
            TypeKind::U64 => Some(InternedType::U64),
            TypeKind::Bool => Some(InternedType::BOOL),
            TypeKind::Unit => Some(InternedType::UNIT),
            TypeKind::Never => Some(InternedType::NEVER),
            TypeKind::Error => Some(InternedType::ERROR),
            // Struct, enum, array, and module require pool lookup by ID - we need the name
            // to find the interned type. This conversion is not straightforward
            // without additional context. Return None to indicate we can't convert.
            TypeKind::Struct(_) | TypeKind::Enum(_) | TypeKind::Array(_) | TypeKind::Module(_) => {
                None
            }
            // ComptimeType is a comptime-only type, cannot be interned for runtime
            TypeKind::ComptimeType => None,
        }
    }

    /// Convert an `InternedType` back to the old-style `Type`.
    ///
    /// This is a temporary helper for Phase 1 migration to verify correctness.
    /// Returns `None` for composite types since we need the old IDs.
    pub fn interned_to_type(&self, ty: InternedType) -> Option<Type> {
        if !ty.is_primitive() {
            return None;
        }

        Some(match ty.0 {
            0 => Type::I8,
            1 => Type::I16,
            2 => Type::I32,
            3 => Type::I64,
            4 => Type::U8,
            5 => Type::U16,
            6 => Type::U32,
            7 => Type::U64,
            8 => Type::Bool,
            9 => Type::Unit,
            10 => Type::Never,
            11 => Type::Error,
            _ => return None,
        })
    }
}

impl Default for TypeInternPool {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for TypeInternPool {
    /// Clone the pool by copying all type data into a new pool.
    ///
    /// This is used when building `SemaContext` from `Sema`, as the context
    /// needs its own copy of the pool for thread-safe sharing.
    fn clone(&self) -> Self {
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        Self {
            inner: RwLock::new(TypeInternPoolInner {
                types: inner.types.clone(),
                array_map: inner.array_map.clone(),
                struct_by_name: inner.struct_by_name.clone(),
                enum_by_name: inner.enum_by_name.clone(),
            }),
        }
    }
}

/// Statistics about the intern pool contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeInternPoolStats {
    pub struct_count: usize,
    pub enum_count: usize,
    pub array_count: usize,
    pub total: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use lasso::ThreadedRodeo;

    // ========================================================================
    // InternedType tests
    // ========================================================================

    #[test]
    fn test_interned_type_primitives() {
        assert!(InternedType::I8.is_primitive());
        assert!(InternedType::I16.is_primitive());
        assert!(InternedType::I32.is_primitive());
        assert!(InternedType::I64.is_primitive());
        assert!(InternedType::U8.is_primitive());
        assert!(InternedType::U16.is_primitive());
        assert!(InternedType::U32.is_primitive());
        assert!(InternedType::U64.is_primitive());
        assert!(InternedType::BOOL.is_primitive());
        assert!(InternedType::UNIT.is_primitive());
        assert!(InternedType::NEVER.is_primitive());
        assert!(InternedType::ERROR.is_primitive());
    }

    #[test]
    fn test_interned_type_indices() {
        assert_eq!(InternedType::I8.index(), 0);
        assert_eq!(InternedType::I16.index(), 1);
        assert_eq!(InternedType::I32.index(), 2);
        assert_eq!(InternedType::I64.index(), 3);
        assert_eq!(InternedType::U8.index(), 4);
        assert_eq!(InternedType::BOOL.index(), 8);
        assert_eq!(InternedType::UNIT.index(), 9);
    }

    #[test]
    fn test_interned_type_pool_index() {
        // Primitives don't have pool indices
        assert_eq!(InternedType::I32.pool_index(), None);
        assert_eq!(InternedType::BOOL.pool_index(), None);

        // Composite types have pool indices
        let composite = InternedType::from_pool_index(0);
        assert_eq!(composite.pool_index(), Some(0));
        assert!(!composite.is_primitive());

        let composite2 = InternedType::from_pool_index(42);
        assert_eq!(composite2.pool_index(), Some(42));
    }

    #[test]
    fn test_interned_type_equality() {
        assert_eq!(InternedType::I32, InternedType::I32);
        assert_ne!(InternedType::I32, InternedType::I64);
        assert_ne!(InternedType::I32, InternedType::from_pool_index(0));
    }

    #[test]
    fn test_interned_type_debug() {
        let i32_str = format!("{:?}", InternedType::I32);
        assert!(i32_str.contains("i32"));

        let composite_str = format!("{:?}", InternedType::from_pool_index(5));
        assert!(composite_str.contains("pool:5"));
    }

    // ========================================================================
    // TypeInternPool tests
    // ========================================================================

    #[test]
    fn test_pool_new() {
        let pool = TypeInternPool::new();
        assert!(pool.is_empty());
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn test_pool_register_struct() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let name = interner.get_or_intern("Point");

        let def = StructDef {
            name: "Point".to_string(),
            fields: vec![],
            is_copy: false,
            is_handle: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
        };

        let (struct_id, is_new) = pool.register_struct(name, def.clone());
        assert!(is_new);
        assert_eq!(struct_id.pool_index(), 0); // First entry in pool
        assert_eq!(pool.len(), 1);

        // Registering the same name returns the existing type
        let (struct_id2, is_new2) = pool.register_struct(name, def);
        assert!(!is_new2);
        assert_eq!(struct_id, struct_id2);
        assert_eq!(pool.len(), 1); // No new type added
    }

    #[test]
    fn test_pool_register_enum() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let name = interner.get_or_intern("Color");

        let def = EnumDef {
            name: "Color".to_string(),
            variants: vec!["Red".to_string(), "Green".to_string(), "Blue".to_string()],
        };

        let (enum_id, is_new) = pool.register_enum(name, def.clone());
        assert!(is_new);
        assert_eq!(enum_id.pool_index(), 0); // First entry in pool
        assert_eq!(pool.len(), 1);

        // Registering the same name returns the existing type
        let (enum_id2, is_new2) = pool.register_enum(name, def);
        assert!(!is_new2);
        assert_eq!(enum_id, enum_id2);
    }

    #[test]
    fn test_pool_intern_array() {
        let pool = TypeInternPool::new();

        // Intern [i32; 5]
        let arr1 = pool.intern_array(InternedType::I32, 5);
        assert!(!arr1.is_primitive());
        assert_eq!(pool.len(), 1);

        // Interning the same array returns the same type
        let arr2 = pool.intern_array(InternedType::I32, 5);
        assert_eq!(arr1, arr2);
        assert_eq!(pool.len(), 1);

        // Different length is a different type
        let arr3 = pool.intern_array(InternedType::I32, 10);
        assert_ne!(arr1, arr3);
        assert_eq!(pool.len(), 2);

        // Different element type is a different type
        let arr4 = pool.intern_array(InternedType::I64, 5);
        assert_ne!(arr1, arr4);
        assert_eq!(pool.len(), 3);
    }

    #[test]
    fn test_pool_get_struct_by_name() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let name = interner.get_or_intern("Point");

        assert!(pool.get_struct_by_name(name).is_none());

        let def = StructDef {
            name: "Point".to_string(),
            fields: vec![],
            is_copy: false,
            is_handle: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
        };

        let (struct_id, _) = pool.register_struct(name, def);
        // get_struct_by_name returns InternedType, convert StructId for comparison
        let expected = pool.struct_id_to_interned(struct_id);
        assert_eq!(pool.get_struct_by_name(name), Some(expected));
    }

    #[test]
    fn test_pool_get_enum_by_name() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let name = interner.get_or_intern("Status");

        assert!(pool.get_enum_by_name(name).is_none());

        let def = EnumDef {
            name: "Status".to_string(),
            variants: vec!["Active".to_string(), "Inactive".to_string()],
        };

        let (enum_id, _) = pool.register_enum(name, def);
        // get_enum_by_name returns InternedType, convert EnumId for comparison
        let expected = pool.enum_id_to_interned(enum_id);
        assert_eq!(pool.get_enum_by_name(name), Some(expected));
    }

    #[test]
    fn test_pool_get_array() {
        let pool = TypeInternPool::new();

        assert!(pool.get_array(InternedType::I32, 5).is_none());

        let arr = pool.intern_array(InternedType::I32, 5);
        assert_eq!(pool.get_array(InternedType::I32, 5), Some(arr));
        assert!(pool.get_array(InternedType::I32, 10).is_none());
    }

    #[test]
    fn test_pool_get_type_data() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        // Primitive types return None
        assert!(pool.get(InternedType::I32).is_none());

        // Register a struct
        let struct_name = interner.get_or_intern("Point");
        let struct_def = StructDef {
            name: "Point".to_string(),
            fields: vec![],
            is_copy: false,
            is_handle: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
        };
        let (struct_id, _) = pool.register_struct(struct_name, struct_def);
        let struct_ty = pool.struct_id_to_interned(struct_id);

        // Get struct data
        let data = pool.get(struct_ty).expect("should get struct data");
        assert!(matches!(data, TypeData::Struct(_)));

        // Intern an array
        let arr_ty = pool.intern_array(InternedType::I32, 10);
        let arr_data = pool.get(arr_ty).expect("should get array data");
        match arr_data {
            TypeData::Array { element, len } => {
                assert_eq!(element, InternedType::I32);
                assert_eq!(len, 10);
            }
            _ => panic!("expected array data"),
        }
    }

    #[test]
    fn test_pool_type_checks() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        let struct_name = interner.get_or_intern("Point");
        let struct_def = StructDef {
            name: "Point".to_string(),
            fields: vec![],
            is_copy: false,
            is_handle: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
        };
        let (struct_id, _) = pool.register_struct(struct_name, struct_def);
        let struct_ty = pool.struct_id_to_interned(struct_id);

        let enum_name = interner.get_or_intern("Color");
        let enum_def = EnumDef {
            name: "Color".to_string(),
            variants: vec!["Red".to_string()],
        };
        let (enum_id, _) = pool.register_enum(enum_name, enum_def);
        let enum_ty = pool.enum_id_to_interned(enum_id);

        let array_ty = pool.intern_array(InternedType::I32, 5);

        // Check is_struct
        assert!(pool.is_struct(struct_ty));
        assert!(!pool.is_struct(enum_ty));
        assert!(!pool.is_struct(array_ty));
        assert!(!pool.is_struct(InternedType::I32));

        // Check is_enum
        assert!(!pool.is_enum(struct_ty));
        assert!(pool.is_enum(enum_ty));
        assert!(!pool.is_enum(array_ty));
        assert!(!pool.is_enum(InternedType::I32));

        // Check is_array
        assert!(!pool.is_array(struct_ty));
        assert!(!pool.is_array(enum_ty));
        assert!(pool.is_array(array_ty));
        assert!(!pool.is_array(InternedType::I32));
    }

    #[test]
    fn test_pool_get_struct_def() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        let name = interner.get_or_intern("Point");
        let def = StructDef {
            name: "Point".to_string(),
            fields: vec![],
            is_copy: true,
            is_handle: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
        };
        let (struct_id, _) = pool.register_struct(name, def.clone());

        // Test the new Phase 3 struct_def() method that takes StructId directly
        let retrieved = pool.struct_def(struct_id);
        assert_eq!(retrieved.name, def.name);
        assert_eq!(retrieved.is_copy, def.is_copy);

        // Test the old get_struct_def() that takes InternedType
        let interned = pool.struct_id_to_interned(struct_id);
        let retrieved2 = pool
            .get_struct_def(interned)
            .expect("should get struct def");
        assert_eq!(retrieved2.name, def.name);

        // Non-struct returns None for get_struct_def
        let array_ty = pool.intern_array(InternedType::I32, 5);
        assert!(pool.get_struct_def(array_ty).is_none());
        assert!(pool.get_struct_def(InternedType::I32).is_none());
    }

    #[test]
    fn test_pool_get_enum_def() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        let name = interner.get_or_intern("Status");
        let def = EnumDef {
            name: "Status".to_string(),
            variants: vec!["A".to_string(), "B".to_string()],
        };
        let (enum_id, _) = pool.register_enum(name, def.clone());

        // Test the new Phase 3 enum_def() method that takes EnumId directly
        let retrieved = pool.enum_def(enum_id);
        assert_eq!(retrieved.name, def.name);
        assert_eq!(retrieved.variants.len(), 2);

        // Test the old get_enum_def() that takes InternedType
        let interned = pool.enum_id_to_interned(enum_id);
        let retrieved2 = pool.get_enum_def(interned).expect("should get enum def");
        assert_eq!(retrieved2.name, def.name);

        // Non-enum returns None for get_enum_def
        let array_ty = pool.intern_array(InternedType::I32, 5);
        assert!(pool.get_enum_def(array_ty).is_none());
        assert!(pool.get_enum_def(InternedType::I32).is_none());
    }

    #[test]
    fn test_pool_get_array_info() {
        let pool = TypeInternPool::new();

        let array_ty = pool.intern_array(InternedType::I64, 100);
        let (element, len) = pool
            .get_array_info(array_ty)
            .expect("should get array info");
        assert_eq!(element, InternedType::I64);
        assert_eq!(len, 100);

        // Non-array returns None
        let interner = ThreadedRodeo::default();
        let name = interner.get_or_intern("X");
        let def = StructDef {
            name: "X".to_string(),
            fields: vec![],
            is_copy: false,
            is_handle: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
        };
        let (struct_id, _) = pool.register_struct(name, def);
        let struct_ty = pool.struct_id_to_interned(struct_id);
        assert!(pool.get_array_info(struct_ty).is_none());
        assert!(pool.get_array_info(InternedType::I32).is_none());
    }

    #[test]
    fn test_pool_stats() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        let stats = pool.stats();
        assert_eq!(stats.struct_count, 0);
        assert_eq!(stats.enum_count, 0);
        assert_eq!(stats.array_count, 0);
        assert_eq!(stats.total, 0);

        // Add some types
        let s1 = interner.get_or_intern("S1");
        let s2 = interner.get_or_intern("S2");
        let e1 = interner.get_or_intern("E1");

        let def = StructDef {
            name: "S1".to_string(),
            fields: vec![],
            is_copy: false,
            is_handle: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
        };
        pool.register_struct(s1, def.clone());
        pool.register_struct(
            s2,
            StructDef {
                name: "S2".to_string(),
                ..def
            },
        );

        pool.register_enum(
            e1,
            EnumDef {
                name: "E1".to_string(),
                variants: vec![],
            },
        );

        pool.intern_array(InternedType::I32, 5);
        pool.intern_array(InternedType::I32, 10);
        pool.intern_array(InternedType::BOOL, 3);

        let stats = pool.stats();
        assert_eq!(stats.struct_count, 2);
        assert_eq!(stats.enum_count, 1);
        assert_eq!(stats.array_count, 3);
        assert_eq!(stats.total, 6);
    }

    #[test]
    fn test_pool_nested_arrays() {
        let pool = TypeInternPool::new();

        // Create [i32; 3]
        let inner = pool.intern_array(InternedType::I32, 3);

        // Create [[i32; 3]; 4]
        let outer = pool.intern_array(inner, 4);

        // Verify structure
        let (outer_elem, outer_len) = pool.get_array_info(outer).expect("outer array info");
        assert_eq!(outer_elem, inner);
        assert_eq!(outer_len, 4);

        let (inner_elem, inner_len) = pool.get_array_info(inner).expect("inner array info");
        assert_eq!(inner_elem, InternedType::I32);
        assert_eq!(inner_len, 3);
    }

    #[test]
    fn test_pool_type_to_interned() {
        let pool = TypeInternPool::new();

        // Primitive types convert correctly
        assert_eq!(pool.type_to_interned(Type::I8), Some(InternedType::I8));
        assert_eq!(pool.type_to_interned(Type::I16), Some(InternedType::I16));
        assert_eq!(pool.type_to_interned(Type::I32), Some(InternedType::I32));
        assert_eq!(pool.type_to_interned(Type::I64), Some(InternedType::I64));
        assert_eq!(pool.type_to_interned(Type::U8), Some(InternedType::U8));
        assert_eq!(pool.type_to_interned(Type::U16), Some(InternedType::U16));
        assert_eq!(pool.type_to_interned(Type::U32), Some(InternedType::U32));
        assert_eq!(pool.type_to_interned(Type::U64), Some(InternedType::U64));
        assert_eq!(pool.type_to_interned(Type::Bool), Some(InternedType::BOOL));
        assert_eq!(pool.type_to_interned(Type::Unit), Some(InternedType::UNIT));
        assert_eq!(
            pool.type_to_interned(Type::Never),
            Some(InternedType::NEVER)
        );
        assert_eq!(
            pool.type_to_interned(Type::Error),
            Some(InternedType::ERROR)
        );

        // Composite types return None (need name lookup)
        assert!(
            pool.type_to_interned(Type::Struct(crate::types::StructId(0)))
                .is_none()
        );
        assert!(
            pool.type_to_interned(Type::Enum(crate::types::EnumId(0)))
                .is_none()
        );
        assert!(
            pool.type_to_interned(Type::Array(crate::types::ArrayTypeId(0)))
                .is_none()
        );
    }

    #[test]
    fn test_pool_interned_to_type() {
        let pool = TypeInternPool::new();

        // Primitive types convert back correctly
        assert_eq!(pool.interned_to_type(InternedType::I8), Some(Type::I8));
        assert_eq!(pool.interned_to_type(InternedType::I16), Some(Type::I16));
        assert_eq!(pool.interned_to_type(InternedType::I32), Some(Type::I32));
        assert_eq!(pool.interned_to_type(InternedType::I64), Some(Type::I64));
        assert_eq!(pool.interned_to_type(InternedType::U8), Some(Type::U8));
        assert_eq!(pool.interned_to_type(InternedType::U16), Some(Type::U16));
        assert_eq!(pool.interned_to_type(InternedType::U32), Some(Type::U32));
        assert_eq!(pool.interned_to_type(InternedType::U64), Some(Type::U64));
        assert_eq!(pool.interned_to_type(InternedType::BOOL), Some(Type::Bool));
        assert_eq!(pool.interned_to_type(InternedType::UNIT), Some(Type::Unit));
        assert_eq!(
            pool.interned_to_type(InternedType::NEVER),
            Some(Type::Never)
        );
        assert_eq!(
            pool.interned_to_type(InternedType::ERROR),
            Some(Type::Error)
        );

        // Composite types return None
        assert!(
            pool.interned_to_type(InternedType::from_pool_index(0))
                .is_none()
        );
    }

    // ========================================================================
    // Thread safety tests
    // ========================================================================

    #[test]
    fn test_pool_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        let pool = Arc::new(TypeInternPool::new());
        let interner = Arc::new(ThreadedRodeo::default());

        // Pre-register names for thread safety
        let names: Vec<Spur> = (0..100)
            .map(|i| interner.get_or_intern(format!("Type{}", i)))
            .collect();

        let handles: Vec<_> = (0..10)
            .map(|thread_id| {
                let pool = Arc::clone(&pool);
                let names = names.clone();
                thread::spawn(move || {
                    // Each thread registers 10 types
                    for i in 0..10 {
                        let idx = thread_id * 10 + i;
                        let name = names[idx];
                        let def = StructDef {
                            name: format!("Type{}", idx),
                            fields: vec![],
                            is_copy: false,
                            is_handle: false,
                            is_linear: false,
                            destructor: None,
                            is_builtin: false,
                        };
                        pool.register_struct(name, def);
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("thread panicked");
        }

        // All 100 types should be registered
        assert_eq!(pool.len(), 100);

        // Each name should map to a valid type
        for name in &names {
            assert!(pool.get_struct_by_name(*name).is_some());
        }
    }

    #[test]
    fn test_pool_concurrent_array_interning() {
        use std::sync::Arc;
        use std::thread;

        let pool = Arc::new(TypeInternPool::new());

        // Multiple threads try to intern the same array type
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let pool = Arc::clone(&pool);
                thread::spawn(move || pool.intern_array(InternedType::I32, 42))
            })
            .collect();

        let results: Vec<_> = handles
            .into_iter()
            .map(|h| h.join().expect("thread panicked"))
            .collect();

        // All threads should get the same type
        let first = results[0];
        for result in &results {
            assert_eq!(*result, first);
        }

        // Only one array type should be in the pool
        assert_eq!(pool.stats().array_count, 1);
    }

    // Compile-time assertion that TypeInternPool is Send + Sync
    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn test_pool_is_send_sync() {
        assert_send_sync::<TypeInternPool>();
    }
}
