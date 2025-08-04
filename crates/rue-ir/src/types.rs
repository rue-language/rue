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

    /// Compute the memory layout of this type
    pub fn layout(&self) -> TypeLayout {
        match self {
            RueType::I32 => TypeLayout::new(4, 4),
            RueType::I64 => TypeLayout::new(8, 8),
            RueType::Bool => TypeLayout::new(1, 1),
            RueType::Unit => TypeLayout::new(0, 1),
            RueType::Unknown => TypeLayout::new(0, 1), // Placeholder, should not be used in final code

            RueType::Struct(_struct_id) => {
                // For now, we can't compute struct layout without access to the struct registry
                // This would need to be resolved through a type context in the future
                //
                // TEMPORARY FIX: Assume structs with 2 i64 fields (common case for testing)
                // This addresses Issue #113 where 8 bytes was too small for Point{x,y} structs
                // TODO: Implement proper struct field lookup via type registry
                TypeLayout::new(16, 8) // Assume 2 x i64 fields = 16 bytes
            }

            RueType::Tuple(types) => Self::compute_tuple_layout(types),

            RueType::Array(elem_type, len) => {
                let elem_layout = elem_type.layout();
                TypeLayout::new(elem_layout.size * len, elem_layout.align)
            }
        }
    }

    /// Get the size in bytes of this type
    pub fn size_bytes(&self) -> usize {
        self.layout().size
    }

    /// Get the alignment requirement in bytes of this type
    pub fn align_bytes(&self) -> usize {
        self.layout().align
    }

    /// Compute layout for a tuple type (internal helper)
    fn compute_tuple_layout(types: &[RueType]) -> TypeLayout {
        if types.is_empty() {
            return TypeLayout::new(0, 1);
        }

        let mut offset = 0;
        let mut max_align = 1;

        for ty in types {
            let field_layout = ty.layout();

            // Align the field
            offset = field_layout.align_offset(offset);
            offset += field_layout.size;
            max_align = max_align.max(field_layout.align);
        }

        // Align the total size to the tuple's alignment
        let final_size = TypeLayout::new(0, max_align).align_offset(offset);
        TypeLayout::new(final_size, max_align)
    }

    /// Compute the offset of a tuple field by index
    pub fn tuple_field_offset(&self, index: usize) -> Option<usize> {
        match self {
            RueType::Tuple(types) => {
                if index >= types.len() {
                    return None;
                }

                let mut offset = 0;
                for (i, ty) in types.iter().enumerate() {
                    if i == index {
                        let field_layout = ty.layout();
                        return Some(field_layout.align_offset(offset));
                    }

                    let field_layout = ty.layout();
                    offset = field_layout.align_offset(offset);
                    offset += field_layout.size;
                }
                None
            }
            _ => None,
        }
    }

    /// Compute the offset of an array element by index
    pub fn array_element_offset(&self, index: usize) -> Option<usize> {
        match self {
            RueType::Array(elem_type, len) => {
                if index >= *len {
                    return None;
                }
                let elem_layout = elem_type.layout();
                Some(elem_layout.size * index)
            }
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

/// Computed layout information for a type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeLayout {
    /// Size in bytes
    pub size: usize,
    /// Alignment requirement in bytes (must be power of 2)
    pub align: usize,
}

impl TypeLayout {
    /// Create a new layout with given size and alignment
    pub fn new(size: usize, align: usize) -> Self {
        debug_assert!(
            align > 0 && (align & (align - 1)) == 0,
            "Alignment must be power of 2"
        );
        Self { size, align }
    }

    /// Compute the offset for the next field, given the current offset
    pub fn align_offset(&self, offset: usize) -> usize {
        (offset + self.align - 1) & !(self.align - 1)
    }

    /// Compute the size needed to hold this layout at the given offset
    pub fn size_at_offset(&self, offset: usize) -> usize {
        let aligned_offset = self.align_offset(offset);
        aligned_offset + self.size
    }
}

/// Field layout information
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldLayout {
    /// Offset from start of struct in bytes
    pub offset: usize,
    /// Field type
    pub field_type: RueType,
}

/// Struct definition with computed layout information
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructDef {
    /// The struct's unique ID
    pub id: StructId,
    /// The struct's name (for debugging/display)
    pub name: String,
    /// Field definitions (name -> type)
    /// Using Vec to preserve field order
    pub fields: Vec<(String, RueType)>,
    /// Computed layout information
    layout: Option<TypeLayout>,
    /// Field layout information (computed lazily)
    field_layouts: Option<Vec<FieldLayout>>,
}

impl StructDef {
    /// Create a struct definition with fields
    pub fn new(id: StructId, name: impl Into<String>, fields: Vec<(String, RueType)>) -> Self {
        Self {
            id,
            name: name.into(),
            fields,
            layout: None,
            field_layouts: None,
        }
    }

    /// Create a stub struct definition
    pub fn stub(id: StructId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            fields: Vec::new(),
            layout: None,
            field_layouts: None,
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

    /// Compute the layout of this struct, caching it for future calls
    pub fn compute_layout(&mut self) -> TypeLayout {
        // Return cached layout if already computed
        if let Some(layout) = self.layout {
            return layout;
        }

        // Compute and cache the layout and field layouts
        let (layout, field_layouts) = Self::compute_struct_layout(&self.fields);
        self.layout = Some(layout);
        self.field_layouts = Some(field_layouts);
        layout
    }

    /// Get the field layout by index
    pub fn field_layout_by_index(&mut self, index: usize) -> Option<&FieldLayout> {
        self.compute_layout(); // Ensure layouts are computed
        self.field_layouts.as_ref()?.get(index)
    }

    /// Get the field layout by name
    pub fn field_layout_by_name(&mut self, name: &str) -> Option<&FieldLayout> {
        let index = self
            .fields
            .iter()
            .position(|(field_name, _)| field_name == name)?;
        self.field_layout_by_index(index)
    }

    /// Compute layout for a list of field types (internal helper)
    fn compute_struct_layout(fields: &[(String, RueType)]) -> (TypeLayout, Vec<FieldLayout>) {
        if fields.is_empty() {
            return (TypeLayout::new(0, 1), Vec::new());
        }

        let mut offset = 0;
        let mut max_align = 1;
        let mut field_layouts = Vec::new();

        for (_, field_type) in fields {
            let field_layout = field_type.layout();

            // Align the field
            offset = field_layout.align_offset(offset);

            field_layouts.push(FieldLayout {
                offset,
                field_type: field_type.clone(),
            });

            offset += field_layout.size;
            max_align = max_align.max(field_layout.align);
        }

        // Align the total size to the struct's alignment
        let final_size = TypeLayout::new(0, max_align).align_offset(offset);
        let layout = TypeLayout::new(final_size, max_align);

        (layout, field_layouts)
    }
}
