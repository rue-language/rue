//! Type system for Rue.
//!
//! Currently very minimal - just i32. Will be extended as the language grows.

/// A unique identifier for a struct definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StructId(pub u32);

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

impl Type {
    /// Get a human-readable name for this type.
    /// Note: For struct types, this returns a placeholder.
    /// Use `type_name_with_structs` for proper struct names.
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

    /// Check if this type can coerce to the target type.
    ///
    /// Coercion rules:
    /// - Never can coerce to any type (it represents divergent control flow)
    /// - Error can coerce to any type (for error recovery during type checking)
    /// - Otherwise, types must be equal
    pub fn can_coerce_to(&self, target: &Type) -> bool {
        self.is_never() || self.is_error() || self == target
    }
}

impl std::fmt::Display for Type {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}
