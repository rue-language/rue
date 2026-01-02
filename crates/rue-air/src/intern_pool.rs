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
//! in the `Type` index using reserved indices 0-15, requiring no pool lookup.
//!
//! # Thread Safety
//!
//! The pool uses `RwLock` for thread-safe access during parallel compilation:
//! - Read lock for lookups (common case)
//! - Write lock for insertions (rare, during declaration gathering)

use std::collections::HashMap;
use std::sync::RwLock;

use lasso::Spur;

// Import ID types from types.rs
use crate::types::{ArrayTypeId, EnumId, StructId};
// Import old Type with alias for conversion helpers during migration
use crate::types::Type as OldType;

/// Definition of a struct type.
///
/// This is the canonical definition - `StructField.ty` uses the new interned `Type`.
#[derive(Debug, Clone)]
pub struct StructDef {
    /// Struct name
    pub name: String,
    /// Fields in declaration order
    pub fields: Vec<StructField>,
    /// Whether this struct is marked with @copy (can be implicitly duplicated)
    pub is_copy: bool,
    /// Whether this struct is marked with @handle (can be explicitly duplicated via .handle())
    pub is_handle: bool,
    /// Whether this struct is a linear type (must be consumed, cannot be dropped)
    pub is_linear: bool,
    /// User-defined destructor function name, if any (e.g., "Data.__drop")
    pub destructor: Option<String>,
    /// Whether this is a built-in type (e.g., String) injected by the compiler.
    ///
    /// Built-in types behave like regular structs but have runtime implementations
    /// for their methods rather than generated code.
    pub is_builtin: bool,
}

/// A field in a struct definition.
#[derive(Debug, Clone)]
pub struct StructField {
    /// Field name
    pub name: String,
    /// Field type - uses the old Type enum for now since arrays haven't been
    /// migrated to the pool yet. Will be changed to new interned Type in a future phase.
    pub ty: OldType,
}

impl StructDef {
    /// Find a field by name and return its index and definition.
    pub fn find_field(&self, name: &str) -> Option<(usize, &StructField)> {
        self.fields.iter().enumerate().find(|(_, f)| f.name == name)
    }

    /// Get the number of fields in this struct.
    pub fn field_count(&self) -> usize {
        self.fields.len()
    }
}

/// Definition of an enum type.
#[derive(Debug, Clone)]
pub struct EnumDef {
    /// Enum name
    pub name: String,
    /// Variant names in declaration order
    pub variants: Vec<String>,
}

impl EnumDef {
    /// Get the number of variants in this enum.
    pub fn variant_count(&self) -> usize {
        self.variants.len()
    }

    /// Find a variant by name and return its index.
    pub fn find_variant(&self, name: &str) -> Option<usize> {
        self.variants.iter().position(|v| v == name)
    }

    /// Get the discriminant type for this enum.
    /// Returns the smallest unsigned integer type that can hold all variant indices.
    pub fn discriminant_type(&self) -> Type {
        let count = self.variants.len();
        if count == 0 {
            Type::NEVER // Zero-variant enum is uninhabited
        } else if count <= 256 {
            Type::U8
        } else if count <= 65536 {
            Type::U16
        } else if count <= 4_294_967_296 {
            Type::U32
        } else {
            Type::U64
        }
    }
}

/// A type in the Rue type system.
///
/// `Type` is a 32-bit index representing any type in the language.
/// Primitive types are encoded directly in reserved indices 0-15 (no pool lookup needed).
/// Composite types (struct, enum, array) use indices 16+ that reference the `TypeInternPool`.
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
///
/// # Type Equality
///
/// Type equality is O(1) - just comparing two u32 values. This is one of the key
/// benefits of the intern pool design.
///
/// # Usage
///
/// ```ignore
/// // Primitive types are constants
/// let int_type = Type::I32;
/// let bool_type = Type::BOOL;
///
/// // Composite types come from the pool
/// let array_type = pool.intern_array(Type::I32, 10);
/// let struct_type = pool.struct_id_to_type(struct_id);
///
/// // Type checks work without pool access for primitives
/// if ty == Type::I32 { ... }
/// if ty.is_integer() { ... }
///
/// // Composite type queries need the pool
/// if let Some(TypeData::Struct(data)) = pool.get(ty) { ... }
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Type(u32);

