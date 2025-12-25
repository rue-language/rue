//! Type system for Rue.
//!
//! Currently very minimal - just i32. Will be extended as the language grows.

/// A unique identifier for a struct definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StructId(pub u32);

/// A unique identifier for an enum definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnumId(pub u32);

/// A unique identifier for an array type.
/// This is needed because Type is Copy, so we can't use Box<Type> for the element type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArrayTypeId(pub u32);

/// A type in the Rue type system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Type {
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
    #[default]
    Unit,
    /// User-defined struct type
    Struct(StructId),
    /// User-defined enum type
    Enum(EnumId),
    /// Fixed-size array type: [T; N]
    Array(ArrayTypeId),
    /// String type (fat pointer: ptr + len, 16 bytes)
    String,
    /// An error type (used during type checking to continue after errors)
    Error,
    /// The never type - represents computations that don't return (e.g., break, continue).
    /// Can coerce to any other type.
    Never,
}

/// Definition of a struct type.
#[derive(Debug, Clone)]
pub struct StructDef {
    /// Struct name
    pub name: String,
    /// Fields in declaration order
    pub fields: Vec<StructField>,
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

/// Definition of an array type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArrayTypeDef {
    /// Element type
    pub element_type: Type,
    /// Fixed array length
    pub length: u64,
}

impl ArrayTypeDef {
    /// Get the total number of elements in this array.
    pub fn len(&self) -> u64 {
        self.length
    }

    /// Check if this array has zero length.
    pub fn is_empty(&self) -> bool {
        self.length == 0
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
            Type::Never // Zero-variant enum is uninhabited
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

impl Type {
    /// Get a human-readable name for this type.
    /// Note: For struct and array types, this returns a placeholder.
    /// Use `type_name_with_structs` for proper struct/array names.
    pub fn name(&self) -> &'static str {
        match self {
            Type::I8 => "i8",
            Type::I16 => "i16",
            Type::I32 => "i32",
            Type::I64 => "i64",
            Type::U8 => "u8",
            Type::U16 => "u16",
            Type::U32 => "u32",
            Type::U64 => "u64",
            Type::Bool => "bool",
            Type::Unit => "()",
            Type::Struct(_) => "<struct>",
            Type::Enum(_) => "<enum>",
            Type::Array(_) => "<array>",
            Type::String => "String",
            Type::Error => "<error>",
            Type::Never => "!",
        }
    }

    /// Check if this type is an integer type.
    pub fn is_integer(&self) -> bool {
        matches!(
            self,
            Type::I8
                | Type::I16
                | Type::I32
                | Type::I64
                | Type::U8
                | Type::U16
                | Type::U32
                | Type::U64
        )
    }

    /// Check if this is an error type.
    pub fn is_error(&self) -> bool {
        matches!(self, Type::Error)
    }

    /// Check if this is the never type.
    pub fn is_never(&self) -> bool {
        matches!(self, Type::Never)
    }

    /// Check if this is a struct type.
    pub fn is_struct(&self) -> bool {
        matches!(self, Type::Struct(_))
    }

    /// Get the struct ID if this is a struct type.
    pub fn as_struct(&self) -> Option<StructId> {
        match self {
            Type::Struct(id) => Some(*id),
            _ => None,
        }
    }

    /// Check if this is an array type.
    pub fn is_array(&self) -> bool {
        matches!(self, Type::Array(_))
    }

    /// Get the array type ID if this is an array type.
    pub fn as_array(&self) -> Option<ArrayTypeId> {
        match self {
            Type::Array(id) => Some(*id),
            _ => None,
        }
    }

    /// Check if this is an enum type.
    pub fn is_enum(&self) -> bool {
        matches!(self, Type::Enum(_))
    }

    /// Get the enum ID if this is an enum type.
    pub fn as_enum(&self) -> Option<EnumId> {
        match self {
            Type::Enum(id) => Some(*id),
            _ => None,
        }
    }

    /// Check if this is a string type.
    pub fn is_string(&self) -> bool {
        matches!(self, Type::String)
    }

    /// Check if this is a signed integer type.
    pub fn is_signed(&self) -> bool {
        matches!(self, Type::I8 | Type::I16 | Type::I32 | Type::I64)
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
    /// - Struct types
    /// - String
    /// - Array types (until we implement Copy arrays with Copy elements)
    pub fn is_copy(&self) -> bool {
        match self {
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
            // Struct types are move types
            Type::Struct(_) => false,
            // String is a move type (owns heap data)
            Type::String => false,
            // Arrays are move types for now
            // TODO: Arrays of Copy types could be Copy
            Type::Array(_) => false,
        }
    }

    /// Check if this is a 64-bit type (uses 64-bit operations).
    pub fn is_64_bit(&self) -> bool {
        matches!(self, Type::I64 | Type::U64)
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
    #[must_use]
    pub fn is_unsigned(&self) -> bool {
        matches!(self, Type::U8 | Type::U16 | Type::U32 | Type::U64)
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
        match self {
            Type::I8 => value <= i8::MAX as u64,
            Type::I16 => value <= i16::MAX as u64,
            Type::I32 => value <= i32::MAX as u64,
            Type::I64 => value <= i64::MAX as u64,
            Type::U8 => value <= u8::MAX as u64,
            Type::U16 => value <= u16::MAX as u64,
            Type::U32 => value <= u32::MAX as u64,
            Type::U64 => true, // Any u64 value fits in u64
            _ => false,
        }
    }

    /// Check if a u64 value can be negated to fit within the range of this signed integer type.
    ///
    /// This is used to allow literals like `2147483648` when negated to `-2147483648` (i32::MIN).
    /// Returns `true` if the negated value fits, `false` otherwise.
    #[must_use]
    pub fn negated_literal_fits(&self, value: u64) -> bool {
        match self {
            Type::I8 => value <= (i8::MIN as i64).unsigned_abs(),
            Type::I16 => value <= (i16::MIN as i64).unsigned_abs(),
            Type::I32 => value <= (i32::MIN as i64).unsigned_abs(),
            Type::I64 => value <= (i64::MIN).unsigned_abs(),
            _ => false,
        }
    }
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}
