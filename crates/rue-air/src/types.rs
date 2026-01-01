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
    /// - Struct types (unless marked @copy, checked via StructDef.is_copy)
    /// - Array types (unless element type is Copy, checked via Sema.is_type_copy)
    ///
    /// Note: This method can't check struct's is_copy attribute or array element
    /// types since it doesn't have access to StructDefs or ArrayTypeDefs.
    /// Use Sema.is_type_copy() for full checking.
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
            // Struct types are move types by default (may be @copy, but need StructDef to check)
            Type::Struct(_) => false,
            // Arrays may be Copy if element type is Copy (need ArrayTypeDef to check)
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
        assert_eq!(Type::Bool.name(), "bool");
        assert_eq!(Type::Unit.name(), "()");
        assert_eq!(Type::Error.name(), "<error>");
        assert_eq!(Type::Never.name(), "!");
    }

    #[test]
    fn test_type_name_composite() {
        assert_eq!(Type::Struct(StructId(0)).name(), "<struct>");
        assert_eq!(Type::Enum(EnumId(0)).name(), "<enum>");
        assert_eq!(Type::Array(ArrayTypeId(0)).name(), "<array>");
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
        assert!(!Type::Bool.is_integer());
        assert!(!Type::Unit.is_integer());
        assert!(!Type::Struct(StructId(0)).is_integer());
        assert!(!Type::Enum(EnumId(0)).is_integer());
        assert!(!Type::Array(ArrayTypeId(0)).is_integer());
        assert!(!Type::Error.is_integer());
        assert!(!Type::Never.is_integer());
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
        assert!(!Type::Bool.is_signed());
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
        assert!(!Type::Bool.is_unsigned());
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
        assert!(!Type::Bool.is_64_bit());
    }

    // ========== Type::is_error() tests ==========

    #[test]
    fn test_is_error() {
        assert!(Type::Error.is_error());
        assert!(!Type::I32.is_error());
        assert!(!Type::Never.is_error());
    }

    // ========== Type::is_never() tests ==========

    #[test]
    fn test_is_never() {
        assert!(Type::Never.is_never());
        assert!(!Type::I32.is_never());
        assert!(!Type::Error.is_never());
    }

    // ========== Type::is_struct() and as_struct() tests ==========

    #[test]
    fn test_is_struct() {
        assert!(Type::Struct(StructId(0)).is_struct());
        assert!(Type::Struct(StructId(42)).is_struct());
        assert!(!Type::I32.is_struct());
        assert!(!Type::Enum(EnumId(0)).is_struct());
    }

    #[test]
    fn test_as_struct() {
        assert_eq!(Type::Struct(StructId(5)).as_struct(), Some(StructId(5)));
        assert_eq!(Type::I32.as_struct(), None);
        assert_eq!(Type::Enum(EnumId(0)).as_struct(), None);
    }

    // ========== Type::is_enum() and as_enum() tests ==========

    #[test]
    fn test_is_enum() {
        assert!(Type::Enum(EnumId(0)).is_enum());
        assert!(Type::Enum(EnumId(42)).is_enum());
        assert!(!Type::I32.is_enum());
        assert!(!Type::Struct(StructId(0)).is_enum());
    }

    #[test]
    fn test_as_enum() {
        assert_eq!(Type::Enum(EnumId(5)).as_enum(), Some(EnumId(5)));
        assert_eq!(Type::I32.as_enum(), None);
        assert_eq!(Type::Struct(StructId(0)).as_enum(), None);
    }

    // ========== Type::is_array() and as_array() tests ==========

    #[test]
    fn test_is_array() {
        assert!(Type::Array(ArrayTypeId(0)).is_array());
        assert!(Type::Array(ArrayTypeId(42)).is_array());
        assert!(!Type::I32.is_array());
        assert!(!Type::Struct(StructId(0)).is_array());
    }

    #[test]
    fn test_as_array() {
        assert_eq!(Type::Array(ArrayTypeId(5)).as_array(), Some(ArrayTypeId(5)));
        assert_eq!(Type::I32.as_array(), None);
        assert_eq!(Type::Struct(StructId(0)).as_array(), None);
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
        assert!(Type::Bool.is_copy());
        assert!(Type::Unit.is_copy());
    }

    #[test]
    fn test_is_copy_special() {
        // Enum types are Copy
        assert!(Type::Enum(EnumId(0)).is_copy());

        // Never and Error are Copy for convenience
        assert!(Type::Never.is_copy());
        assert!(Type::Error.is_copy());
    }

    #[test]
    fn test_is_copy_move_types() {
        // Struct and Array are move types (String is a builtin struct now)
        assert!(!Type::Struct(StructId(0)).is_copy());
        assert!(!Type::Array(ArrayTypeId(0)).is_copy());
    }

    // ========== Type::can_coerce_to() tests ==========

    #[test]
    fn test_can_coerce_to_same_type() {
        assert!(Type::I32.can_coerce_to(&Type::I32));
        assert!(Type::Bool.can_coerce_to(&Type::Bool));
        assert!(Type::Struct(StructId(0)).can_coerce_to(&Type::Struct(StructId(0))));
    }

    #[test]
    fn test_can_coerce_to_never_coerces_to_anything() {
        assert!(Type::Never.can_coerce_to(&Type::I32));
        assert!(Type::Never.can_coerce_to(&Type::Bool));
        assert!(Type::Never.can_coerce_to(&Type::Struct(StructId(0))));
    }

    #[test]
    fn test_can_coerce_to_error_coerces_to_anything() {
        assert!(Type::Error.can_coerce_to(&Type::I32));
        assert!(Type::Error.can_coerce_to(&Type::Bool));
        assert!(Type::Error.can_coerce_to(&Type::Struct(StructId(0))));
    }

    #[test]
    fn test_can_coerce_to_different_types_fail() {
        assert!(!Type::I32.can_coerce_to(&Type::Bool));
        assert!(!Type::Bool.can_coerce_to(&Type::I32));
        assert!(!Type::I32.can_coerce_to(&Type::I64));
        assert!(!Type::Struct(StructId(0)).can_coerce_to(&Type::I32));
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
        assert!(!Type::Bool.literal_fits(0));
        assert!(!Type::Struct(StructId(0)).literal_fits(0));
        assert!(!Type::Unit.literal_fits(0));
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
        assert!(!Type::Bool.negated_literal_fits(1));
        assert!(!Type::Struct(StructId(0)).negated_literal_fits(1));
    }

    // ========== Type Display tests ==========

    #[test]
    fn test_type_display() {
        assert_eq!(format!("{}", Type::I32), "i32");
        assert_eq!(format!("{}", Type::Bool), "bool");
        assert_eq!(format!("{}", Type::Never), "!");
    }

    // ========== Type Default tests ==========

    #[test]
    fn test_type_default() {
        assert_eq!(Type::default(), Type::Unit);
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
                    ty: Type::Bool,
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
        };
        assert_eq!(with_fields.field_count(), 3);
    }

    // ========== ArrayTypeDef tests ==========

    #[test]
    fn test_array_type_def_len() {
        let arr = ArrayTypeDef {
            element_type: Type::I32,
            length: 10,
        };
        assert_eq!(arr.len(), 10);
    }

    #[test]
    fn test_array_type_def_is_empty() {
        let empty = ArrayTypeDef {
            element_type: Type::I32,
            length: 0,
        };
        assert!(empty.is_empty());

        let non_empty = ArrayTypeDef {
            element_type: Type::I32,
            length: 1,
        };
        assert!(!non_empty.is_empty());
    }

    // ========== EnumDef tests ==========

    #[test]
    fn test_enum_def_variant_count() {
        let empty = EnumDef {
            name: "Empty".to_string(),
            variants: vec![],
        };
        assert_eq!(empty.variant_count(), 0);

        let color = EnumDef {
            name: "Color".to_string(),
            variants: vec!["Red".to_string(), "Green".to_string(), "Blue".to_string()],
        };
        assert_eq!(color.variant_count(), 3);
    }

    #[test]
    fn test_enum_def_find_variant() {
        let color = EnumDef {
            name: "Color".to_string(),
            variants: vec!["Red".to_string(), "Green".to_string(), "Blue".to_string()],
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
        };
        assert_eq!(empty.discriminant_type(), Type::Never);
    }

    #[test]
    fn test_enum_def_discriminant_type_small() {
        // 1-256 variants -> U8
        let small = EnumDef {
            name: "Small".to_string(),
            variants: vec!["A".to_string()],
        };
        assert_eq!(small.discriminant_type(), Type::U8);

        let max_u8 = EnumDef {
            name: "MaxU8".to_string(),
            variants: (0..256).map(|i| format!("V{}", i)).collect(),
        };
        assert_eq!(max_u8.discriminant_type(), Type::U8);
    }

    #[test]
    fn test_enum_def_discriminant_type_medium() {
        // 257-65536 variants -> U16
        let medium = EnumDef {
            name: "Medium".to_string(),
            variants: (0..257).map(|i| format!("V{}", i)).collect(),
        };
        assert_eq!(medium.discriminant_type(), Type::U16);
    }
}