impl Type {
    // Reserved indices for primitives
    pub const I8: Type = Type(0);
    pub const I16: Type = Type(1);
    pub const I32: Type = Type(2);
    pub const I64: Type = Type(3);
    pub const U8: Type = Type(4);
    pub const U16: Type = Type(5);
    pub const U32: Type = Type(6);
    pub const U64: Type = Type(7);
    pub const BOOL: Type = Type(8);
    pub const UNIT: Type = Type(9);
    pub const NEVER: Type = Type(10);
    pub const ERROR: Type = Type(11);

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

    /// Create a Type from a raw index.
    ///
    /// # Safety
    ///
    /// The caller must ensure the index is valid (either a primitive index 0-15,
    /// or a composite index that exists in the pool).
    #[inline]
    pub fn from_raw(index: u32) -> Self {
        Type(index)
    }

    /// Create a Type for a composite type from its pool index.
    ///
    /// The pool index is offset by `PRIMITIVE_COUNT` to produce the final index.
    #[inline]
    pub(crate) fn from_pool_index(pool_index: u32) -> Self {
        Type(pool_index + Self::PRIMITIVE_COUNT)
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

    // ========================================================================
    // Type classification methods
    // ========================================================================
    // These methods provide efficient type queries without pool access for primitives.

    /// Check if this type is an integer type.
    #[inline]
    pub fn is_integer(self) -> bool {
        matches!(
            self.0,
            0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 // i8-i64, u8-u64
        )
    }

    /// Check if this is a signed integer type.
    #[inline]
    pub fn is_signed(self) -> bool {
        matches!(self.0, 0 | 1 | 2 | 3) // i8, i16, i32, i64
    }

    /// Check if this is an unsigned integer type.
    #[inline]
    pub fn is_unsigned(self) -> bool {
        matches!(self.0, 4 | 5 | 6 | 7) // u8, u16, u32, u64
    }

    /// Check if this is the boolean type.
    #[inline]
    pub fn is_bool(self) -> bool {
        self == Self::BOOL
    }

    /// Check if this is the unit type.
    #[inline]
    pub fn is_unit(self) -> bool {
        self == Self::UNIT
    }

    /// Check if this is the never type.
    #[inline]
    pub fn is_never(self) -> bool {
        self == Self::NEVER
    }

    /// Check if this is the error type.
    #[inline]
    pub fn is_error(self) -> bool {
        self == Self::ERROR
    }

    /// Check if this is a 64-bit type (uses 64-bit operations).
    #[inline]
    pub fn is_64_bit(self) -> bool {
        self == Self::I64 || self == Self::U64
    }

    /// Get a human-readable name for this type.
    /// Note: For composite types (struct, enum, array), this returns a placeholder.
    /// Use `TypeInternPool::type_name()` for full names including composite types.
    pub fn name(self) -> &'static str {
        match self.0 {
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
            12..=15 => "<reserved>",
            _ => "<composite>",
        }
    }

    /// Check if this type can coerce to the target type.
    ///
    /// Coercion rules:
    /// - Never can coerce to any type (it represents divergent control flow)
    /// - Error can coerce to any type (for error recovery during type checking)
    /// - Otherwise, types must be equal
    #[inline]
    pub fn can_coerce_to(self, target: Type) -> bool {
        self.is_never() || self.is_error() || self == target
    }

    /// Check if this is a Copy type based on the primitive type alone.
    ///
    /// Copy types are:
    /// - All integer types (i8-i64, u8-u64)
    /// - Boolean
    /// - Unit
    /// - Never type and Error type (for convenience in error recovery)
    ///
    /// Note: For composite types (struct, enum, array), this returns `false`
    /// even if the type might be @copy. Use `TypeInternPool::is_type_copy()`
    /// for full checking that includes composite types.
    #[inline]
    pub fn is_primitive_copy(self) -> bool {
        self.is_primitive() && !matches!(self.0, 12..=15) // All non-reserved primitives are Copy
    }

