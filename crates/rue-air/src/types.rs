//! Type system for Rue.
//!
//! Currently very minimal - just i32. Will be extended as the language grows.

/// A unique identifier for a struct definition.
///
/// As of Phase 3 (ADR-0024), the inner value is a pool index into `TypeInternPool`,
/// not a vector index into a separate struct definitions array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StructId(pub u32);

impl StructId {
    /// Create a StructId from a pool index.
    ///
    /// This is the primary way to create StructIds during Phase 3+.
    /// The pool index is the raw index into `TypeInternPool.types`.
    #[inline]
    pub fn from_pool_index(pool_index: u32) -> Self {
        StructId(pool_index)
    }

    /// Get the pool index for this struct.
    ///
    /// This is the index into `TypeInternPool.types`.
    #[inline]
    pub fn pool_index(self) -> u32 {
        self.0
    }
}

/// A unique identifier for an enum definition.
///
/// As of Phase 3 (ADR-0024), the inner value is a pool index into `TypeInternPool`,
/// not a vector index into a separate enum definitions array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnumId(pub u32);

impl EnumId {
    /// Create an EnumId from a pool index.
    ///
    /// This is the primary way to create EnumIds during Phase 3+.
    /// The pool index is the raw index into `TypeInternPool.types`.
    #[inline]
    pub fn from_pool_index(pool_index: u32) -> Self {
        EnumId(pool_index)
    }

    /// Get the pool index for this enum.
    ///
    /// This is the index into `TypeInternPool.types`.
    #[inline]
    pub fn pool_index(self) -> u32 {
        self.0
    }
}

/// A unique identifier for an array type.
/// This is needed because Type is Copy, so we can't use Box<Type> for the element type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArrayTypeId(pub u32);

impl ArrayTypeId {
    /// Create an ArrayTypeId from a pool index.
    ///
    /// This is used during Phase 2B to create ArrayTypeIds from pool indices.
    /// The pool index is the raw index into `TypeInternPool.types`.
    #[inline]
    pub fn from_pool_index(pool_index: u32) -> Self {
        ArrayTypeId(pool_index)
    }

    /// Get the pool index for this array type.
    ///
    /// Returns the raw index into the TypeInternPool.
    #[inline]
    pub fn pool_index(self) -> u32 {
        self.0
    }
}

/// A unique identifier for a `ptr const T` type.
/// This is needed because Type is Copy, so we can't use Box<Type> for the pointee type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PtrConstTypeId(pub u32);

impl PtrConstTypeId {
    /// Create a PtrConstTypeId from a pool index.
    #[inline]
    pub fn from_pool_index(pool_index: u32) -> Self {
        PtrConstTypeId(pool_index)
    }

    /// Get the pool index for this pointer type.
    #[inline]
    pub fn pool_index(self) -> u32 {
        self.0
    }
}

/// A unique identifier for a `ptr mut T` type.
/// This is needed because Type is Copy, so we can't use Box<Type> for the pointee type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PtrMutTypeId(pub u32);

impl PtrMutTypeId {
    /// Create a PtrMutTypeId from a pool index.
    #[inline]
    pub fn from_pool_index(pool_index: u32) -> Self {
        PtrMutTypeId(pool_index)
    }

    /// Get the pool index for this pointer type.
    #[inline]
    pub fn pool_index(self) -> u32 {
        self.0
    }
}

/// A unique identifier for a module (imported file).
///
/// Modules are created by `@import("path.rue")` and represent the public
/// declarations of an imported file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModuleId(pub u32);

impl ModuleId {
    /// Create a ModuleId from an index.
    #[inline]
    pub fn new(index: u32) -> Self {
        ModuleId(index)
    }

    /// Get the index for this module.
    #[inline]
    pub fn index(self) -> u32 {
        self.0
    }
}

/// The kind of a type - used for pattern matching.
///
/// This enum mirrors the structure of the `Type` enum but is designed for
/// pattern matching. During the migration to `Type(InternedType)`, code that
/// pattern matches on types will use `ty.kind()` to get a `TypeKind`.
///
/// This separation allows incremental migration: all pattern matches can be
/// updated to use `.kind()` while `Type` is still an enum, then `Type` can be
/// replaced with `Type(InternedType)` without breaking existing code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeKind {
    /// 8-bit signed integer
    I8,
    /// 16-bit signed integer
    I16,
    /// 32-bit signed integer
    I32,
    /// 64-bit signed integer
    I64,
    /// 8-bit unsigned integer
    U8,
    /// 16-bit unsigned integer
    U16,
    /// 32-bit unsigned integer
    U32,
    /// 64-bit unsigned integer
    U64,
    /// Boolean
    Bool,
    /// The unit type (for functions that don't return a value)
    Unit,
    /// User-defined struct type
    Struct(StructId),
    /// User-defined enum type
    Enum(EnumId),
    /// Fixed-size array type: [T; N]
    Array(ArrayTypeId),
    /// Raw pointer to immutable data: ptr const T
    PtrConst(PtrConstTypeId),
    /// Raw pointer to mutable data: ptr mut T
    PtrMut(PtrMutTypeId),
    /// A module type (from @import)
    Module(ModuleId),
    /// An error type (used during type checking to continue after errors)
    Error,
    /// The never type - represents computations that don't return
    Never,
    /// The comptime type - the type of types themselves
    ComptimeType,
}

/// A type in the Rue type system.
///
/// After Phase 4.1 of ADR-0024, `Type` is a newtype wrapping a u32 index.
/// This enables O(1) type equality via u32 comparison.
///
/// # Encoding
///
/// The u32 value uses a tag-based encoding:
/// - Primitives (0-12): I8=0, I16=1, I32=2, I64=3, U8=4, U16=5, U32=6, U64=7,
///   Bool=8, Unit=9, Error=10, Never=11, ComptimeType=12
/// - Composites: low byte is tag (TAG_STRUCT, TAG_ENUM, TAG_ARRAY, TAG_MODULE),
///   high 24 bits are the ID
///
/// # Usage
///
/// Use the associated constants for primitive types:
/// ```ignore
/// let ty = Type::I32;
/// ```
///
/// Use constructor methods for composite types:
/// ```ignore
/// let ty = Type::new_struct(struct_id);
/// ```
///
/// Use `kind()` for pattern matching:
/// ```ignore
/// match ty.kind() {
///     TypeKind::I32 => { /* ... */ }
///     TypeKind::Struct(id) => { /* ... */ }
///     _ => { /* ... */ }
/// }
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Type(u32);

impl Default for Type {
    fn default() -> Self {
        Type::UNIT
    }
}

