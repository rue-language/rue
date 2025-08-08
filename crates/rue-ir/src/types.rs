use std::collections::HashMap;
use std::fmt;

/// Unique identifier for a variable binding
///
/// This represents a unique binding site in the program, allowing us to distinguish
/// between different variables with the same name (e.g., due to shadowing).
/// Each `let` statement allocates a new BindingId, even if it shadows an existing variable.
///
/// # Design Notes
/// - Uses newtype pattern for type safety (can't mix with other u32s)
/// - Derives standard traits for use in collections
/// - Display implementation shows human-readable format for debugging
/// - Ord trait allows stable ordering based on allocation order
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BindingId(pub u32);

impl fmt::Display for BindingId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "binding#{}", self.0)
    }
}

impl BindingId {
    /// Create a new BindingId with the given numeric value
    pub fn new(id: u32) -> Self {
        Self(id)
    }

    /// Get the raw numeric ID
    pub fn as_u32(self) -> u32 {
        self.0
    }
}

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
    pub fn compute_layout(&mut self, type_ctx: &mut TypeContext) -> Result<TypeLayout, TypeError> {
        // Return cached layout if already computed
        if let Some(layout) = self.layout {
            return Ok(layout);
        }

        // Compute and cache the layout and field layouts
        let (layout, field_layouts) = self.compute_struct_layout_with_context(type_ctx)?;
        self.layout = Some(layout);
        self.field_layouts = Some(field_layouts);
        Ok(layout)
    }

    /// Compute struct layout using TypeContext
    fn compute_struct_layout_with_context(
        &self,
        type_ctx: &mut TypeContext,
    ) -> Result<(TypeLayout, Vec<FieldLayout>), TypeError> {
        if self.fields.is_empty() {
            return Ok((TypeLayout::new(0, 1), Vec::new()));
        }

        let mut offset = 0;
        let mut max_align = 1;
        let mut field_layouts = Vec::new();

        for (_, field_type) in &self.fields {
            let field_layout = type_ctx.compute_layout(field_type)?;

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

        Ok((layout, field_layouts))
    }

    /// Get the field layout by index (requires TypeContext)
    pub fn field_layout_by_index(
        &mut self,
        index: usize,
        type_ctx: &mut TypeContext,
    ) -> Result<Option<&FieldLayout>, TypeError> {
        self.compute_layout(type_ctx)?; // Ensure layouts are computed
        Ok(self
            .field_layouts
            .as_ref()
            .and_then(|layouts| layouts.get(index)))
    }

    /// Get the field layout by name (requires TypeContext)
    pub fn field_layout_by_name(
        &mut self,
        name: &str,
        type_ctx: &mut TypeContext,
    ) -> Result<Option<&FieldLayout>, TypeError> {
        let index = self
            .fields
            .iter()
            .position(|(field_name, _)| field_name == name);
        match index {
            Some(idx) => self.field_layout_by_index(idx, type_ctx),
            None => Ok(None),
        }
    }
}

/// Errors that can occur during type operations
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeError {
    /// Struct ID not found in registry
    UnknownStruct(StructId),
    /// Invalid field access for type
    InvalidFieldAccess { ty: RueType, field: FieldId },
    /// Type mismatch
    TypeMismatch { expected: RueType, actual: RueType },
    /// Array index out of bounds
    OutOfBoundsAccess { index: usize, len: usize },
    /// Field not found in struct
    FieldNotFound { struct_id: StructId, field: FieldId },
}

impl fmt::Display for TypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TypeError::UnknownStruct(id) => write!(f, "Unknown struct: {id}"),
            TypeError::InvalidFieldAccess { ty, field } => {
                write!(f, "Invalid field access: {field} on type {ty}")
            }
            TypeError::TypeMismatch { expected, actual } => {
                write!(f, "Type mismatch: expected {expected}, got {actual}")
            }
            TypeError::OutOfBoundsAccess { index, len } => {
                write!(f, "Index {index} out of bounds for length {len}")
            }
            TypeError::FieldNotFound { struct_id, field } => {
                write!(f, "Field {field} not found in struct {struct_id}")
            }
        }
    }
}