    /// Check if a u64 value fits within the range of this integer type.
    ///
    /// For signed types, only the positive range is checked (0 to max positive).
    /// Negation is handled separately to allow values like `-128` for i8.
    ///
    /// Returns `true` if the value fits, `false` otherwise.
    /// For non-integer types, returns `false`.
    #[must_use]
    pub fn literal_fits(self, value: u64) -> bool {
        match self.0 {
            0 => value <= i8::MAX as u64,  // i8
            1 => value <= i16::MAX as u64, // i16
            2 => value <= i32::MAX as u64, // i32
            3 => value <= i64::MAX as u64, // i64
            4 => value <= u8::MAX as u64,  // u8
            5 => value <= u16::MAX as u64, // u16
            6 => value <= u32::MAX as u64, // u32
            7 => true,                     // u64 - any value fits
            _ => false,
        }
    }

    /// Check if a u64 value can be negated to fit within the range of this signed integer type.
    ///
    /// This is used to allow literals like `2147483648` when negated to `-2147483648` (i32::MIN).
    /// Returns `true` if the negated value fits, `false` otherwise.
    #[must_use]
    pub fn negated_literal_fits(self, value: u64) -> bool {
        match self.0 {
            0 => value <= (i8::MIN as i64).unsigned_abs(),  // i8
            1 => value <= (i16::MIN as i64).unsigned_abs(), // i16
            2 => value <= (i32::MIN as i64).unsigned_abs(), // i32
            3 => value <= (i64::MIN).unsigned_abs(),        // i64
            _ => false,
        }
    }
}