impl std::fmt::Debug for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Provide a readable debug format
        match self.kind() {
            TypeKind::I8 => write!(f, "Type::I8"),
            TypeKind::I16 => write!(f, "Type::I16"),
            TypeKind::I32 => write!(f, "Type::I32"),
            TypeKind::I64 => write!(f, "Type::I64"),
            TypeKind::U8 => write!(f, "Type::U8"),
            TypeKind::U16 => write!(f, "Type::U16"),
            TypeKind::U32 => write!(f, "Type::U32"),
            TypeKind::U64 => write!(f, "Type::U64"),
            TypeKind::Bool => write!(f, "Type::BOOL"),
            TypeKind::Unit => write!(f, "Type::UNIT"),
            TypeKind::Error => write!(f, "Type::ERROR"),
            TypeKind::Never => write!(f, "Type::NEVER"),
            TypeKind::ComptimeType => write!(f, "Type::COMPTIME_TYPE"),
            TypeKind::Struct(id) => write!(f, "Type::new_struct(StructId({}))", id.0),
            TypeKind::Enum(id) => write!(f, "Type::new_enum(EnumId({}))", id.0),
            TypeKind::Array(id) => write!(f, "Type::new_array(ArrayTypeId({}))", id.0),
            TypeKind::PtrConst(id) => write!(f, "Type::new_ptr_const(PtrConstTypeId({}))", id.0),
            TypeKind::PtrMut(id) => write!(f, "Type::new_ptr_mut(PtrMutTypeId({}))", id.0),
            TypeKind::Module(id) => write!(f, "Type::new_module(ModuleId({}))", id.0),
        }
    }
}

// Composite type tag constants
// These are used in the low byte of the u32 encoding to identify composite types.
// The high 24 bits contain the ID (StructId, EnumId, ArrayTypeId, ModuleId, or pointer type IDs).
const TAG_STRUCT: u32 = 100;
const TAG_ENUM: u32 = 101;
const TAG_ARRAY: u32 = 102;
const TAG_MODULE: u32 = 103;
const TAG_PTR_CONST: u32 = 104;
const TAG_PTR_MUT: u32 = 105;

// Primitive type constants
impl Type {
    /// 8-bit signed integer
    pub const I8: Type = Type(0);
    /// 16-bit signed integer
    pub const I16: Type = Type(1);
    /// 32-bit signed integer
    pub const I32: Type = Type(2);
    /// 64-bit signed integer
    pub const I64: Type = Type(3);
    /// 8-bit unsigned integer
    pub const U8: Type = Type(4);
    /// 16-bit unsigned integer
    pub const U16: Type = Type(5);
    /// 32-bit unsigned integer
    pub const U32: Type = Type(6);
    /// 64-bit unsigned integer
    pub const U64: Type = Type(7);
    /// Boolean
    pub const BOOL: Type = Type(8);
    /// The unit type (for functions that don't return a value)
    pub const UNIT: Type = Type(9);
    /// An error type (used during type checking to continue after errors)
    pub const ERROR: Type = Type(10);
    /// The never type - represents computations that don't return
    pub const NEVER: Type = Type(11);
    /// The comptime type - the type of types themselves
    pub const COMPTIME_TYPE: Type = Type(12);
}

// Composite type constructors
impl Type {
    /// Create a struct type from a StructId.
    #[inline]
    pub const fn new_struct(id: StructId) -> Type {
        Type(TAG_STRUCT | ((id.0 as u32) << 8))
    }

    /// Create an enum type from an EnumId.
    #[inline]
    pub const fn new_enum(id: EnumId) -> Type {
        Type(TAG_ENUM | ((id.0 as u32) << 8))
    }

    /// Create an array type from an ArrayTypeId.
    #[inline]
    pub const fn new_array(id: ArrayTypeId) -> Type {
        Type(TAG_ARRAY | ((id.0 as u32) << 8))
    }

    /// Create a raw const pointer type from a PtrConstTypeId.
    #[inline]
    pub const fn new_ptr_const(id: PtrConstTypeId) -> Type {
        Type(TAG_PTR_CONST | ((id.0 as u32) << 8))
    }

    /// Create a raw mut pointer type from a PtrMutTypeId.
    #[inline]
    pub const fn new_ptr_mut(id: PtrMutTypeId) -> Type {
        Type(TAG_PTR_MUT | ((id.0 as u32) << 8))
    }

    /// Create a module type from a ModuleId.
    #[inline]
    pub const fn new_module(id: ModuleId) -> Type {
        Type(TAG_MODULE | ((id.0 as u32) << 8))
    }
}

/// Definition of a struct type.
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
    /// Whether this struct is public (visible outside its directory)
    pub is_pub: bool,
    /// File ID this struct was declared in (for visibility checking)
    pub file_id: rue_span::FileId,
}

/// A field in a struct definition.
#[derive(Debug, Clone)]
pub struct StructField {
    /// Field name
    pub name: String,
    /// Field type
    pub ty: Type,
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
    /// Whether this enum is public (visible outside its directory)
    pub is_pub: bool,
    /// File ID this enum was declared in (for visibility checking)
    pub file_id: rue_span::FileId,
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

/// Definition of a module (imported file).
///
/// A module contains the public declarations from an imported file.
/// When code accesses `math.add()`, the module definition is consulted
/// to find the corresponding function.
#[derive(Debug, Clone)]
pub struct ModuleDef {
    /// The path used in @import (e.g., "math.rue")
    pub import_path: String,
    /// The resolved absolute file path
    pub file_path: String,
    /// Public functions in this module: name -> mangled name
    /// The mangled name includes the module path (e.g., "math::add")
    pub functions: std::collections::HashMap<String, String>,
    /// Public structs in this module
    pub structs: Vec<String>,
    /// Public enums in this module
    pub enums: Vec<String>,
}

impl ModuleDef {
    /// Create a new empty module definition.
    pub fn new(import_path: String, file_path: String) -> Self {
        Self {
            import_path,
            file_path,
            functions: std::collections::HashMap::new(),
            structs: Vec::new(),
            enums: Vec::new(),
        }
    }