impl std::error::Error for TypeError {}

/// Type context that holds all type definitions
///
/// This is the central registry for all type information in the compiler.
/// It provides methods to define and look up struct types, compute layouts,
/// and resolve field information.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeContext {
    /// All struct definitions
    structs: HashMap<StructId, StructDef>,
    /// Next available struct ID
    next_struct_id: u32,
}

impl TypeContext {
    /// Create a new empty type context
    pub fn new() -> Self {
        Self {
            structs: HashMap::new(),
            next_struct_id: 0,
        }
    }

    /// Define a new struct type
    pub fn define_struct(
        &mut self,
        name: impl Into<String>,
        fields: Vec<(String, RueType)>,
    ) -> StructId {
        let id = StructId(self.next_struct_id);
        self.next_struct_id += 1;

        let struct_def = StructDef::new(id, name, fields);
        self.structs.insert(id, struct_def);
        id
    }

    /// Define a struct with an explicit ID (used when importing from semantic analysis)
    pub fn define_struct_with_id(
        &mut self,
        id: StructId,
        name: impl Into<String>,
        fields: Vec<(String, RueType)>,
    ) {
        let struct_def = StructDef::new(id, name, fields);
        self.structs.insert(id, struct_def);

        // Update the next_struct_id to ensure no conflicts
        if id.as_u32() >= self.next_struct_id {
            self.next_struct_id = id.as_u32() + 1;
        }
    }

    /// Look up a struct definition
    pub fn get_struct(&self, id: StructId) -> Option<&StructDef> {
        self.structs.get(&id)
    }

    /// Look up a struct definition, returning an error if not found
    pub fn lookup_struct(&self, id: StructId) -> Result<&StructDef, TypeError> {
        self.structs.get(&id).ok_or(TypeError::UnknownStruct(id))
    }

    /// Get the type of a field in a struct
    pub fn get_field_type(
        &self,
        struct_id: StructId,
        field: &FieldId,
    ) -> Result<&RueType, TypeError> {
        let struct_def = self.lookup_struct(struct_id)?;

        match field {
            FieldId::Named(name) => {
                struct_def
                    .field_type(name)
                    .ok_or_else(|| TypeError::FieldNotFound {
                        struct_id,
                        field: field.clone(),
                    })
            }
            FieldId::Index(idx) => {
                struct_def
                    .field_type_by_index(*idx)
                    .ok_or_else(|| TypeError::FieldNotFound {
                        struct_id,
                        field: field.clone(),
                    })
            }
        }
    }

    /// Compute the layout of a type
    pub fn compute_layout(&mut self, ty: &RueType) -> Result<TypeLayout, TypeError> {
        match ty {
            RueType::I32 => Ok(TypeLayout::new(4, 4)),
            RueType::I64 => Ok(TypeLayout::new(8, 8)),
            RueType::Bool => Ok(TypeLayout::new(1, 1)),
            RueType::Unit => Ok(TypeLayout::new(0, 1)),
            RueType::Unknown => Ok(TypeLayout::new(0, 1)),

            RueType::Struct(id) => {
                // We need to compute the struct layout, but we can't borrow self mutably twice
                // So we'll clone the struct def, compute its layout, then update the cache
                let mut struct_def = self
                    .structs
                    .get(id)
                    .ok_or(TypeError::UnknownStruct(*id))?
                    .clone();
                let layout = struct_def.compute_layout(self)?;
                // Update the cached struct with computed layout
                if let Some(cached_struct) = self.structs.get_mut(id) {
                    cached_struct.layout = struct_def.layout;
                    cached_struct.field_layouts = struct_def.field_layouts;
                }
                Ok(layout)
            }

            RueType::Tuple(types) => self.compute_tuple_layout(types),

            RueType::Array(elem_type, len) => {
                let elem_layout = self.compute_layout(elem_type)?;
                Ok(TypeLayout::new(elem_layout.size * len, elem_layout.align))
            }
        }
    }