impl std::fmt::Debug for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Type({})", self.name())
    }
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// Type data stored in the intern pool.
///
/// This is NOT Copy - it lives in the pool. You work with `Type` indices.
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
    Array { element: Type, len: u64 },
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
    /// All composite type data, indexed by (Type.0 - PRIMITIVE_COUNT).
    types: Vec<TypeData>,

    /// Structural type deduplication: (element, len) -> Type for arrays.
    array_map: HashMap<(Type, u64), Type>,

    /// Nominal type lookup: name -> Type for structs.
    struct_by_name: HashMap<Spur, Type>,

    /// Nominal type lookup: name -> Type for enums.
    enum_by_name: HashMap<Spur, Type>,
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
    /// Returns the `Type` for the struct and whether it was newly inserted.
    /// If a struct with this name already exists, returns the existing Type.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned.
    pub fn register_struct(&self, name: Spur, def: StructDef) -> (Type, bool) {
        // Fast path: check with read lock
        {
            let inner = self.inner.read().expect("TypeInternPool lock poisoned");
            if let Some(&existing) = inner.struct_by_name.get(&name) {
                return (existing, false);
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().expect("TypeInternPool lock poisoned");

        // Double-check after acquiring write lock
        if let Some(&existing) = inner.struct_by_name.get(&name) {
            return (existing, false);
        }

        // Create new struct type
        let pool_index = inner.types.len() as u32;
        let ty = Type::from_pool_index(pool_index);

        inner.types.push(TypeData::Struct(StructData { name, def }));
        inner.struct_by_name.insert(name, ty);

        (ty, true)
    }

    /// Register a new enum (nominal - no deduplication).
    ///
    /// Returns the `Type` for the enum and whether it was newly inserted.
    /// If an enum with this name already exists, returns the existing Type.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned.
    pub fn register_enum(&self, name: Spur, def: EnumDef) -> (Type, bool) {
        // Fast path: check with read lock
        {
            let inner = self.inner.read().expect("TypeInternPool lock poisoned");
            if let Some(&existing) = inner.enum_by_name.get(&name) {
                return (existing, false);
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().expect("TypeInternPool lock poisoned");

        // Double-check after acquiring write lock
        if let Some(&existing) = inner.enum_by_name.get(&name) {
            return (existing, false);
        }

        // Create new enum type
        let pool_index = inner.types.len() as u32;
        let ty = Type::from_pool_index(pool_index);

        inner.types.push(TypeData::Enum(EnumData { name, def }));
        inner.enum_by_name.insert(name, ty);

        (ty, true)
    }

    /// Intern an array type (structural - deduplicates).
    ///
    /// Returns the canonical `Type` for arrays with this element type and length.
    /// If an identical array type already exists, returns the existing type.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned.
    pub fn intern_array(&self, element: Type, len: u64) -> Type {
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
        let ty = Type::from_pool_index(pool_index);

        inner.types.push(TypeData::Array { element, len });
        inner.array_map.insert(key, ty);

        ty
    }

    /// Look up a struct by name.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned.
    pub fn get_struct_by_name(&self, name: Spur) -> Option<Type> {
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        inner.struct_by_name.get(&name).copied()
    }

    /// Look up an enum by name.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned.
    pub fn get_enum_by_name(&self, name: Spur) -> Option<Type> {
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        inner.enum_by_name.get(&name).copied()
    }

    /// Look up an array type by element and length.
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned.
    pub fn get_array(&self, element: Type, len: u64) -> Option<Type> {
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        inner.array_map.get(&(element, len)).copied()
    }

    /// Get type data for a composite type.
    ///
    /// Returns `None` for primitive types (use `Type::is_primitive()` first).
    ///
    /// # Panics
    ///
    /// Panics if the lock is poisoned or if the index is invalid.
    pub fn get(&self, ty: Type) -> Option<TypeData> {
        if ty.is_primitive() {
            return None;
        }

        let pool_index = ty.pool_index().expect("non-primitive must have pool index");
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        Some(inner.types[pool_index as usize].clone())
    }

    /// Check if this is a struct type.
    pub fn is_struct(&self, ty: Type) -> bool {
        if ty.is_primitive() {
            return false;
        }
        matches!(self.get(ty), Some(TypeData::Struct(_)))
    }

    /// Check if this is an enum type.
    pub fn is_enum(&self, ty: Type) -> bool {
        if ty.is_primitive() {
            return false;
        }
        matches!(self.get(ty), Some(TypeData::Enum(_)))
    }

    /// Check if this is an array type.
    pub fn is_array(&self, ty: Type) -> bool {
        if ty.is_primitive() {
            return false;
        }
        matches!(self.get(ty), Some(TypeData::Array { .. }))
    }

    /// Get the struct definition if this is a struct type.
    pub fn get_struct_def(&self, ty: Type) -> Option<StructDef> {
        match self.get(ty)? {
            TypeData::Struct(data) => Some(data.def),
            _ => None,
        }
    }

    /// Get the enum definition if this is an enum type.
    pub fn get_enum_def(&self, ty: Type) -> Option<EnumDef> {
        match self.get(ty)? {
            TypeData::Enum(data) => Some(data.def),
            _ => None,
        }
    }

    /// Get array info (element type, length) if this is an array type.
    pub fn get_array_info(&self, ty: Type) -> Option<(Type, u64)> {
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

    /// Convert a StructId to a Type.
    ///
    /// Since StructId now contains a pool index, we just add the primitive offset.
    #[inline]
    pub fn struct_id_to_type(&self, struct_id: StructId) -> Type {
        Type::from_pool_index(struct_id.0)
    }

    /// Convert an EnumId to a Type.
    ///
    /// Since EnumId now contains a pool index, we just add the primitive offset.
    #[inline]
    pub fn enum_id_to_type(&self, enum_id: EnumId) -> Type {
        Type::from_pool_index(enum_id.0)
    }

    /// Get all struct types registered in the pool.
    ///
    /// Returns a vector of all struct Types, useful for iterating over all
    /// structs (e.g., for drop glue synthesis).
    pub fn all_struct_types(&self) -> Vec<Type> {
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        inner.struct_by_name.values().copied().collect()
    }

    /// Get all enum types registered in the pool.
    ///
    /// Returns a vector of all enum Types, useful for iterating over all enums.
    pub fn all_enum_types(&self) -> Vec<Type> {
        let inner = self.inner.read().expect("TypeInternPool lock poisoned");
        inner.enum_by_name.values().copied().collect()
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
    // Conversion helpers for migration
    // ========================================================================

    /// Convert an old-style `OldType` enum to a `Type`.
    ///
    /// This is a temporary helper during migration from the old Type enum
    /// to the new interned Type. It converts primitive types directly.
    ///
    /// For composite types (struct/enum/array), the method looks up the
    /// type in the pool using the ID.
    pub fn from_old_type(&self, ty: OldType) -> Type {
        match ty {
            OldType::I8 => Type::I8,
            OldType::I16 => Type::I16,
            OldType::I32 => Type::I32,
            OldType::I64 => Type::I64,
            OldType::U8 => Type::U8,
            OldType::U16 => Type::U16,
            OldType::U32 => Type::U32,
            OldType::U64 => Type::U64,
            OldType::Bool => Type::BOOL,
            OldType::Unit => Type::UNIT,
            OldType::Never => Type::NEVER,
            OldType::Error => Type::ERROR,
            OldType::Struct(struct_id) => self.struct_id_to_type(struct_id),
            OldType::Enum(enum_id) => self.enum_id_to_type(enum_id),
            OldType::Array(array_id) => {
                // Array types need to be looked up - the ArrayTypeId was an index
                // into a separate array registry, but now we use the pool.
                // For now, we'll create from the pool index directly.
                Type::from_pool_index(array_id.0)
            }
        }
    }

    /// Convert a `Type` back to the old-style `OldType` enum.
    ///
    /// This is a temporary helper during migration.
    /// Returns the corresponding OldType variant.
    pub fn to_old_type(&self, ty: Type) -> OldType {
        if ty.is_primitive() {
            match ty.0 {
                0 => OldType::I8,
                1 => OldType::I16,
                2 => OldType::I32,
                3 => OldType::I64,
                4 => OldType::U8,
                5 => OldType::U16,
                6 => OldType::U32,
                7 => OldType::U64,
                8 => OldType::Bool,
                9 => OldType::Unit,
                10 => OldType::Never,
                11 => OldType::Error,
                _ => OldType::Error, // Reserved primitives
            }
        } else {
            // Look up in pool to determine what kind of composite type
            match self.get(ty) {
                Some(TypeData::Struct(_)) => {
                    let pool_index = ty.pool_index().unwrap();
                    OldType::Struct(StructId::from_pool_index(pool_index))
                }
                Some(TypeData::Enum(_)) => {
                    let pool_index = ty.pool_index().unwrap();
                    OldType::Enum(EnumId::from_pool_index(pool_index))
                }
                Some(TypeData::Array { .. }) => {
                    let pool_index = ty.pool_index().unwrap();
                    OldType::Array(ArrayTypeId(pool_index))
                }
                None => OldType::Error, // Invalid composite type
            }
        }
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
    // Type tests
    // ========================================================================

    #[test]
    fn test_type_primitives() {
        assert!(Type::I8.is_primitive());
        assert!(Type::I16.is_primitive());
        assert!(Type::I32.is_primitive());
        assert!(Type::I64.is_primitive());
        assert!(Type::U8.is_primitive());
        assert!(Type::U16.is_primitive());
        assert!(Type::U32.is_primitive());
        assert!(Type::U64.is_primitive());
        assert!(Type::BOOL.is_primitive());
        assert!(Type::UNIT.is_primitive());
        assert!(Type::NEVER.is_primitive());
        assert!(Type::ERROR.is_primitive());
    }

    #[test]
    fn test_type_indices() {
        assert_eq!(Type::I8.index(), 0);
        assert_eq!(Type::I16.index(), 1);
        assert_eq!(Type::I32.index(), 2);
        assert_eq!(Type::I64.index(), 3);
        assert_eq!(Type::U8.index(), 4);
        assert_eq!(Type::BOOL.index(), 8);
        assert_eq!(Type::UNIT.index(), 9);
    }

    #[test]
    fn test_type_pool_index() {
        // Primitives don't have pool indices
        assert_eq!(Type::I32.pool_index(), None);
        assert_eq!(Type::BOOL.pool_index(), None);

        // Composite types have pool indices
        let composite = Type::from_pool_index(0);
        assert_eq!(composite.pool_index(), Some(0));
        assert!(!composite.is_primitive());

        let composite2 = Type::from_pool_index(42);
        assert_eq!(composite2.pool_index(), Some(42));
    }

    #[test]
    fn test_type_equality() {
        assert_eq!(Type::I32, Type::I32);
        assert_ne!(Type::I32, Type::I64);
        assert_ne!(Type::I32, Type::from_pool_index(0));
    }

    #[test]
    fn test_type_debug() {
        let i32_str = format!("{:?}", Type::I32);
        assert!(i32_str.contains("i32"));

        let composite_str = format!("{:?}", Type::from_pool_index(5));
        assert!(composite_str.contains("composite"));
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

        let (struct_ty, is_new) = pool.register_struct(name, def.clone());
        assert!(is_new);
        assert_eq!(struct_ty.pool_index(), Some(0)); // First entry in pool
        assert_eq!(pool.len(), 1);

        // Registering the same name returns the existing type
        let (struct_ty2, is_new2) = pool.register_struct(name, def);
        assert!(!is_new2);
        assert_eq!(struct_ty, struct_ty2);
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

        let (enum_ty, is_new) = pool.register_enum(name, def.clone());
        assert!(is_new);
        assert_eq!(enum_ty.pool_index(), Some(0)); // First entry in pool
        assert_eq!(pool.len(), 1);

        // Registering the same name returns the existing type
        let (enum_ty2, is_new2) = pool.register_enum(name, def);
        assert!(!is_new2);
        assert_eq!(enum_ty, enum_ty2);
    }

    #[test]
    fn test_pool_intern_array() {
        let pool = TypeInternPool::new();

        // Intern [i32; 5]
        let arr1 = pool.intern_array(Type::I32, 5);
        assert!(!arr1.is_primitive());
        assert_eq!(pool.len(), 1);

        // Interning the same array returns the same type
        let arr2 = pool.intern_array(Type::I32, 5);
        assert_eq!(arr1, arr2);
        assert_eq!(pool.len(), 1);

        // Different length is a different type
        let arr3 = pool.intern_array(Type::I32, 10);
        assert_ne!(arr1, arr3);
        assert_eq!(pool.len(), 2);

        // Different element type is a different type
        let arr4 = pool.intern_array(Type::I64, 5);
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

        let (struct_ty, _) = pool.register_struct(name, def);
        // get_struct_by_name returns Type directly
        assert_eq!(pool.get_struct_by_name(name), Some(struct_ty));
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

        let (enum_ty, _) = pool.register_enum(name, def);
        // get_enum_by_name returns Type directly
        assert_eq!(pool.get_enum_by_name(name), Some(enum_ty));
    }

    #[test]
    fn test_pool_get_array() {
        let pool = TypeInternPool::new();

        assert!(pool.get_array(Type::I32, 5).is_none());

        let arr = pool.intern_array(Type::I32, 5);
        assert_eq!(pool.get_array(Type::I32, 5), Some(arr));
        assert!(pool.get_array(Type::I32, 10).is_none());
    }

    #[test]
    fn test_pool_get_type_data() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        // Primitive types return None
        assert!(pool.get(Type::I32).is_none());

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
        let (struct_ty, _) = pool.register_struct(struct_name, struct_def);

        // Get struct data
        let data = pool.get(struct_ty).expect("should get struct data");
        assert!(matches!(data, TypeData::Struct(_)));

        // Intern an array
        let arr_ty = pool.intern_array(Type::I32, 10);
        let arr_data = pool.get(arr_ty).expect("should get array data");
        match arr_data {
            TypeData::Array { element, len } => {
                assert_eq!(element, Type::I32);
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
        let (struct_ty, _) = pool.register_struct(struct_name, struct_def);

        let enum_name = interner.get_or_intern("Color");
        let enum_def = EnumDef {
            name: "Color".to_string(),
            variants: vec!["Red".to_string()],
        };
        let (enum_ty, _) = pool.register_enum(enum_name, enum_def);

        let array_ty = pool.intern_array(Type::I32, 5);

        // Check is_struct
        assert!(pool.is_struct(struct_ty));
        assert!(!pool.is_struct(enum_ty));
        assert!(!pool.is_struct(array_ty));
        assert!(!pool.is_struct(Type::I32));

        // Check is_enum
        assert!(!pool.is_enum(struct_ty));
        assert!(pool.is_enum(enum_ty));
        assert!(!pool.is_enum(array_ty));
        assert!(!pool.is_enum(Type::I32));

        // Check is_array
        assert!(!pool.is_array(struct_ty));
        assert!(!pool.is_array(enum_ty));
        assert!(pool.is_array(array_ty));
        assert!(!pool.is_array(Type::I32));
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
        let (struct_ty, _) = pool.register_struct(name, def.clone());

        // Test the struct_def() method that takes StructId
        let struct_id = StructId::from_pool_index(struct_ty.pool_index().unwrap());
        let retrieved = pool.struct_def(struct_id);
        assert_eq!(retrieved.name, def.name);
        assert_eq!(retrieved.is_copy, def.is_copy);

        // Test get_struct_def() that takes Type
        let retrieved2 = pool
            .get_struct_def(struct_ty)
            .expect("should get struct def");
        assert_eq!(retrieved2.name, def.name);

        // Non-struct returns None for get_struct_def
        let array_ty = pool.intern_array(Type::I32, 5);
        assert!(pool.get_struct_def(array_ty).is_none());
        assert!(pool.get_struct_def(Type::I32).is_none());
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
        let (enum_ty, _) = pool.register_enum(name, def.clone());

        // Test the enum_def() method that takes EnumId
        let enum_id = EnumId::from_pool_index(enum_ty.pool_index().unwrap());
        let retrieved = pool.enum_def(enum_id);
        assert_eq!(retrieved.name, def.name);
        assert_eq!(retrieved.variants.len(), 2);

        // Test get_enum_def() that takes Type
        let retrieved2 = pool.get_enum_def(enum_ty).expect("should get enum def");
        assert_eq!(retrieved2.name, def.name);

        // Non-enum returns None for get_enum_def
        let array_ty = pool.intern_array(Type::I32, 5);
        assert!(pool.get_enum_def(array_ty).is_none());
        assert!(pool.get_enum_def(Type::I32).is_none());
    }

    #[test]
    fn test_pool_get_array_info() {
        let pool = TypeInternPool::new();

        let array_ty = pool.intern_array(Type::I64, 100);
        let (element, len) = pool
            .get_array_info(array_ty)
            .expect("should get array info");
        assert_eq!(element, Type::I64);
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
        let (struct_ty, _) = pool.register_struct(name, def);
        assert!(pool.get_array_info(struct_ty).is_none());
        assert!(pool.get_array_info(Type::I32).is_none());
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

        pool.intern_array(Type::I32, 5);
        pool.intern_array(Type::I32, 10);
        pool.intern_array(Type::BOOL, 3);

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
        let inner = pool.intern_array(Type::I32, 3);

        // Create [[i32; 3]; 4]
        let outer = pool.intern_array(inner, 4);

        // Verify structure
        let (outer_elem, outer_len) = pool.get_array_info(outer).expect("outer array info");
        assert_eq!(outer_elem, inner);
        assert_eq!(outer_len, 4);

        let (inner_elem, inner_len) = pool.get_array_info(inner).expect("inner array info");
        assert_eq!(inner_elem, Type::I32);
        assert_eq!(inner_len, 3);
    }

    #[test]
    fn test_pool_from_old_type() {
        let pool = TypeInternPool::new();

        // Primitive types convert correctly
        assert_eq!(pool.from_old_type(OldType::I8), Type::I8);
        assert_eq!(pool.from_old_type(OldType::I16), Type::I16);
        assert_eq!(pool.from_old_type(OldType::I32), Type::I32);
        assert_eq!(pool.from_old_type(OldType::I64), Type::I64);
        assert_eq!(pool.from_old_type(OldType::U8), Type::U8);
        assert_eq!(pool.from_old_type(OldType::U16), Type::U16);
        assert_eq!(pool.from_old_type(OldType::U32), Type::U32);
        assert_eq!(pool.from_old_type(OldType::U64), Type::U64);
        assert_eq!(pool.from_old_type(OldType::Bool), Type::BOOL);
        assert_eq!(pool.from_old_type(OldType::Unit), Type::UNIT);
        assert_eq!(pool.from_old_type(OldType::Never), Type::NEVER);
        assert_eq!(pool.from_old_type(OldType::Error), Type::ERROR);
    }

    #[test]
    fn test_pool_to_old_type() {
        let pool = TypeInternPool::new();

        // Primitive types convert back correctly
        assert_eq!(pool.to_old_type(Type::I8), OldType::I8);
        assert_eq!(pool.to_old_type(Type::I16), OldType::I16);
        assert_eq!(pool.to_old_type(Type::I32), OldType::I32);
        assert_eq!(pool.to_old_type(Type::I64), OldType::I64);
        assert_eq!(pool.to_old_type(Type::U8), OldType::U8);
        assert_eq!(pool.to_old_type(Type::U16), OldType::U16);
        assert_eq!(pool.to_old_type(Type::U32), OldType::U32);
        assert_eq!(pool.to_old_type(Type::U64), OldType::U64);
        assert_eq!(pool.to_old_type(Type::BOOL), OldType::Bool);
        assert_eq!(pool.to_old_type(Type::UNIT), OldType::Unit);
        assert_eq!(pool.to_old_type(Type::NEVER), OldType::Never);
        assert_eq!(pool.to_old_type(Type::ERROR), OldType::Error);
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
                thread::spawn(move || pool.intern_array(Type::I32, 42))
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