    /// Find a function by name in this module.
    /// Returns the mangled function name if found.
    pub fn find_function(&self, name: &str) -> Option<&str> {
        self.functions.get(name).map(|s| s.as_str())
    }
}

impl Type {
    /// Get the kind of this type for pattern matching.
    ///
    /// This method decodes the u32 representation back to a `TypeKind` for pattern matching.
    /// Primitive types (0-12) decode directly; composite types decode the tag and ID.
    ///
    /// # Panics
    ///
    /// Panics if the Type has an invalid encoding. This should never happen with Types
    /// created through the normal API. If you're working with potentially corrupt data,
    /// use [`try_kind`](Self::try_kind) instead.
    ///
    /// # Example
    ///
    /// ```ignore
    /// match ty.kind() {
    ///     TypeKind::I32 | TypeKind::I64 => { /* handle integers */ }
    ///     TypeKind::Struct(id) => { /* handle struct */ }
    ///     _ => { /* other types */ }
    /// }
    /// ```
    #[inline]
    pub fn kind(&self) -> TypeKind {
        self.try_kind().unwrap_or_else(|| {
            panic!(
                "invalid Type encoding: raw value {:#010x} (tag={}, id={}). \
                 This indicates data corruption or a bug in Type construction. \
                 Valid tags are 0-12 (primitives) or 100-105 (composites).",
                self.0,
                self.0 & 0xFF,
                self.0 >> 8
            )
        })
    }

    /// Try to get the kind of this type, returning `None` if the encoding is invalid.
    ///
    /// This is the non-panicking version of [`kind`](Self::kind). Use this when working
    /// with potentially corrupt data or for defensive programming.
    ///
    /// # Example
    ///
    /// ```ignore
    /// if let Some(kind) = ty.try_kind() {
    ///     match kind {
    ///         TypeKind::I32 => { /* ... */ }
    ///         _ => { /* ... */ }
    ///     }
    /// } else {
    ///     eprintln!("corrupt type data");
    /// }
    /// ```
    #[inline]
    pub fn try_kind(&self) -> Option<TypeKind> {
        let tag = self.0 & 0xFF;
        match tag {
            0 => Some(TypeKind::I8),
            1 => Some(TypeKind::I16),
            2 => Some(TypeKind::I32),
            3 => Some(TypeKind::I64),
            4 => Some(TypeKind::U8),
            5 => Some(TypeKind::U16),
            6 => Some(TypeKind::U32),
            7 => Some(TypeKind::U64),
            8 => Some(TypeKind::Bool),
            9 => Some(TypeKind::Unit),
            10 => Some(TypeKind::Error),
            11 => Some(TypeKind::Never),
            12 => Some(TypeKind::ComptimeType),
            TAG_STRUCT => Some(TypeKind::Struct(StructId(self.0 >> 8))),
            TAG_ENUM => Some(TypeKind::Enum(EnumId(self.0 >> 8))),
            TAG_ARRAY => Some(TypeKind::Array(ArrayTypeId(self.0 >> 8))),
            TAG_PTR_CONST => Some(TypeKind::PtrConst(PtrConstTypeId(self.0 >> 8))),
            TAG_PTR_MUT => Some(TypeKind::PtrMut(PtrMutTypeId(self.0 >> 8))),
            TAG_MODULE => Some(TypeKind::Module(ModuleId(self.0 >> 8))),
            _ => None,
        }
    }

    /// Get a human-readable name for this type.
    /// Note: For struct and array types, this returns a placeholder.
    /// Use `type_name_with_structs` for proper struct/array names.
    pub fn name(&self) -> &'static str {
        match self.kind() {
            TypeKind::I8 => "i8",
            TypeKind::I16 => "i16",
            TypeKind::I32 => "i32",
            TypeKind::I64 => "i64",
            TypeKind::U8 => "u8",
            TypeKind::U16 => "u16",
            TypeKind::U32 => "u32",
            TypeKind::U64 => "u64",
            TypeKind::Bool => "bool",
            TypeKind::Unit => "()",
            TypeKind::Struct(_) => "<struct>",
            TypeKind::Enum(_) => "<enum>",
            TypeKind::Array(_) => "<array>",
            TypeKind::PtrConst(_) => "<ptr const>",
            TypeKind::PtrMut(_) => "<ptr mut>",
            TypeKind::Module(_) => "<module>",
            TypeKind::Error => "<error>",
            TypeKind::Never => "!",
            TypeKind::ComptimeType => "type",
        }
    }

    /// Check if this type is an integer type.
    /// Optimized: checks tag range directly (0-7 are integer types).
    #[inline]
    pub fn is_integer(&self) -> bool {
        self.0 <= 7
    }

    /// Check if this is an error type.
    #[inline]
    pub fn is_error(&self) -> bool {
        *self == Type::ERROR
    }

    /// Check if this is the never type.
    #[inline]
    pub fn is_never(&self) -> bool {
        *self == Type::NEVER
    }

    /// Check if this is the comptime type (the type of types).
    #[inline]
    pub fn is_comptime_type(&self) -> bool {
        *self == Type::COMPTIME_TYPE
    }

    /// Check if this is a struct type.
    #[inline]
    pub fn is_struct(&self) -> bool {
        (self.0 & 0xFF) == TAG_STRUCT
    }

    /// Get the struct ID if this is a struct type.
    #[inline]
    pub fn as_struct(&self) -> Option<StructId> {
        if self.is_struct() {
            Some(StructId(self.0 >> 8))
        } else {
            None
        }
    }

    /// Check if this is an array type.
    #[inline]
    pub fn is_array(&self) -> bool {
        (self.0 & 0xFF) == TAG_ARRAY
    }

    /// Get the array type ID if this is an array type.
    #[inline]
    pub fn as_array(&self) -> Option<ArrayTypeId> {
        if self.is_array() {
            Some(ArrayTypeId(self.0 >> 8))
        } else {
            None
        }
    }

    /// Check if this is an enum type.
    #[inline]
    pub fn is_enum(&self) -> bool {
        (self.0 & 0xFF) == TAG_ENUM
    }

    /// Get the enum ID if this is an enum type.
    #[inline]
    pub fn as_enum(&self) -> Option<EnumId> {
        if self.is_enum() {
            Some(EnumId(self.0 >> 8))
        } else {
            None
        }
    }

    /// Check if this is a module type.
    #[inline]
    pub fn is_module(&self) -> bool {
        (self.0 & 0xFF) == TAG_MODULE
    }

    /// Get the module ID if this is a module type.
    #[inline]
    pub fn as_module(&self) -> Option<ModuleId> {
        if self.is_module() {
            Some(ModuleId(self.0 >> 8))
        } else {
            None
        }
    }

    /// Check if this is a raw const pointer type.
    #[inline]
    pub fn is_ptr_const(&self) -> bool {
        (self.0 & 0xFF) == TAG_PTR_CONST
    }

