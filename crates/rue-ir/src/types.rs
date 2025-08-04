use std::fmt;

/// Unique identifier for a struct type definition
///
/// This is an opaque handle that references a struct definition in a type registry.
/// The internal u32 provides efficient copying and comparison.
///
/// # Design Notes
/// - Uses newtype pattern for type safety (can't mix with other u32s)
/// - Derives standard traits for use in collections
/// - Display implementation shows human-readable format for debugging
/// - Forward-compatible with future type registry implementation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StructId(pub u32);

impl fmt::Display for StructId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "struct#{}", self.0)
    }
}

impl StructId {
    /// Create a new StructId with the given numeric value
    ///
    /// This is a stub implementation for now. In the future, this will
    /// be replaced by proper registration in a type context/registry.
    pub fn new(id: u32) -> Self {
        Self(id)
    }

    /// Get the raw numeric ID
    pub fn as_u32(self) -> u32 {
        self.0
    }
}

/// Identifier for a field within a struct
///
/// Fields can be identified either by numeric index (for tuples/arrays)
/// or by name (for named structs). This enum captures both cases.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FieldId {
    /// Numeric field index (0-based)
    Index(usize),
    /// Named field (stored as string for now, could be interned later)
    Named(String),
}

impl fmt::Display for FieldId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FieldId::Index(idx) => write!(f, "{idx}"),
            FieldId::Named(name) => write!(f, "{name}"),
        }
    }
}

impl FieldId {
    /// Create a field ID from a numeric index
    pub fn from_index(idx: usize) -> Self {
        FieldId::Index(idx)
    }

    /// Create a field ID from a field name
    pub fn from_name(name: impl Into<String>) -> Self {
        FieldId::Named(name.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RueType {
    I32,
    I64,
    Bool,
    Unit,
    Unknown,

    // Aggregate types
    /// Struct type identified by its StructId
    Struct(StructId),
    /// Tuple type with ordered component types
    Tuple(Vec<RueType>),
    /// Array type with element type and fixed size
    Array(Box<RueType>, usize),
}

impl RueType {
    /// Check if this type is an aggregate (struct, tuple, or array)
    pub fn is_aggregate(&self) -> bool {
        matches!(
            self,
            RueType::Struct(_) | RueType::Tuple(_) | RueType::Array(_, _)
        )
    }

    /// Check if this type is a primitive scalar type
    pub fn is_scalar(&self) -> bool {
        matches!(self, RueType::I32 | RueType::I64 | RueType::Bool)
    }

    /// Get the size of an array type, if this is an array
    pub fn array_len(&self) -> Option<usize> {
        match self {
            RueType::Array(_, len) => Some(*len),
            _ => None,
        }
    }

    /// Get the element type of an array, if this is an array
    pub fn array_element_type(&self) -> Option<&RueType> {
        match self {
            RueType::Array(elem_ty, _) => Some(elem_ty),
            _ => None,
        }
    }

    /// Get the component types of a tuple, if this is a tuple
    pub fn tuple_types(&self) -> Option<&[RueType]> {
        match self {
            RueType::Tuple(types) => Some(types),
            _ => None,
        }
    }
}

impl fmt::Display for RueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RueType::I32 => write!(f, "i32"),
            RueType::I64 => write!(f, "i64"),
            RueType::Bool => write!(f, "bool"),
            RueType::Unit => write!(f, "()"),
            RueType::Unknown => write!(f, "unknown"),
            RueType::Struct(id) => write!(f, "{id}"),
            RueType::Tuple(types) => {
                write!(f, "(")?;
                for (i, ty) in types.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{ty}")?;
                }
                write!(f, ")")
            }
            RueType::Array(elem_ty, len) => write!(f, "[{elem_ty}; {len}]"),
        }
    }
}

/// Stub struct definition for future use
///
/// This will eventually hold the actual struct field definitions.
/// For now, it's a placeholder to demonstrate the API design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructDef {
    /// The struct's unique ID
    pub id: StructId,
    /// The struct's name (for debugging/display)
    pub name: String,
    /// Field definitions (name -> type)
    /// Using Vec to preserve field order
    pub fields: Vec<(String, RueType)>,
}

impl StructDef {
    /// Create a stub struct definition
    pub fn stub(id: StructId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            fields: Vec::new(),
        }
    }

    /// Get the type of a field by name
    pub fn field_type(&self, name: &str) -> Option<&RueType> {
        self.fields
            .iter()
            .find(|(field_name, _)| field_name == name)
            .map(|(_, ty)| ty)
    }

    /// Get the type of a field by index
    pub fn field_type_by_index(&self, index: usize) -> Option<&RueType> {
        self.fields.get(index).map(|(_, ty)| ty)
    }
}