    /// Compute the offset of a field within a struct
    pub fn compute_field_offset(
        &mut self,
        struct_id: StructId,
        field: &FieldId,
    ) -> Result<usize, TypeError> {
        // Clone the struct to avoid double borrow
        let mut struct_def = self
            .structs
            .get(&struct_id)
            .ok_or(TypeError::UnknownStruct(struct_id))?
            .clone();

        match field {
            FieldId::Named(name) => {
                let field_layout =
                    struct_def
                        .field_layout_by_name(name, self)?
                        .ok_or_else(|| TypeError::FieldNotFound {
                            struct_id,
                            field: field.clone(),
                        })?;
                let offset = field_layout.offset;

                // Update cache
                if let Some(cached_struct) = self.structs.get_mut(&struct_id) {
                    cached_struct.layout = struct_def.layout;
                    cached_struct.field_layouts = struct_def.field_layouts;
                }

                Ok(offset)
            }
            FieldId::Index(idx) => {
                let field_layout =
                    struct_def
                        .field_layout_by_index(*idx, self)?
                        .ok_or_else(|| TypeError::FieldNotFound {
                            struct_id,
                            field: field.clone(),
                        })?;
                let offset = field_layout.offset;

                // Update cache
                if let Some(cached_struct) = self.structs.get_mut(&struct_id) {
                    cached_struct.layout = struct_def.layout;
                    cached_struct.field_layouts = struct_def.field_layouts;
                }

                Ok(offset)
            }
        }
    }

    /// Get all struct definitions (for debugging/display)
    pub fn all_structs(&self) -> impl Iterator<Item = (&StructId, &StructDef)> {
        self.structs.iter()
    }

    /// Compute the offset of a tuple field by index
    pub fn compute_tuple_field_offset(
        &mut self,
        types: &[RueType],
        index: usize,
    ) -> Result<usize, TypeError> {
        if index >= types.len() {
            return Err(TypeError::OutOfBoundsAccess {
                index,
                len: types.len(),
            });
        }

        let mut offset = 0;
        for (i, ty) in types.iter().enumerate() {
            if i == index {
                let field_layout = self.compute_layout(ty)?;
                return Ok(field_layout.align_offset(offset));
            }

            let field_layout = self.compute_layout(ty)?;
            offset = field_layout.align_offset(offset);
            offset += field_layout.size;
        }
        unreachable!("Index was validated but not found")
    }

    /// Compute the offset of an array element by index
    pub fn compute_array_element_offset(
        &mut self,
        elem_type: &RueType,
        len: usize,
        index: usize,
    ) -> Result<usize, TypeError> {
        if index >= len {
            return Err(TypeError::OutOfBoundsAccess { index, len });
        }
        let elem_layout = self.compute_layout(elem_type)?;
        Ok(elem_layout.size * index)
    }

    /// Compute layout for a tuple type (helper)
    fn compute_tuple_layout(&mut self, types: &[RueType]) -> Result<TypeLayout, TypeError> {
        if types.is_empty() {
            return Ok(TypeLayout::new(0, 1));
        }

        let mut offset = 0;
        let mut max_align = 1;

        for ty in types {
            let field_layout = self.compute_layout(ty)?;

            // Align the field
            offset = field_layout.align_offset(offset);
            offset += field_layout.size;
            max_align = max_align.max(field_layout.align);
        }

        // Align the total size to the tuple's alignment
        let final_size = TypeLayout::new(0, max_align).align_offset(offset);
        Ok(TypeLayout::new(final_size, max_align))
    }
}

impl Default for TypeContext {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "types_test.rs"]
mod types_test;