    /// Get the pointer type ID if this is a ptr const type.
    #[inline]
    pub fn as_ptr_const(&self) -> Option<PtrConstTypeId> {
        if self.is_ptr_const() {
            Some(PtrConstTypeId(self.0 >> 8))
        } else {
            None
        }
    }

    /// Check if this is a raw mut pointer type.
    #[inline]
    pub fn is_ptr_mut(&self) -> bool {
        (self.0 & 0xFF) == TAG_PTR_MUT
    }

    /// Get the pointer type ID if this is a ptr mut type.
    #[inline]
    pub fn as_ptr_mut(&self) -> Option<PtrMutTypeId> {
        if self.is_ptr_mut() {
            Some(PtrMutTypeId(self.0 >> 8))
        } else {
            None
        }
    }

    /// Check if this is any raw pointer type (ptr const or ptr mut).
    #[inline]
    pub fn is_ptr(&self) -> bool {
        let tag = self.0 & 0xFF;
        tag == TAG_PTR_CONST || tag == TAG_PTR_MUT
    }

    /// Check if this is a signed integer type.
    /// Optimized: checks tag range directly (0-3 are signed integers).
    #[inline]
    pub fn is_signed(&self) -> bool {
        self.0 <= 3
    }

    /// Check if this is a Copy type (can be implicitly duplicated).
    ///
    /// Copy types are:
    /// - All integer types (i8-i64, u8-u64)
    /// - Boolean
    /// - Unit
    /// - Enum types
    /// - Never type and Error type (for convenience in error recovery)
    ///
    /// Non-Copy types (move types) are:
    /// - Struct types (unless marked @copy, checked via StructDef.is_copy)
    /// - Array types (unless element type is Copy, checked via Sema.is_type_copy)
    ///
    /// Note: This method can't check struct's is_copy attribute or array element
    /// types since it doesn't have access to StructDefs or array type information.
    /// Use Sema.is_type_copy() for full checking.
    pub fn is_copy(&self) -> bool {
        let tag = self.0 & 0xFF;
        match tag {
            // Primitive Copy types (I8..Unit = 0..9)
            0..=9 => true,
            // Error, Never, ComptimeType are Copy for convenience
            10..=12 => true,
            // Enum types are Copy (they're small discriminant values)
            TAG_ENUM => true,
            // Module types are Copy (they're just compile-time namespace references)
            TAG_MODULE => true,
            // Struct types are move types by default
            TAG_STRUCT => false,
            // Arrays may be Copy if element type is Copy (need TypeInternPool to check)
            TAG_ARRAY => false,
            _ => false,
        }
    }

    /// Check if this type is Copy, with access to TypeInternPool for struct checking.
    ///
    /// This is used during anonymous struct creation to determine if the new struct
    /// should be Copy based on its field types.
    pub fn is_copy_in_pool(&self, type_pool: &crate::intern_pool::TypeInternPool) -> bool {
        if let Some(struct_id) = self.as_struct() {
            type_pool.struct_def(struct_id).is_copy
        } else {
            self.is_copy()
        }
    }

    /// Check if this is a 64-bit type (uses 64-bit operations).
    /// Optimized: checks for I64 (3) or U64 (7).
    #[inline]
    pub fn is_64_bit(&self) -> bool {
        self.0 == 3 || self.0 == 7
    }

    /// Check if this type can coerce to the target type.
    ///
    /// Coercion rules:
    /// - Never can coerce to any type (it represents divergent control flow)
    /// - Error can coerce to any type (for error recovery during type checking)
    /// - Otherwise, types must be equal
    pub fn can_coerce_to(&self, target: &Type) -> bool {
        self.is_never() || self.is_error() || self == target
    }

    /// Check if this is an unsigned integer type.
    /// Optimized: checks tag range directly (4-7 are unsigned integers).
    #[inline]
    #[must_use]
    pub fn is_unsigned(&self) -> bool {
        self.0 >= 4 && self.0 <= 7
    }

    /// Check if a u64 value fits within the range of this integer type.
    ///
    /// For signed types, only the positive range is checked (0 to max positive).
    /// Negation is handled separately to allow values like `-128` for i8.
    ///
    /// Returns `true` if the value fits, `false` otherwise.
    /// For non-integer types, returns `false`.
    #[must_use]
    pub fn literal_fits(&self, value: u64) -> bool {
        match self.0 {
            0 => value <= i8::MAX as u64,  // I8
            1 => value <= i16::MAX as u64, // I16
            2 => value <= i32::MAX as u64, // I32
            3 => value <= i64::MAX as u64, // I64
            4 => value <= u8::MAX as u64,  // U8
            5 => value <= u16::MAX as u64, // U16
            6 => value <= u32::MAX as u64, // U32
            7 => true,                     // U64 - Any u64 value fits
            _ => false,
        }
    }

    /// Check if a u64 value can be negated to fit within the range of this signed integer type.
    ///
    /// This is used to allow literals like `2147483648` when negated to `-2147483648` (i32::MIN).
    /// Returns `true` if the negated value fits, `false` otherwise.
    #[must_use]
    pub fn negated_literal_fits(&self, value: u64) -> bool {
        match self.0 {
            0 => value <= (i8::MIN as i64).unsigned_abs(),  // I8
            1 => value <= (i16::MIN as i64).unsigned_abs(), // I16
            2 => value <= (i32::MIN as i64).unsigned_abs(), // I32
            3 => value <= (i64::MIN).unsigned_abs(),        // I64
            _ => false,
        }
    }

    /// Encode this type as a u32 for storage in extra arrays.
    ///
    /// Since Type is now a u32 newtype, this simply returns the inner value.
    #[inline]
    pub fn as_u32(&self) -> u32 {
        self.0
    }

    /// Decode a type from a u32 value.
    ///
    /// Since Type is now a u32 newtype, this simply wraps the value.
    /// Note: This does not validate the encoding - use with values from `as_u32()`.
    ///
    /// # Safety (not unsafe, but correctness)
    ///
    /// This method trusts that the input is a valid encoding. For untrusted data,
    /// use [`try_from_u32`](Self::try_from_u32) which validates the encoding.
    #[inline]
    pub fn from_u32(v: u32) -> Self {
        Type(v)
    }

    /// Try to decode a type from a u32 value, returning `None` if invalid.
    ///
    /// This validates that the encoding represents a valid type before returning.
    /// Use this when reading potentially corrupt data (e.g., deserialization,
    /// memory-mapped files, or debugging).
    ///
    /// # Example
    ///
    /// ```ignore
    /// if let Some(ty) = Type::try_from_u32(encoded) {
    ///     // Safe to use ty.kind()
    /// } else {
    ///     // Handle invalid encoding
    /// }
    /// ```
    #[inline]
    pub fn try_from_u32(v: u32) -> Option<Self> {
        if Self::is_valid_encoding(v) {
            Some(Type(v))
        } else {
            None
        }
    }

    /// Check if a u32 value is a valid Type encoding.
    ///
    /// Returns `true` if the value represents a valid primitive or composite type.
    #[inline]
    pub fn is_valid_encoding(v: u32) -> bool {
        let tag = v & 0xFF;
        match tag {
            // Primitive types: I8=0 through ComptimeType=12
            0..=12 => true,
            // Composite types with valid tags
            TAG_STRUCT | TAG_ENUM | TAG_ARRAY | TAG_PTR_CONST | TAG_PTR_MUT | TAG_MODULE => true,
            // Everything else is invalid
            _ => false,
        }
    }

    /// Check if this Type has a valid encoding.
    ///
    /// This is useful for debugging and assertions.
    #[inline]
    pub fn is_valid(&self) -> bool {
        Self::is_valid_encoding(self.0)
    }
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// Pointer mutability - whether the pointed-to data can be modified.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtrMutability {
    /// Immutable pointer (`ptr const T`)
    Const,
    /// Mutable pointer (`ptr mut T`)
    Mut,
}

/// Parse pointer type syntax "ptr const T" or "ptr mut T" and return (pointee_type_str, mutability).
///
/// Returns `None` if the string doesn't match pointer syntax.
pub fn parse_pointer_type_syntax(type_name: &str) -> Option<(String, PtrMutability)> {
    let type_name = type_name.trim();
    if let Some(rest) = type_name.strip_prefix("ptr const ") {
        Some((rest.trim().to_string(), PtrMutability::Const))
    } else if let Some(rest) = type_name.strip_prefix("ptr mut ") {
        Some((rest.trim().to_string(), PtrMutability::Mut))
    } else {
        None
    }
}

/// Parse array type syntax "[T; N]" and return (element_type_str, length).
///
/// This handles nested arrays correctly by tracking bracket depth.
/// For example, `[[i32; 3]; 4]` returns `("[i32; 3]", 4)`.
pub fn parse_array_type_syntax(type_name: &str) -> Option<(String, u64)> {
    let type_name = type_name.trim();
    if !type_name.starts_with('[') || !type_name.ends_with(']') {
        return None;
    }

    // Remove the outer brackets
    let inner = &type_name[1..type_name.len() - 1];

    // Find the semicolon separator - need to handle nested arrays
    // We look for the last `;` that's at nesting level 0
    let mut bracket_depth = 0;
    let mut semi_pos = None;
    for (i, ch) in inner.char_indices() {
        match ch {
            '[' => bracket_depth += 1,
            ']' => bracket_depth -= 1,
            ';' if bracket_depth == 0 => semi_pos = Some(i),
            _ => {}
        }
    }

    let semi_pos = semi_pos?;
    let element_type = inner[..semi_pos].trim().to_string();
    let length_str = inner[semi_pos + 1..].trim();
    let length: u64 = length_str.parse().ok()?;

    Some((element_type, length))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== Type ID tests ==========

    #[test]
    fn test_struct_id_equality() {
        let id1 = StructId(0);
        let id2 = StructId(0);
        let id3 = StructId(1);
        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_enum_id_equality() {
        let id1 = EnumId(0);
        let id2 = EnumId(0);
        let id3 = EnumId(1);
        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn test_array_type_id_equality() {
        let id1 = ArrayTypeId(0);
        let id2 = ArrayTypeId(0);
        let id3 = ArrayTypeId(1);
        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    // ========== Type::name() tests ==========

    #[test]
    fn test_type_name_integers() {
        assert_eq!(Type::I8.name(), "i8");
        assert_eq!(Type::I16.name(), "i16");
        assert_eq!(Type::I32.name(), "i32");
        assert_eq!(Type::I64.name(), "i64");
        assert_eq!(Type::U8.name(), "u8");
        assert_eq!(Type::U16.name(), "u16");
        assert_eq!(Type::U32.name(), "u32");
        assert_eq!(Type::U64.name(), "u64");
    }

    #[test]
    fn test_type_name_other() {
        assert_eq!(Type::BOOL.name(), "bool");
        assert_eq!(Type::UNIT.name(), "()");
        assert_eq!(Type::ERROR.name(), "<error>");
        assert_eq!(Type::NEVER.name(), "!");
    }

    #[test]
    fn test_type_name_composite() {
        assert_eq!(Type::new_struct(StructId(0)).name(), "<struct>");
        assert_eq!(Type::new_enum(EnumId(0)).name(), "<enum>");
        assert_eq!(Type::new_array(ArrayTypeId(0)).name(), "<array>");
    }

    // ========== Type::is_integer() tests ==========

    #[test]
    fn test_is_integer_signed() {
        assert!(Type::I8.is_integer());
        assert!(Type::I16.is_integer());
        assert!(Type::I32.is_integer());
        assert!(Type::I64.is_integer());
    }

    #[test]
    fn test_is_integer_unsigned() {
        assert!(Type::U8.is_integer());
        assert!(Type::U16.is_integer());
        assert!(Type::U32.is_integer());
        assert!(Type::U64.is_integer());
    }

    #[test]
    fn test_is_integer_non_integers() {
        assert!(!Type::BOOL.is_integer());
        assert!(!Type::UNIT.is_integer());
        assert!(!Type::new_struct(StructId(0)).is_integer());
        assert!(!Type::new_enum(EnumId(0)).is_integer());
        assert!(!Type::new_array(ArrayTypeId(0)).is_integer());
        assert!(!Type::ERROR.is_integer());
        assert!(!Type::NEVER.is_integer());
    }

    // ========== Type::is_signed() tests ==========

    #[test]
    fn test_is_signed() {
        assert!(Type::I8.is_signed());
        assert!(Type::I16.is_signed());
        assert!(Type::I32.is_signed());
        assert!(Type::I64.is_signed());

        assert!(!Type::U8.is_signed());
        assert!(!Type::U16.is_signed());
        assert!(!Type::U32.is_signed());
        assert!(!Type::U64.is_signed());
        assert!(!Type::BOOL.is_signed());
    }

    // ========== Type::is_unsigned() tests ==========

    #[test]
    fn test_is_unsigned() {
        assert!(Type::U8.is_unsigned());
        assert!(Type::U16.is_unsigned());
        assert!(Type::U32.is_unsigned());
        assert!(Type::U64.is_unsigned());

        assert!(!Type::I8.is_unsigned());
        assert!(!Type::I16.is_unsigned());
        assert!(!Type::I32.is_unsigned());
        assert!(!Type::I64.is_unsigned());
        assert!(!Type::BOOL.is_unsigned());
    }

    // ========== Type::is_64_bit() tests ==========

    #[test]
    fn test_is_64_bit() {
        assert!(Type::I64.is_64_bit());
        assert!(Type::U64.is_64_bit());

        assert!(!Type::I8.is_64_bit());
        assert!(!Type::I16.is_64_bit());
        assert!(!Type::I32.is_64_bit());
        assert!(!Type::U8.is_64_bit());
        assert!(!Type::U16.is_64_bit());
        assert!(!Type::U32.is_64_bit());
        assert!(!Type::BOOL.is_64_bit());
    }

    // ========== Type::is_error() tests ==========

    #[test]
    fn test_is_error() {
        assert!(Type::ERROR.is_error());
        assert!(!Type::I32.is_error());
        assert!(!Type::NEVER.is_error());
    }

    // ========== Type::is_never() tests ==========

    #[test]
    fn test_is_never() {
        assert!(Type::NEVER.is_never());
        assert!(!Type::I32.is_never());
        assert!(!Type::ERROR.is_never());
    }

    // ========== Type::is_struct() and as_struct() tests ==========

    #[test]
    fn test_is_struct() {
        assert!(Type::new_struct(StructId(0)).is_struct());
        assert!(Type::new_struct(StructId(42)).is_struct());
        assert!(!Type::I32.is_struct());
        assert!(!Type::new_enum(EnumId(0)).is_struct());
    }

    #[test]
    fn test_as_struct() {
        assert_eq!(Type::new_struct(StructId(5)).as_struct(), Some(StructId(5)));
        assert_eq!(Type::I32.as_struct(), None);
        assert_eq!(Type::new_enum(EnumId(0)).as_struct(), None);
    }

    // ========== Type::is_enum() and as_enum() tests ==========

    #[test]
    fn test_is_enum() {
        assert!(Type::new_enum(EnumId(0)).is_enum());
        assert!(Type::new_enum(EnumId(42)).is_enum());
        assert!(!Type::I32.is_enum());
        assert!(!Type::new_struct(StructId(0)).is_enum());
    }

    #[test]
    fn test_as_enum() {
        assert_eq!(Type::new_enum(EnumId(5)).as_enum(), Some(EnumId(5)));
        assert_eq!(Type::I32.as_enum(), None);
        assert_eq!(Type::new_struct(StructId(0)).as_enum(), None);
    }

    // ========== Type::is_array() and as_array() tests ==========

    #[test]
    fn test_is_array() {
        assert!(Type::new_array(ArrayTypeId(0)).is_array());
        assert!(Type::new_array(ArrayTypeId(42)).is_array());
        assert!(!Type::I32.is_array());
        assert!(!Type::new_struct(StructId(0)).is_array());
    }

    #[test]
    fn test_as_array() {
        assert_eq!(
            Type::new_array(ArrayTypeId(5)).as_array(),
            Some(ArrayTypeId(5))
        );
        assert_eq!(Type::I32.as_array(), None);
        assert_eq!(Type::new_struct(StructId(0)).as_array(), None);
    }

    // ========== Type::is_copy() tests ==========

    #[test]
    fn test_is_copy_primitives() {
        // All integer types are Copy
        assert!(Type::I8.is_copy());
        assert!(Type::I16.is_copy());
        assert!(Type::I32.is_copy());
        assert!(Type::I64.is_copy());
        assert!(Type::U8.is_copy());
        assert!(Type::U16.is_copy());
        assert!(Type::U32.is_copy());
        assert!(Type::U64.is_copy());

        // Bool and Unit are Copy
        assert!(Type::BOOL.is_copy());
        assert!(Type::UNIT.is_copy());
    }

    #[test]
    fn test_is_copy_special() {
        // Enum types are Copy
        assert!(Type::new_enum(EnumId(0)).is_copy());

        // Never and Error are Copy for convenience
        assert!(Type::NEVER.is_copy());
        assert!(Type::ERROR.is_copy());
    }

    #[test]
    fn test_is_copy_move_types() {
        // Struct and Array are move types (String is a builtin struct now)
        assert!(!Type::new_struct(StructId(0)).is_copy());
        assert!(!Type::new_array(ArrayTypeId(0)).is_copy());
    }

    // ========== Type::can_coerce_to() tests ==========

    #[test]
    fn test_can_coerce_to_same_type() {
        assert!(Type::I32.can_coerce_to(&Type::I32));
        assert!(Type::BOOL.can_coerce_to(&Type::BOOL));
        assert!(Type::new_struct(StructId(0)).can_coerce_to(&Type::new_struct(StructId(0))));
    }

    #[test]
    fn test_can_coerce_to_never_coerces_to_anything() {
        assert!(Type::NEVER.can_coerce_to(&Type::I32));
        assert!(Type::NEVER.can_coerce_to(&Type::BOOL));
        assert!(Type::NEVER.can_coerce_to(&Type::new_struct(StructId(0))));
    }

    #[test]
    fn test_can_coerce_to_error_coerces_to_anything() {
        assert!(Type::ERROR.can_coerce_to(&Type::I32));
        assert!(Type::ERROR.can_coerce_to(&Type::BOOL));
        assert!(Type::ERROR.can_coerce_to(&Type::new_struct(StructId(0))));
    }

    #[test]
    fn test_can_coerce_to_different_types_fail() {
        assert!(!Type::I32.can_coerce_to(&Type::BOOL));
        assert!(!Type::BOOL.can_coerce_to(&Type::I32));
        assert!(!Type::I32.can_coerce_to(&Type::I64));
        assert!(!Type::new_struct(StructId(0)).can_coerce_to(&Type::I32));
    }

    // ========== Type::literal_fits() tests ==========

    #[test]
    fn test_literal_fits_i8() {
        assert!(Type::I8.literal_fits(0));
        assert!(Type::I8.literal_fits(127)); // i8::MAX
        assert!(!Type::I8.literal_fits(128));
    }

    #[test]
    fn test_literal_fits_i16() {
        assert!(Type::I16.literal_fits(0));
        assert!(Type::I16.literal_fits(32767)); // i16::MAX
        assert!(!Type::I16.literal_fits(32768));
    }

    #[test]
    fn test_literal_fits_i32() {
        assert!(Type::I32.literal_fits(0));
        assert!(Type::I32.literal_fits(2147483647)); // i32::MAX
        assert!(!Type::I32.literal_fits(2147483648));
    }

    #[test]
    fn test_literal_fits_i64() {
        assert!(Type::I64.literal_fits(0));
        assert!(Type::I64.literal_fits(9223372036854775807)); // i64::MAX
        assert!(!Type::I64.literal_fits(9223372036854775808));
    }

    #[test]
    fn test_literal_fits_u8() {
        assert!(Type::U8.literal_fits(0));
        assert!(Type::U8.literal_fits(255)); // u8::MAX
        assert!(!Type::U8.literal_fits(256));
    }

    #[test]
    fn test_literal_fits_u16() {
        assert!(Type::U16.literal_fits(0));
        assert!(Type::U16.literal_fits(65535)); // u16::MAX
        assert!(!Type::U16.literal_fits(65536));
    }

    #[test]
    fn test_literal_fits_u32() {
        assert!(Type::U32.literal_fits(0));
        assert!(Type::U32.literal_fits(4294967295)); // u32::MAX
        assert!(!Type::U32.literal_fits(4294967296));
    }

    #[test]
    fn test_literal_fits_u64() {
        assert!(Type::U64.literal_fits(0));
        assert!(Type::U64.literal_fits(u64::MAX)); // Any u64 fits
    }

    #[test]
    fn test_literal_fits_non_integer() {
        assert!(!Type::BOOL.literal_fits(0));
        assert!(!Type::new_struct(StructId(0)).literal_fits(0));
        assert!(!Type::UNIT.literal_fits(0));
    }

    // ========== Type::negated_literal_fits() tests ==========

    #[test]
    fn test_negated_literal_fits_i8() {
        assert!(Type::I8.negated_literal_fits(128)); // -128 = i8::MIN
        assert!(!Type::I8.negated_literal_fits(129));
    }

    #[test]
    fn test_negated_literal_fits_i16() {
        assert!(Type::I16.negated_literal_fits(32768)); // -32768 = i16::MIN
        assert!(!Type::I16.negated_literal_fits(32769));
    }

    #[test]
    fn test_negated_literal_fits_i32() {
        assert!(Type::I32.negated_literal_fits(2147483648)); // -2147483648 = i32::MIN
        assert!(!Type::I32.negated_literal_fits(2147483649));
    }

    #[test]
    fn test_negated_literal_fits_i64() {
        assert!(Type::I64.negated_literal_fits(9223372036854775808)); // i64::MIN abs
        assert!(!Type::I64.negated_literal_fits(9223372036854775809));
    }

    #[test]
    fn test_negated_literal_fits_unsigned() {
        // Unsigned types don't support negated literals
        assert!(!Type::U8.negated_literal_fits(1));
        assert!(!Type::U16.negated_literal_fits(1));
        assert!(!Type::U32.negated_literal_fits(1));
        assert!(!Type::U64.negated_literal_fits(1));
    }

    #[test]
    fn test_negated_literal_fits_non_integer() {
        assert!(!Type::BOOL.negated_literal_fits(1));
        assert!(!Type::new_struct(StructId(0)).negated_literal_fits(1));
    }

    // ========== Type Display tests ==========

    #[test]
    fn test_type_display() {
        assert_eq!(format!("{}", Type::I32), "i32");
        assert_eq!(format!("{}", Type::BOOL), "bool");
        assert_eq!(format!("{}", Type::NEVER), "!");
    }

    // ========== Type Default tests ==========

    #[test]
    fn test_type_default() {
        assert_eq!(Type::default(), Type::UNIT);
    }

    // ========== StructDef tests ==========

    #[test]
    fn test_struct_def_find_field() {
        let def = StructDef {
            name: "Point".to_string(),
            fields: vec![
                StructField {
                    name: "x".to_string(),
                    ty: Type::I32,
                },
                StructField {
                    name: "y".to_string(),
                    ty: Type::I32,
                },
            ],
            is_copy: false,
            is_handle: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };

        let (idx, field) = def.find_field("x").unwrap();
        assert_eq!(idx, 0);
        assert_eq!(field.name, "x");
        assert_eq!(field.ty, Type::I32);

        let (idx, field) = def.find_field("y").unwrap();
        assert_eq!(idx, 1);
        assert_eq!(field.name, "y");

        assert!(def.find_field("z").is_none());
    }

    #[test]
    fn test_struct_def_field_count() {
        let empty = StructDef {
            name: "Empty".to_string(),
            fields: vec![],
            is_copy: false,
            is_handle: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        assert_eq!(empty.field_count(), 0);

        let with_fields = StructDef {
            name: "Data".to_string(),
            fields: vec![
                StructField {
                    name: "a".to_string(),
                    ty: Type::I32,
                },
                StructField {
                    name: "b".to_string(),
                    ty: Type::BOOL,
                },
                StructField {
                    name: "c".to_string(),
                    ty: Type::I64,
                },
            ],
            is_copy: false,
            is_handle: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        assert_eq!(with_fields.field_count(), 3);
    }

    // ========== EnumDef tests ==========

    #[test]
    fn test_enum_def_variant_count() {
        let empty = EnumDef {
            name: "Empty".to_string(),
            variants: vec![],
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        assert_eq!(empty.variant_count(), 0);

        let color = EnumDef {
            name: "Color".to_string(),
            variants: vec!["Red".to_string(), "Green".to_string(), "Blue".to_string()],
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        assert_eq!(color.variant_count(), 3);
    }

    #[test]
    fn test_enum_def_find_variant() {
        let color = EnumDef {
            name: "Color".to_string(),
            variants: vec!["Red".to_string(), "Green".to_string(), "Blue".to_string()],
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };

        assert_eq!(color.find_variant("Red"), Some(0));
        assert_eq!(color.find_variant("Green"), Some(1));
        assert_eq!(color.find_variant("Blue"), Some(2));
        assert_eq!(color.find_variant("Yellow"), None);
    }

    #[test]
    fn test_enum_def_discriminant_type_empty() {
        let empty = EnumDef {
            name: "Empty".to_string(),
            variants: vec![],
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        assert_eq!(empty.discriminant_type(), Type::NEVER);
    }

    #[test]
    fn test_enum_def_discriminant_type_small() {
        // 1-256 variants -> U8
        let small = EnumDef {
            name: "Small".to_string(),
            variants: vec!["A".to_string()],
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        assert_eq!(small.discriminant_type(), Type::U8);

        let max_u8 = EnumDef {
            name: "MaxU8".to_string(),
            variants: (0..256).map(|i| format!("V{}", i)).collect(),
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        assert_eq!(max_u8.discriminant_type(), Type::U8);
    }

    #[test]
    fn test_enum_def_discriminant_type_medium() {
        // 257-65536 variants -> U16
        let medium = EnumDef {
            name: "Medium".to_string(),
            variants: (0..257).map(|i| format!("V{}", i)).collect(),
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        assert_eq!(medium.discriminant_type(), Type::U16);
    }

    // ========== Type::COMPTIME_TYPE tests ==========

    #[test]
    fn test_comptime_type_name() {
        assert_eq!(Type::COMPTIME_TYPE.name(), "type");
    }

    #[test]
    fn test_comptime_type_is_copy() {
        assert!(Type::COMPTIME_TYPE.is_copy());
    }

    #[test]
    fn test_comptime_type_is_comptime_type() {
        assert!(Type::COMPTIME_TYPE.is_comptime_type());
        assert!(!Type::I32.is_comptime_type());
        assert!(!Type::BOOL.is_comptime_type());
    }

    #[test]
    fn test_comptime_type_not_integer() {
        assert!(!Type::COMPTIME_TYPE.is_integer());
    }

    #[test]
    fn test_comptime_type_not_signed() {
        assert!(!Type::COMPTIME_TYPE.is_signed());
    }

    #[test]
    fn test_comptime_type_not_64_bit() {
        assert!(!Type::COMPTIME_TYPE.is_64_bit());
    }

    #[test]
    fn test_comptime_type_can_coerce_to_itself() {
        assert!(Type::COMPTIME_TYPE.can_coerce_to(&Type::COMPTIME_TYPE));
    }

    #[test]
    fn test_comptime_type_cannot_coerce_to_runtime_types() {
        assert!(!Type::COMPTIME_TYPE.can_coerce_to(&Type::I32));
        assert!(!Type::COMPTIME_TYPE.can_coerce_to(&Type::BOOL));
    }

    // ========== Type encoding validation tests ==========

    #[test]
    fn test_is_valid_encoding_primitives() {
        // All primitive types (0-12) are valid
        for i in 0..=12u32 {
            assert!(
                Type::is_valid_encoding(i),
                "primitive tag {} should be valid",
                i
            );
        }
    }

    #[test]
    fn test_is_valid_encoding_composites() {
        // Composite types with valid tags
        assert!(Type::is_valid_encoding(100)); // TAG_STRUCT
        assert!(Type::is_valid_encoding(101)); // TAG_ENUM
        assert!(Type::is_valid_encoding(102)); // TAG_ARRAY
        assert!(Type::is_valid_encoding(103)); // TAG_MODULE
        assert!(Type::is_valid_encoding(104)); // TAG_PTR_CONST
        assert!(Type::is_valid_encoding(105)); // TAG_PTR_MUT

        // With IDs in the high bits
        assert!(Type::is_valid_encoding(100 | (42 << 8))); // Struct with ID 42
        assert!(Type::is_valid_encoding(101 | (100 << 8))); // Enum with ID 100
    }

    #[test]
    fn test_is_valid_encoding_invalid() {
        // Tags between primitives and composites are invalid (13-99)
        for tag in 13..100u32 {
            assert!(
                !Type::is_valid_encoding(tag),
                "tag {} should be invalid",
                tag
            );
        }

        // Tags above composites are invalid (106+)
        for tag in 106..=255u32 {
            assert!(
                !Type::is_valid_encoding(tag),
                "tag {} should be invalid",
                tag
            );
        }
    }

    #[test]
    fn test_try_from_u32_valid() {
        // Valid primitives
        assert!(Type::try_from_u32(0).is_some()); // I8
        assert!(Type::try_from_u32(2).is_some()); // I32
        assert!(Type::try_from_u32(12).is_some()); // ComptimeType

        // Valid composites
        assert!(Type::try_from_u32(100).is_some()); // Struct(0)
        assert!(Type::try_from_u32(100 | (42 << 8)).is_some()); // Struct(42)
    }

    #[test]
    fn test_try_from_u32_invalid() {
        // Invalid tags
        assert!(Type::try_from_u32(50).is_none());
        assert!(Type::try_from_u32(99).is_none());
        assert!(Type::try_from_u32(106).is_none());
        assert!(Type::try_from_u32(255).is_none());
    }

    #[test]
    fn test_try_kind_valid() {
        assert_eq!(Type::I32.try_kind(), Some(TypeKind::I32));
        assert_eq!(Type::BOOL.try_kind(), Some(TypeKind::Bool));
        assert_eq!(
            Type::new_struct(StructId(42)).try_kind(),
            Some(TypeKind::Struct(StructId(42)))
        );
    }

    #[test]
    fn test_try_kind_invalid() {
        // Create an invalid Type by directly constructing with invalid encoding
        let invalid = Type::from_u32(50); // Tag 50 is invalid
        assert!(invalid.try_kind().is_none());

        let invalid2 = Type::from_u32(200); // Tag 200 is invalid
        assert!(invalid2.try_kind().is_none());
    }

    #[test]
    fn test_is_valid_method() {
        assert!(Type::I32.is_valid());
        assert!(Type::new_struct(StructId(0)).is_valid());

        // Invalid types
        let invalid = Type::from_u32(50);
        assert!(!invalid.is_valid());
    }

    #[test]
    #[should_panic(expected = "invalid Type encoding")]
    fn test_kind_panics_on_invalid() {
        let invalid = Type::from_u32(50);
        let _ = invalid.kind(); // Should panic
    }

    #[test]
    fn test_roundtrip_encoding() {
        // Test that as_u32 and from_u32 are inverses for valid types
        let types = [
            Type::I8,
            Type::I16,
            Type::I32,
            Type::I64,
            Type::U8,
            Type::U16,
            Type::U32,
            Type::U64,
            Type::BOOL,
            Type::UNIT,
            Type::ERROR,
            Type::NEVER,
            Type::COMPTIME_TYPE,
            Type::new_struct(StructId(0)),
            Type::new_struct(StructId(1000)),
            Type::new_enum(EnumId(5)),
            Type::new_array(ArrayTypeId(10)),
            Type::new_ptr_const(PtrConstTypeId(20)),
            Type::new_ptr_mut(PtrMutTypeId(30)),
            Type::new_module(ModuleId(40)),
        ];

        for ty in types {
            let encoded = ty.as_u32();
            let decoded = Type::from_u32(encoded);
            assert_eq!(ty, decoded, "roundtrip failed for {:?}", ty);
            assert!(
                decoded.is_valid(),
                "{:?} should be valid after roundtrip",
                ty
            );
        }
    }
}
