//! Type intern pool for efficient type representation.
//!
//! This module implements a unified type interning system inspired by Zig's `InternPool`.
//! All types become 32-bit indices into a canonical pool, enabling:
//!
//! - O(1) type equality (u32 comparison)
//! - Efficient memory usage
//! - Clean parallel compilation (no per-function type merging)
//! - Canonical identities for generic instantiations
//!
//! # Architecture
//!
//! The `TypeInternPool` serves as a canonical repository for all composite types:
//! - **Structs and enums** are nominal types (same name = same type)
//! - **Arrays** are structural types (same element type + length = same type)
//!
//! The transitional `InternedType` wrapper consumes the same authoritative
//! primitive assignments as [`Type`]. Its compatibility-only pool-index
//! encoding is centralized beside the live encoding and is removed by RUE-838.
//!
//! [`Type`] is the compact compiler-facing handle. Composite `StructId`,
//! `EnumId`, `ArrayTypeId`, and pointer IDs are indices into this pool, while
//! `InternedType` provides the pool's primitive-or-composite encoding for
//! interning and structural lookup. Definitions and structural identities are
//! therefore resolved through one pool (ADR-0024).
//!
//! # Thread Safety
//!
//! The pool uses `RwLock` for thread-safe access during parallel compilation:
//! - Read lock for lookups (common case)
//! - Write lock for insertions (rare, during declaration gathering)

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use lasso::Spur;
use rue_span::FileId;

use crate::path_norm::{mangle_symbol_component, normalize_module_path};
use crate::type_encoding::{self, Decoded, Primitive};
use crate::types::{
    ArrayTypeId, EnumDef, EnumId, LangItem, PtrConstTypeId, PtrMutTypeId, StructDef, StructId,
    Type, TypeKind,
};

/// Interned type index - 32 bits, Copy, cheap comparison.
///
/// Primitive values are exactly the authoritative [`Type`] encodings.
/// Composite values use the centralized transitional pool-index encoding.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct InternedType(u32);

impl InternedType {
    pub const I8: InternedType = InternedType(Type::I8.raw_encoding());
    pub const I16: InternedType = InternedType(Type::I16.raw_encoding());
    pub const I32: InternedType = InternedType(Type::I32.raw_encoding());
    pub const I64: InternedType = InternedType(Type::I64.raw_encoding());
    pub const U8: InternedType = InternedType(Type::U8.raw_encoding());
    pub const U16: InternedType = InternedType(Type::U16.raw_encoding());
    pub const U32: InternedType = InternedType(Type::U32.raw_encoding());
    pub const U64: InternedType = InternedType(Type::U64.raw_encoding());
    pub const BOOL: InternedType = InternedType(Type::BOOL.raw_encoding());
    pub const UNIT: InternedType = InternedType(Type::UNIT.raw_encoding());
    pub const ERROR: InternedType = InternedType(Type::ERROR.raw_encoding());
    pub const NEVER: InternedType = InternedType(Type::NEVER.raw_encoding());

    /// Check if this is a primitive type (no pool lookup needed).
    #[inline]
    pub fn is_primitive(self) -> bool {
        type_encoding::compatibility::is_primitive(self.0)
    }

    /// Get the raw index value.
    #[inline]
    pub fn index(self) -> u32 {
        self.0
    }

    /// Create an `InternedType` from a raw compatibility encoding.
    ///
    /// Pool ownership and bounds are validated separately by pool APIs.
    #[inline]
    pub fn try_from_raw(index: u32) -> Option<Self> {
        if type_encoding::compatibility::is_primitive(index)
            || type_encoding::compatibility::decode_pool_index(index).is_some()
        {
            Some(InternedType(index))
        } else {
            None
        }
    }

    /// Create an InternedType for a composite type from its pool index.
    ///
    /// The pool index is mapped through the centralized compatibility encoding.
    #[inline]
    fn from_pool_index(pool_index: u32) -> Self {
        let encoded = type_encoding::compatibility::encode_pool_index(pool_index)
            .expect("intern pool index overflow");
        InternedType(encoded)
    }

    /// Get the pool index for a composite type.
    ///
    /// Returns `None` for primitive types.
    #[inline]
    pub fn pool_index(self) -> Option<u32> {
        type_encoding::compatibility::decode_pool_index(self.0)
    }
}

impl std::fmt::Debug for InternedType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(Decoded::Primitive(primitive)) = type_encoding::decode(self.0) {
            let name = match primitive {
                Primitive::I8 => "i8",
                Primitive::I16 => "i16",
                Primitive::I32 => "i32",
                Primitive::I64 => "i64",
                Primitive::U8 => "u8",
                Primitive::U16 => "u16",
                Primitive::U32 => "u32",
                Primitive::U64 => "u64",
                Primitive::Bool => "bool",
                Primitive::Unit => "()",
                Primitive::Error => "<error>",
                Primitive::Never => "!",
                Primitive::ComptimeType => "type",
            };
            write!(f, "InternedType({name})")
        } else if let Some(pool_index) = self.pool_index() {
            write!(f, "InternedType(pool:{pool_index})")
        } else {
            write!(f, "InternedType(invalid:{:#x})", self.0)
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
/// - **Array**, **PtrConst**, and **PtrMut** are structural types: identity comes from element/pointee type
#[derive(Debug, Clone)]
pub enum TypeData {
    /// Private anonymous-construction slot. No live [`Type`] is issued for it.
    ReservedStruct,

    /// Named struct identity whose definition has not completed yet.
    DeclaredStruct(StructData),

    /// Named enum identity whose definition has not completed yet.
    DeclaredEnum(EnumData),

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

    /// Raw const pointer (structural type).
    ///
    /// `ptr const T` - pointer to immutable data.
    PtrConst { pointee: InternedType },

    /// Raw mut pointer (structural type).
    ///
    /// `ptr mut T` - pointer to mutable data.
    PtrMut { pointee: InternedType },
}

impl TypeData {
    fn is_incomplete(&self) -> bool {
        matches!(
            self,
            Self::ReservedStruct | Self::DeclaredStruct(_) | Self::DeclaredEnum(_)
        )
    }
}

/// Data for a struct type in the intern pool.
///
/// The pool entry for a nominal struct and its definition.
#[derive(Debug, Clone)]
pub struct StructData {
    /// The name symbol (interned string).
    pub name: Spur,
    /// The canonical struct definition stored at this pool index.
    pub def: StructDef,
}

/// Data for an enum type in the intern pool.
///
/// The pool entry for a nominal enum and its definition.
#[derive(Debug, Clone)]
pub struct EnumData {
    /// The name symbol (interned string).
    pub name: Spur,
    /// The canonical enum definition stored at this pool index.
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

/// Immutable type metadata used after semantic analysis completes.
///
/// Semantic analysis is the only phase allowed to extend or update the type
/// universe. [`TypeInternPool::freeze`] consumes that mutable universe after
/// specialization and anonymous-type/destructor discovery have reached their
/// fixed point. CFG construction and code generation receive this type instead:
/// nominal reads borrow definitions directly, and iteration takes no lock and
/// allocates no temporary ID vector.
#[derive(Debug, Clone)]
pub struct FrozenTypeInternPool {
    inner: Arc<TypeInternPoolInner>,
}

#[derive(Debug)]
struct TypeInternPoolInner {
    /// All composite type data, indexed by the decoded compatibility pool index.
    types: Vec<TypeData>,

    /// Structural type deduplication: (element, len) -> InternedType for arrays.
    array_map: HashMap<(InternedType, u64), InternedType>,

    /// Structural type deduplication: pointee -> InternedType for ptr const.
    ptr_const_map: HashMap<InternedType, InternedType>,

    /// Structural type deduplication: pointee -> InternedType for ptr mut.
    ptr_mut_map: HashMap<InternedType, InternedType>,

    /// Nominal struct lookup: (defining file, source name) -> InternedType.
    struct_by_file_name: HashMap<(FileId, Spur), InternedType>,

    /// Nominal enum lookup: (defining file, source name) -> InternedType.
    enum_by_file_name: HashMap<(FileId, Spur), InternedType>,

    /// Relocation-stable logical identity for each defining source file.
    symbol_paths: HashMap<FileId, String>,

    /// Explicit language-item assignments issued by a trusted frontend or
    /// durable semantic import boundary.
    struct_lang_items: HashMap<StructId, LangItem>,

    /// Reverse index enforcing one canonical nominal for each language item.
    lang_item_structs: HashMap<LangItem, StructId>,
}

fn checked_pool_index(index: usize) -> Option<u32> {
    let index = u32::try_from(index).ok()?;
    (index <= type_encoding::MAX_PAYLOAD).then_some(index)
}

impl TypeInternPoolInner {
    fn next_pool_index(&self) -> u32 {
        checked_pool_index(self.types.len())
            .expect("type intern pool exceeds the 24-bit Type payload capacity")
    }

    #[inline]
    fn data(&self, index: u32) -> &TypeData {
        &self.types[index as usize]
    }

    fn try_struct_def(&self, id: StructId) -> Option<&StructDef> {
        match self.types.get(id.0 as usize)? {
            TypeData::DeclaredStruct(data) | TypeData::Struct(data) => Some(&data.def),
            _ => None,
        }
    }

    fn struct_def(&self, id: StructId) -> &StructDef {
        self.try_struct_def(id)
            .unwrap_or_else(|| panic!("Expected struct at pool index {}", id.0))
    }

    fn try_enum_def(&self, id: EnumId) -> Option<&EnumDef> {
        match self.types.get(id.0 as usize)? {
            TypeData::DeclaredEnum(data) | TypeData::Enum(data) => Some(&data.def),
            _ => None,
        }
    }

    fn enum_def(&self, id: EnumId) -> &EnumDef {
        self.try_enum_def(id)
            .unwrap_or_else(|| panic!("Expected enum at pool index {}", id.0))
    }

    fn interned_to_type(&self, ty: InternedType) -> Type {
        if ty.is_primitive() {
            return Type::try_from_u32(ty.0)
                .expect("InternedType primitive must use the canonical Type encoding");
        }

        let index = ty.pool_index().expect("non-primitive must have pool index");
        match self.data(index) {
            TypeData::DeclaredStruct(_) | TypeData::Struct(_) => {
                Type::new_struct(StructId::from_pool_index(index))
            }
            TypeData::DeclaredEnum(_) | TypeData::Enum(_) => {
                Type::new_enum(EnumId::from_pool_index(index))
            }
            TypeData::Array { .. } => Type::new_array(ArrayTypeId::from_pool_index(index)),
            TypeData::PtrConst { .. } => {
                Type::new_ptr_const(PtrConstTypeId::from_pool_index(index))
            }
            TypeData::PtrMut { .. } => Type::new_ptr_mut(PtrMutTypeId::from_pool_index(index)),
            TypeData::ReservedStruct => {
                panic!("reserved pool entry {index} cannot be issued as a Type")
            }
        }
    }

    fn array_def(&self, id: ArrayTypeId) -> (Type, u64) {
        match self.data(id.0) {
            TypeData::Array { element, len } => (self.interned_to_type(*element), *len),
            other => panic!("Expected array at pool index {}, got {:?}", id.0, other),
        }
    }

    fn try_array_def(&self, id: ArrayTypeId) -> Option<(Type, u64)> {
        match self.types.get(id.0 as usize)? {
            TypeData::Array { element, len } => Some((self.interned_to_type(*element), *len)),
            _ => None,
        }
    }

    fn ptr_const_def(&self, id: PtrConstTypeId) -> Type {
        match self.data(id.pool_index()) {
            TypeData::PtrConst { pointee } => self.interned_to_type(*pointee),
            other => panic!(
                "Expected ptr const at pool index {}, got {:?}",
                id.pool_index(),
                other
            ),
        }
    }

    fn ptr_mut_def(&self, id: PtrMutTypeId) -> Type {
        match self.data(id.pool_index()) {
            TypeData::PtrMut { pointee } => self.interned_to_type(*pointee),
            other => panic!(
                "Expected ptr mut at pool index {}, got {:?}",
                id.pool_index(),
                other
            ),
        }
    }

    fn abi_slot_count(&self, ty: Type) -> u32 {
        match ty.kind() {
            TypeKind::I8
            | TypeKind::I16
            | TypeKind::I32
            | TypeKind::I64
            | TypeKind::U8
            | TypeKind::U16
            | TypeKind::U32
            | TypeKind::U64
            | TypeKind::Bool
            | TypeKind::Error
            | TypeKind::PtrConst(_)
            | TypeKind::PtrMut(_) => 1,
            TypeKind::Unit | TypeKind::Never | TypeKind::ComptimeType | TypeKind::Module(_) => 0,
            TypeKind::Struct(id) => self.struct_def(id).fields.iter().fold(0, |total, field| {
                total.saturating_add(self.abi_slot_count(field.ty))
            }),
            TypeKind::Array(id) => {
                let (element, length) = self.array_def(id);
                let slots = u64::from(self.abi_slot_count(element));
                u32::try_from(slots.saturating_mul(length)).unwrap_or(u32::MAX)
            }
            TypeKind::Enum(id) => {
                let def = self.enum_def(id);
                let payload = (0..def.variant_count())
                    .map(|index| {
                        def.variant_payload(index).iter().fold(0u32, |total, &ty| {
                            total.saturating_add(self.abi_slot_count(ty))
                        })
                    })
                    .max()
                    .unwrap_or(0);
                1u32.saturating_add(payload)
            }
        }
    }

    fn file_symbol_component(&self, file_id: FileId) -> String {
        self.symbol_paths
            .get(&file_id)
            .map(|path| mangle_symbol_component(&normalize_module_path(path)))
            .unwrap_or_else(|| file_id.index().to_string())
    }

    fn nominal_name_collides(&self, name: Spur) -> bool {
        self.struct_by_file_name
            .keys()
            .chain(self.enum_by_file_name.keys())
            .filter(|(_, existing_name)| *existing_name == name)
            .take(2)
            .count()
            > 1
    }

    fn struct_symbol_name(&self, id: StructId) -> String {
        let data = match self.data(id.0) {
            TypeData::DeclaredStruct(data) | TypeData::Struct(data) => data,
            other => panic!("Expected struct at pool index {}, got {:?}", id.0, other),
        };
        if !data.def.is_builtin && self.nominal_name_collides(data.name) {
            return format!(
                "{}${}",
                data.def.name,
                self.file_symbol_component(data.def.file_id)
            );
        }
        data.def.name.clone()
    }

    fn enum_symbol_name(&self, id: EnumId) -> String {
        let data = match self.data(id.0) {
            TypeData::DeclaredEnum(data) | TypeData::Enum(data) => data,
            other => panic!("Expected enum at pool index {}, got {:?}", id.0, other),
        };
        if self.nominal_name_collides(data.name) {
            return format!(
                "{}${}",
                data.def.name,
                self.file_symbol_component(data.def.file_id)
            );
        }
        data.def.name.clone()
    }

    fn safe_type_name(&self, ty: Type) -> String {
        match ty.try_kind() {
            Some(TypeKind::Struct(id)) => self
                .try_struct_def(id)
                .map(|def| def.name.clone())
                .unwrap_or_else(|| format!("<struct#{}>", id.0)),
            Some(TypeKind::Enum(id)) => self
                .try_enum_def(id)
                .map(|def| def.name.clone())
                .unwrap_or_else(|| format!("<enum#{}>", id.0)),
            Some(TypeKind::Array(id)) => self
                .try_array_def(id)
                .map(|(element, len)| format!("[{}; {}]", self.safe_type_name(element), len))
                .unwrap_or_else(|| format!("<array#{}>", id.0)),
            Some(TypeKind::PtrConst(id)) => match self.types.get(id.pool_index() as usize) {
                Some(TypeData::PtrConst { pointee }) => {
                    format!(
                        "ptr const {}",
                        self.safe_type_name(self.interned_to_type(*pointee))
                    )
                }
                _ => format!("<ptr const#{}>", id.0),
            },
            Some(TypeKind::PtrMut(id)) => match self.types.get(id.pool_index() as usize) {
                Some(TypeData::PtrMut { pointee }) => {
                    format!(
                        "ptr mut {}",
                        self.safe_type_name(self.interned_to_type(*pointee))
                    )
                }
                _ => format!("<ptr mut#{}>", id.0),
            },
            Some(_) => ty.name().to_string(),
            None => format!("<invalid type encoding: {:#x}>", ty.raw_encoding()),
        }
    }

    fn is_copy_type(&self, ty: Type) -> bool {
        ty.as_struct()
            .map(|id| self.struct_def(id).is_copy)
            .unwrap_or_else(|| ty.is_copy())
    }

    fn stats(&self) -> TypeInternPoolStats {
        let mut stats = TypeInternPoolStats {
            struct_count: 0,
            enum_count: 0,
            array_count: 0,
            total: self.types.len(),
        };
        for data in &self.types {
            match data {
                TypeData::DeclaredStruct(_) | TypeData::Struct(_) => stats.struct_count += 1,
                TypeData::DeclaredEnum(_) | TypeData::Enum(_) => stats.enum_count += 1,
                TypeData::Array { .. } => stats.array_count += 1,
                TypeData::ReservedStruct | TypeData::PtrConst { .. } | TypeData::PtrMut { .. } => {}
            }
        }
        stats
    }
}

impl TypeInternPool {
    /// Create a new empty pool.
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(TypeInternPoolInner {
                types: Vec::new(),
                array_map: HashMap::new(),
                ptr_const_map: HashMap::new(),
                ptr_mut_map: HashMap::new(),
                struct_by_file_name: HashMap::new(),
                enum_by_file_name: HashMap::new(),
                symbol_paths: HashMap::new(),
                struct_lang_items: HashMap::new(),
                lang_item_structs: HashMap::new(),
            }),
        }
    }

    /// Consume the completed semantic type universe for backend-facing reads.
    ///
    /// This is the last legal mutation boundary. Request-local symbol interners
    /// remain separate: type definitions retain stable string names rather than
    /// storing a [`Spur`] from a CFG or codegen request.
    pub fn freeze(self) -> FrozenTypeInternPool {
        let inner = self
            .inner
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some((index, entry)) = inner
            .types
            .iter()
            .enumerate()
            .find(|(_, entry)| entry.is_incomplete())
        {
            panic!("cannot freeze incomplete type-pool entry {index}: {entry:?}");
        }
        FrozenTypeInternPool {
            inner: Arc::new(inner),
        }
    }

    /// Set relocation-stable source identities for type-derived symbols.
    pub(crate) fn set_symbol_paths(&self, symbol_paths: HashMap<FileId, String>) {
        self.inner
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .symbol_paths = symbol_paths;
    }

    /// Return the flattened runtime ABI width of `ty` in eight-byte slots.
    ///
    /// This is the canonical layout query shared by sema, CFG temporary
    /// allocation, and code generation. Aggregate arithmetic saturates; sema
    /// rejects layouts that exceed the representable slot range before they
    /// can be materialized.
    pub fn abi_slot_count(&self, ty: Type) -> u32 {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .abi_slot_count(ty)
    }

    /// Register a new struct (nominal - no deduplication).
    ///
    /// Returns the `StructId` (containing the pool index) and whether it was newly inserted.
    /// If a struct with this name in the same defining file already exists, returns the existing
    /// StructId.
    pub fn register_struct(&self, name: Spur, def: StructDef) -> (StructId, bool) {
        let key = (def.file_id, name);
        // Fast path: check with read lock
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(&existing) = inner.struct_by_file_name.get(&key) {
                // Convert InternedType back to StructId via pool_index
                let pool_index = existing.pool_index().expect("struct must have pool index");
                return (StructId::from_pool_index(pool_index), false);
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);

        // Double-check after acquiring write lock
        if let Some(&existing) = inner.struct_by_file_name.get(&key) {
            let pool_index = existing.pool_index().expect("struct must have pool index");
            return (StructId::from_pool_index(pool_index), false);
        }

        // Create new struct type
        let pool_index = inner.next_pool_index();
        let interned = InternedType::from_pool_index(pool_index);
        let struct_id = StructId::from_pool_index(pool_index);

        inner.types.push(TypeData::Struct(StructData { name, def }));
        inner.struct_by_file_name.insert(key, interned);

        (struct_id, true)
    }

    /// Register a named struct identity whose definition will be completed
    /// after declaration type references have been resolved.
    pub(crate) fn declare_struct(&self, name: Spur, shell: StructDef) -> (StructId, bool) {
        let key = (shell.file_id, name);
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(&existing) = inner.struct_by_file_name.get(&key) {
                return (
                    StructId::from_pool_index(
                        existing.pool_index().expect("struct must have pool index"),
                    ),
                    false,
                );
            }
        }

        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(&existing) = inner.struct_by_file_name.get(&key) {
            return (
                StructId::from_pool_index(
                    existing.pool_index().expect("struct must have pool index"),
                ),
                false,
            );
        }

        let pool_index = inner.next_pool_index();
        let interned = InternedType::from_pool_index(pool_index);
        inner
            .types
            .push(TypeData::DeclaredStruct(StructData { name, def: shell }));
        inner.struct_by_file_name.insert(key, interned);
        (StructId::from_pool_index(pool_index), true)
    }

    /// Reserve a struct ID without registering the full definition yet.
    ///
    /// This is used for anonymous structs where we need to know the ID before
    /// we can construct the name (which includes the ID). Call `complete_struct_registration`
    /// with the reserved ID to finish registration.
    ///
    /// # Returns
    ///
    /// Returns the reserved `StructId`. The caller MUST call `complete_struct_registration`
    /// with this ID before any other pool operations that might read this entry.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let struct_id = pool.reserve_struct_id();
    /// let name = format!("__anon_struct_{}", struct_id.0);
    /// let name_spur = interner.get_or_intern(&name);
    /// let def = StructDef { name: name.clone(), ... };
    /// pool.complete_struct_registration(struct_id, name_spur, def);
    /// ```
    pub(crate) fn reserve_struct_id(&self) -> StructId {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);

        let pool_index = inner.next_pool_index();
        inner.types.push(TypeData::ReservedStruct);

        StructId::from_pool_index(pool_index)
    }

    /// Complete the registration of a previously reserved struct ID.
    ///
    /// This must be called after `reserve_struct_id` to fill in the actual struct data.
    /// The struct will be registered with the provided name for lookup purposes.
    ///
    /// # Panics
    ///
    /// Panics if:
    /// - The struct_id wasn't created by `reserve_struct_id`
    /// - The slot at struct_id doesn't contain a placeholder struct
    /// - A struct with the given name already exists
    pub(crate) fn complete_struct_registration(
        &self,
        struct_id: StructId,
        name: Spur,
        def: StructDef,
    ) {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        let pool_index = struct_id.0 as usize;

        // Verify this is a valid reserved slot
        assert!(
            pool_index < inner.types.len(),
            "Invalid reserved struct ID: index {} out of bounds (len {})",
            pool_index,
            inner.types.len()
        );

        assert!(
            matches!(inner.types.get(pool_index), Some(TypeData::ReservedStruct)),
            "pool index {} is not a reserved struct entry",
            pool_index
        );

        assert!(
            !inner.struct_by_file_name.contains_key(&(def.file_id, name)),
            "Struct with this name already exists"
        );

        // Update the placeholder with actual data
        let key = (def.file_id, name);
        inner.types[pool_index] = TypeData::Struct(StructData { name, def });

        // Register in the defining-file lookup.
        let interned = InternedType::from_pool_index(struct_id.pool_index());
        inner.struct_by_file_name.insert(key, interned);
    }

    /// Complete a named struct declaration exactly once.
    pub(crate) fn complete_declared_struct(&self, struct_id: StructId, def: StructDef) {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        let pool_index = struct_id.pool_index() as usize;
        let entry = inner
            .types
            .get_mut(pool_index)
            .unwrap_or_else(|| panic!("Invalid declared struct ID: {pool_index}"));
        match entry {
            TypeData::DeclaredStruct(data) => {
                assert_eq!(
                    data.def.file_id, def.file_id,
                    "completed struct changed defining file"
                );
                assert_eq!(
                    data.def.name.as_str(),
                    def.name.as_str(),
                    "completed struct changed textual name"
                );
                *entry = TypeData::Struct(StructData {
                    name: data.name,
                    def,
                });
            }
            other => panic!(
                "pool index {} is not a declared struct entry: {:?}",
                pool_index, other
            ),
        }
    }

    /// Register a new enum (nominal - no deduplication).
    ///
    /// Returns the `EnumId` (containing the pool index) and whether it was newly inserted.
    /// If an enum with this name in the same defining file already exists, returns the existing
    /// EnumId.
    pub fn register_enum(&self, name: Spur, def: EnumDef) -> (EnumId, bool) {
        let key = (def.file_id, name);
        // Fast path: check with read lock
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(&existing) = inner.enum_by_file_name.get(&key) {
                let pool_index = existing.pool_index().expect("enum must have pool index");
                return (EnumId::from_pool_index(pool_index), false);
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);

        // Double-check after acquiring write lock
        if let Some(&existing) = inner.enum_by_file_name.get(&key) {
            let pool_index = existing.pool_index().expect("enum must have pool index");
            return (EnumId::from_pool_index(pool_index), false);
        }

        // Create new enum type
        let pool_index = inner.next_pool_index();
        let interned = InternedType::from_pool_index(pool_index);

        inner.types.push(TypeData::Enum(EnumData { name, def }));
        inner.enum_by_file_name.insert(key, interned);

        (EnumId::from_pool_index(pool_index), true)
    }

    /// Register a named enum identity whose definition will be completed after
    /// payload type references have been resolved.
    pub(crate) fn declare_enum(&self, name: Spur, shell: EnumDef) -> (EnumId, bool) {
        let key = (shell.file_id, name);
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(&existing) = inner.enum_by_file_name.get(&key) {
                return (
                    EnumId::from_pool_index(
                        existing.pool_index().expect("enum must have pool index"),
                    ),
                    false,
                );
            }
        }

        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(&existing) = inner.enum_by_file_name.get(&key) {
            return (
                EnumId::from_pool_index(existing.pool_index().expect("enum must have pool index")),
                false,
            );
        }

        let pool_index = inner.next_pool_index();
        let interned = InternedType::from_pool_index(pool_index);
        inner
            .types
            .push(TypeData::DeclaredEnum(EnumData { name, def: shell }));
        inner.enum_by_file_name.insert(key, interned);
        (EnumId::from_pool_index(pool_index), true)
    }

    /// Complete a named enum declaration exactly once.
    pub(crate) fn complete_declared_enum(&self, enum_id: EnumId, def: EnumDef) {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        let pool_index = enum_id.pool_index() as usize;
        let entry = inner
            .types
            .get_mut(pool_index)
            .unwrap_or_else(|| panic!("Invalid declared enum ID: {pool_index}"));
        match entry {
            TypeData::DeclaredEnum(data) => {
                assert_eq!(
                    data.def.file_id, def.file_id,
                    "completed enum changed defining file"
                );
                assert_eq!(
                    data.def.name.as_str(),
                    def.name.as_str(),
                    "completed enum changed textual name"
                );
                *entry = TypeData::Enum(EnumData {
                    name: data.name,
                    def,
                });
            }
            other => panic!(
                "pool index {} is not a declared enum entry: {:?}",
                pool_index, other
            ),
        }
    }

    /// Intern an array type (structural - deduplicates).
    ///
    /// Returns the canonical `InternedType` for arrays with this element type and length.
    /// If an identical array type already exists, returns the existing type.
    pub fn intern_array(&self, element: InternedType, len: u64) -> InternedType {
        let key = (element, len);

        // Fast path: check with read lock
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(&existing) = inner.array_map.get(&key) {
                return existing;
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);

        // Double-check after acquiring write lock
        if let Some(&existing) = inner.array_map.get(&key) {
            return existing;
        }

        // Create new array type
        let pool_index = inner.next_pool_index();
        let interned = InternedType::from_pool_index(pool_index);

        inner.types.push(TypeData::Array { element, len });
        inner.array_map.insert(key, interned);

        interned
    }

    /// Intern a ptr const type (structural - deduplicates).
    ///
    /// Returns the canonical `InternedType` for pointers to this pointee type.
    /// If an identical pointer type already exists, returns the existing type.
    pub fn intern_ptr_const(&self, pointee: InternedType) -> InternedType {
        // Fast path: check with read lock
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(&existing) = inner.ptr_const_map.get(&pointee) {
                return existing;
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);

        // Double-check after acquiring write lock
        if let Some(&existing) = inner.ptr_const_map.get(&pointee) {
            return existing;
        }

        // Create new pointer type
        let pool_index = inner.next_pool_index();
        let interned = InternedType::from_pool_index(pool_index);

        inner.types.push(TypeData::PtrConst { pointee });
        inner.ptr_const_map.insert(pointee, interned);

        interned
    }

    /// Intern a ptr mut type (structural - deduplicates).
    ///
    /// Returns the canonical `InternedType` for mutable pointers to this pointee type.
    /// If an identical pointer type already exists, returns the existing type.
    pub fn intern_ptr_mut(&self, pointee: InternedType) -> InternedType {
        // Fast path: check with read lock
        {
            let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(&existing) = inner.ptr_mut_map.get(&pointee) {
                return existing;
            }
        }

        // Slow path: acquire write lock
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);

        // Double-check after acquiring write lock
        if let Some(&existing) = inner.ptr_mut_map.get(&pointee) {
            return existing;
        }

        // Create new pointer type
        let pool_index = inner.next_pool_index();
        let interned = InternedType::from_pool_index(pool_index);

        inner.types.push(TypeData::PtrMut { pointee });
        inner.ptr_mut_map.insert(pointee, interned);

        interned
    }

    /// Look up a struct by defining file and source name.
    pub fn get_struct_by_file_name(&self, file_id: FileId, name: Spur) -> Option<InternedType> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.struct_by_file_name.get(&(file_id, name)).copied()
    }

    /// Look up an enum by defining file and source name.
    pub fn get_enum_by_file_name(&self, file_id: FileId, name: Spur) -> Option<InternedType> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.enum_by_file_name.get(&(file_id, name)).copied()
    }

    /// Look up an array type by element and length.
    pub fn get_array(&self, element: InternedType, len: u64) -> Option<InternedType> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.array_map.get(&(element, len)).copied()
    }

    /// Get type data for a composite type.
    ///
    /// Returns `None` for primitive types (use `InternedType::is_primitive()` first).
    ///
    /// # Panics
    ///
    /// Panics if the index is invalid.
    pub fn get(&self, ty: InternedType) -> Option<TypeData> {
        if ty.is_primitive() {
            return None;
        }

        let pool_index = ty.pool_index().expect("non-primitive must have pool index");
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        Some(inner.types[pool_index as usize].clone())
    }

    /// Check if this is a struct type.
    pub fn is_struct(&self, ty: InternedType) -> bool {
        if ty.is_primitive() {
            return false;
        }
        matches!(
            self.get(ty),
            Some(TypeData::DeclaredStruct(_) | TypeData::Struct(_))
        )
    }

    /// Check if this is an enum type.
    pub fn is_enum(&self, ty: InternedType) -> bool {
        if ty.is_primitive() {
            return false;
        }
        matches!(
            self.get(ty),
            Some(TypeData::DeclaredEnum(_) | TypeData::Enum(_))
        )
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
    // Direct nominal-ID access
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
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.struct_def(struct_id).clone()
    }

    /// Get a struct definition without panicking on an invalid or wrong-kind ID.
    pub fn try_struct_def(&self, struct_id: StructId) -> Option<StructDef> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.try_struct_def(struct_id).cloned()
    }

    /// Return the stable standard-library identity carried by a nominal type.
    pub fn struct_lang_item(&self, struct_id: StructId) -> Option<LangItem> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.struct_lang_items.get(&struct_id).copied()
    }

    /// Return the nominal type carrying a stable standard-library identity.
    pub fn lang_item_type(&self, lang_item: LangItem) -> Option<Type> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner
            .lang_item_structs
            .get(&lang_item)
            .copied()
            .map(Type::new_struct)
    }

    /// Assign an explicitly authorized language item to a registered nominal.
    pub fn set_struct_lang_item(&self, struct_id: StructId, lang_item: LangItem) {
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        assert!(
            matches!(
                inner.types.get(struct_id.0 as usize),
                Some(TypeData::DeclaredStruct(_) | TypeData::Struct(_))
            ),
            "language items can only be assigned to registered structs"
        );
        if let Some(existing) = inner.lang_item_structs.get(&lang_item) {
            assert_eq!(
                *existing, struct_id,
                "a language item can only identify one canonical struct"
            );
        }
        if let Some(existing) = inner.struct_lang_items.get(&struct_id) {
            assert_eq!(
                *existing, lang_item,
                "a struct can only carry one language item"
            );
        }
        inner.struct_lang_items.insert(struct_id, lang_item);
        inner.lang_item_structs.insert(lang_item, struct_id);
    }

    /// Whether a nominal is the canonical trusted standard-library StrBuf.
    pub fn is_strbuf(&self, struct_id: StructId) -> bool {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.struct_lang_items.get(&struct_id) == Some(&LangItem::StrBuf)
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
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.enum_def(enum_id).clone()
    }

    /// Get an enum definition without panicking on an invalid or wrong-kind ID.
    pub fn try_enum_def(&self, enum_id: EnumId) -> Option<EnumDef> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.try_enum_def(enum_id).cloned()
    }

    /// The symbol-name component for functions derived from a struct —
    /// methods (`P.get`), associated functions (`P::make`), destructors
    /// (`P.__drop`), and drop glue (`__rue_drop_P`) — RUE-571.
    ///
    /// Same-named nominal types across files are legal (RUE-558), but these
    /// symbols are program-wide identities: when this struct's source name is
    /// registered by more than one struct or enum, the name is qualified with
    /// the defining file (`P$left_2fmodel_2erue`). `$` cannot appear in a source
    /// identifier, so a qualified name can never collide with a real type;
    /// unambiguous names (the common case) are returned bare, keeping symbols
    /// and `--emit` output unchanged. Builtins are never qualified (their
    /// symbols pair with runtime-provided definitions).
    ///
    /// Every layer that names a function after a type — sema (definition and
    /// call sites), the drop-glue generator in `rue-compiler`, and both
    /// codegen backends — must derive the name through this ONE helper so
    /// definitions and calls meet at link time.
    pub fn struct_symbol_name(&self, struct_id: StructId) -> String {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.struct_symbol_name(struct_id)
    }

    /// The symbol-name component for an enum's drop glue (`__rue_drop_E`),
    /// file-qualified when another struct or enum has the same source name.
    /// See [`Self::struct_symbol_name`] (RUE-571) — same rule, same reason.
    pub fn enum_symbol_name(&self, enum_id: EnumId) -> String {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.enum_symbol_name(enum_id)
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
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
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
        let mut inner = self.inner.write().unwrap_or_else(PoisonError::into_inner);
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
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.array_def(array_id)
    }

    /// Get an array definition without panicking on an invalid or wrong-kind ID.
    pub fn try_array_def(&self, array_id: ArrayTypeId) -> Option<(Type, u64)> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.try_array_def(array_id)
    }

    /// Intern an array type from a Type element.
    ///
    /// This is a helper method that converts the Type to InternedType
    /// and then interns the array.
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

    /// Look up an array type by Type element and length.
    ///
    /// Returns None if no such array exists in the pool.
    pub fn get_array_by_type(&self, element_type: Type, len: u64) -> Option<ArrayTypeId> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        let element_interned = Self::type_to_interned_recursive(element_type);
        let array_interned = inner.array_map.get(&(element_interned, len))?;
        Some(ArrayTypeId::from_pool_index(
            array_interned
                .pool_index()
                .expect("array must have pool index"),
        ))
    }

    /// Intern a ptr const type from a Type pointee.
    ///
    /// # Panics
    ///
    /// Panics if the pointee type contains a struct/enum that isn't in the pool.
    pub fn intern_ptr_const_from_type(&self, pointee_type: Type) -> PtrConstTypeId {
        let pointee_interned = Self::type_to_interned_recursive(pointee_type);
        let ptr_interned = self.intern_ptr_const(pointee_interned);
        PtrConstTypeId::from_pool_index(
            ptr_interned
                .pool_index()
                .expect("ptr const must have pool index"),
        )
    }

    /// Intern a ptr mut type from a Type pointee.
    ///
    /// # Panics
    ///
    /// Panics if the pointee type contains a struct/enum that isn't in the pool.
    pub fn intern_ptr_mut_from_type(&self, pointee_type: Type) -> PtrMutTypeId {
        let pointee_interned = Self::type_to_interned_recursive(pointee_type);
        let ptr_interned = self.intern_ptr_mut(pointee_interned);
        PtrMutTypeId::from_pool_index(
            ptr_interned
                .pool_index()
                .expect("ptr mut must have pool index"),
        )
    }

    /// Get ptr const pointee type if this is a ptr const type.
    pub fn ptr_const_def(&self, ptr_id: PtrConstTypeId) -> Type {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.ptr_const_def(ptr_id)
    }

    /// Get ptr mut pointee type if this is a ptr mut type.
    pub fn ptr_mut_def(&self, ptr_id: PtrMutTypeId) -> Type {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.ptr_mut_def(ptr_id)
    }

    /// Convert Type to InternedType recursively (handles composite types).
    ///
    /// Used when structural interning needs the pool encoding of a [`Type`].
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
            TypeKind::PtrConst(id) => InternedType::from_pool_index(id.pool_index()),
            TypeKind::PtrMut(id) => InternedType::from_pool_index(id.pool_index()),
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
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner
            .types
            .iter()
            .enumerate()
            .filter_map(|(idx, data)| match data {
                TypeData::DeclaredStruct(_) | TypeData::Struct(_) => {
                    Some(StructId::from_pool_index(
                        checked_pool_index(idx).expect("type pool index invariant"),
                    ))
                }
                _ => None,
            })
            .collect()
    }

    /// Get all enum IDs registered in the pool.
    ///
    /// Returns a vector of all EnumId values, useful for iterating over all
    /// enums.
    pub fn all_enum_ids(&self) -> Vec<EnumId> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner
            .types
            .iter()
            .enumerate()
            .filter_map(|(idx, data)| match data {
                TypeData::DeclaredEnum(_) | TypeData::Enum(_) => Some(EnumId::from_pool_index(
                    checked_pool_index(idx).expect("type pool index invariant"),
                )),
                _ => None,
            })
            .collect()
    }

    /// Get all array IDs registered in the pool.
    ///
    /// Returns a vector of all ArrayTypeId values, useful for iterating over all
    /// arrays (e.g., for drop glue synthesis).
    pub fn all_array_ids(&self) -> Vec<ArrayTypeId> {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner
            .types
            .iter()
            .enumerate()
            .filter_map(|(idx, data)| match data {
                TypeData::Array { .. } => Some(ArrayTypeId::from_pool_index(
                    checked_pool_index(idx).expect("type pool index invariant"),
                )),
                _ => None,
            })
            .collect()
    }

    /// Get the number of composite types in the pool.
    pub fn len(&self) -> usize {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.types.len()
    }

    /// Check if the pool is empty (no composite types).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get statistics about the pool contents.
    pub fn stats(&self) -> TypeInternPoolStats {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        inner.stats()
    }

    pub(crate) fn safe_type_name(&self, ty: Type) -> String {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .safe_type_name(ty)
    }

    pub(crate) fn is_copy_type(&self, ty: Type) -> bool {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .is_copy_type(ty)
    }

    // ========================================================================
    // Primitive conversion helpers
    // ========================================================================

    /// Convert a [`Type`] to an [`InternedType`] when no pool lookup is needed.
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
            // Struct, enum, array, pointer, and module require pool lookup by ID - we need the name
            // to find the interned type. This conversion is not straightforward
            // without additional context. Return None to indicate we can't convert.
            TypeKind::Struct(_)
            | TypeKind::Enum(_)
            | TypeKind::Array(_)
            | TypeKind::PtrConst(_)
            | TypeKind::PtrMut(_)
            | TypeKind::Module(_) => None,
            // ComptimeType is a comptime-only type, cannot be interned for runtime
            TypeKind::ComptimeType => None,
        }
    }

    /// Convert a primitive [`InternedType`] back to [`Type`].
    ///
    /// Composite encodings return `None` because their concrete ID kind must be
    /// read from the pool.
    pub fn interned_to_type(&self, ty: InternedType) -> Option<Type> {
        if !ty.is_primitive() {
            return None;
        }
        Type::try_from_u32(ty.0)
    }
}

impl FrozenTypeInternPool {
    pub fn new() -> Self {
        TypeInternPool::new().freeze()
    }

    /// Return the flattened runtime ABI width of `ty` in eight-byte slots.
    pub fn abi_slot_count(&self, ty: Type) -> u32 {
        self.inner.abi_slot_count(ty)
    }

    /// Borrow a completed nominal struct definition without locking or cloning.
    pub fn struct_def(&self, id: StructId) -> &StructDef {
        self.inner.struct_def(id)
    }

    pub fn try_struct_def(&self, id: StructId) -> Option<&StructDef> {
        self.inner.try_struct_def(id)
    }

    /// Borrow a completed nominal enum definition without locking or cloning.
    pub fn enum_def(&self, id: EnumId) -> &EnumDef {
        self.inner.enum_def(id)
    }

    pub fn try_enum_def(&self, id: EnumId) -> Option<&EnumDef> {
        self.inner.try_enum_def(id)
    }

    pub fn array_def(&self, id: ArrayTypeId) -> (Type, u64) {
        self.inner.array_def(id)
    }

    pub fn try_array_def(&self, id: ArrayTypeId) -> Option<(Type, u64)> {
        self.inner.try_array_def(id)
    }

    pub fn ptr_const_def(&self, id: PtrConstTypeId) -> Type {
        self.inner.ptr_const_def(id)
    }

    pub fn ptr_mut_def(&self, id: PtrMutTypeId) -> Type {
        self.inner.ptr_mut_def(id)
    }

    /// Look up an already-completed mutable pointer type without modifying the pool.
    pub fn get_ptr_mut_by_type(&self, pointee_type: Type) -> Option<PtrMutTypeId> {
        let pointee = TypeInternPool::type_to_interned_recursive(pointee_type);
        let interned = self.inner.ptr_mut_map.get(&pointee)?;
        Some(PtrMutTypeId::from_pool_index(interned.pool_index()?))
    }

    pub fn struct_lang_item(&self, id: StructId) -> Option<LangItem> {
        self.inner.struct_lang_items.get(&id).copied()
    }

    pub fn lang_item_type(&self, item: LangItem) -> Option<Type> {
        self.inner
            .lang_item_structs
            .get(&item)
            .copied()
            .map(Type::new_struct)
    }

    pub fn is_strbuf(&self, id: StructId) -> bool {
        self.struct_lang_item(id) == Some(LangItem::StrBuf)
    }

    pub fn struct_symbol_name(&self, id: StructId) -> String {
        self.inner.struct_symbol_name(id)
    }

    pub fn enum_symbol_name(&self, id: EnumId) -> String {
        self.inner.enum_symbol_name(id)
    }

    pub fn all_struct_ids(&self) -> impl Iterator<Item = StructId> + '_ {
        self.inner
            .types
            .iter()
            .enumerate()
            .filter(|(_, data)| matches!(data, TypeData::Struct(_)))
            .map(|(index, _)| {
                StructId::from_pool_index(
                    checked_pool_index(index).expect("type pool index invariant"),
                )
            })
    }

    pub fn all_enum_ids(&self) -> impl Iterator<Item = EnumId> + '_ {
        self.inner
            .types
            .iter()
            .enumerate()
            .filter(|(_, data)| matches!(data, TypeData::Enum(_)))
            .map(|(index, _)| {
                EnumId::from_pool_index(
                    checked_pool_index(index).expect("type pool index invariant"),
                )
            })
    }

    pub fn all_array_ids(&self) -> impl Iterator<Item = ArrayTypeId> + '_ {
        self.inner
            .types
            .iter()
            .enumerate()
            .filter_map(|(index, data)| {
                matches!(data, TypeData::Array { .. }).then(|| {
                    ArrayTypeId::from_pool_index(
                        checked_pool_index(index).expect("type pool index invariant"),
                    )
                })
            })
    }

    pub fn all_ptr_const_ids(&self) -> impl Iterator<Item = PtrConstTypeId> + '_ {
        self.inner
            .types
            .iter()
            .enumerate()
            .filter(|(_, data)| matches!(data, TypeData::PtrConst { .. }))
            .map(|(index, _)| {
                PtrConstTypeId::from_pool_index(
                    checked_pool_index(index).expect("type pool index invariant"),
                )
            })
    }

    pub fn all_ptr_mut_ids(&self) -> impl Iterator<Item = PtrMutTypeId> + '_ {
        self.inner
            .types
            .iter()
            .enumerate()
            .filter(|(_, data)| matches!(data, TypeData::PtrMut { .. }))
            .map(|(index, _)| {
                PtrMutTypeId::from_pool_index(
                    checked_pool_index(index).expect("type pool index invariant"),
                )
            })
    }

    pub fn len(&self) -> usize {
        self.inner.types.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.types.is_empty()
    }

    pub fn stats(&self) -> TypeInternPoolStats {
        self.inner.stats()
    }

    pub(crate) fn safe_type_name(&self, ty: Type) -> String {
        self.inner.safe_type_name(ty)
    }

    pub(crate) fn is_copy_type(&self, ty: Type) -> bool {
        self.inner.is_copy_type(ty)
    }
}

impl Default for FrozenTypeInternPool {
    fn default() -> Self {
        Self::new()
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
    /// This is used when analysis needs an independent copy of the pool while
    /// preserving the already-interned type data.
    fn clone(&self) -> Self {
        let inner = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        Self {
            inner: RwLock::new(TypeInternPoolInner {
                types: inner.types.clone(),
                array_map: inner.array_map.clone(),
                ptr_const_map: inner.ptr_const_map.clone(),
                ptr_mut_map: inner.ptr_mut_map.clone(),
                struct_by_file_name: inner.struct_by_file_name.clone(),
                enum_by_file_name: inner.enum_by_file_name.clone(),
                symbol_paths: inner.symbol_paths.clone(),
                struct_lang_items: inner.struct_lang_items.clone(),
                lang_item_structs: inner.lang_item_structs.clone(),
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
    use crate::StructField;
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
        assert_eq!(
            InternedType::ERROR.index(),
            Type::ERROR.raw_encoding(),
            "compatibility wrapper must not swap Error and Never"
        );
        assert_eq!(
            InternedType::NEVER.index(),
            Type::NEVER.raw_encoding(),
            "compatibility wrapper must not swap Error and Never"
        );
        assert!(InternedType::try_from_raw(13).is_none());
        assert!(InternedType::try_from_raw(255).is_none());
        assert!(InternedType::try_from_raw(Type::COMPTIME_TYPE.raw_encoding()).is_none());
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

    fn struct_def(name: &str, fields: Vec<StructField>) -> StructDef {
        StructDef {
            name: name.into(),
            fields,
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: FileId::DEFAULT,
        }
    }

    fn enum_def(name: &str) -> EnumDef {
        EnumDef {
            name: name.into(),
            variants: vec![],
            variant_payloads: vec![],
            is_pub: false,
            file_id: FileId::DEFAULT,
        }
    }

    #[test]
    fn checked_pool_index_enforces_type_payload_capacity() {
        let maximum = type_encoding::MAX_PAYLOAD as usize;
        assert_eq!(
            checked_pool_index(maximum),
            Some(type_encoding::MAX_PAYLOAD)
        );
        assert_eq!(checked_pool_index(maximum + 1), None);
    }

    #[test]
    fn declared_struct_has_identity_before_single_completion() {
        let interner = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let name = interner.get_or_intern("Node");
        let (id, is_new) = pool.declare_struct(name, struct_def("Node", vec![]));
        assert!(is_new);

        let interned = pool.struct_id_to_interned(id);
        assert!(matches!(
            pool.get(interned),
            Some(TypeData::DeclaredStruct(_))
        ));
        assert!(pool.is_struct(interned));
        assert!(pool.get_struct_def(interned).is_none());

        // The declared identity is legal in a recursive pointer graph before
        // the nominal definition completes.
        let next_id = pool.intern_ptr_mut_from_type(Type::new_struct(id));
        let next = Type::new_ptr_mut(next_id);
        pool.complete_declared_struct(
            id,
            struct_def(
                "Node",
                vec![StructField {
                    name: "next".into(),
                    ty: next,
                }],
            ),
        );

        assert!(matches!(pool.get(interned), Some(TypeData::Struct(_))));
        assert_eq!(pool.get_struct_def(interned).unwrap().fields[0].ty, next);
        let frozen = pool.freeze();
        assert_eq!(frozen.ptr_mut_def(next_id), Type::new_struct(id));
    }

    #[test]
    #[should_panic(expected = "is not a declared struct entry")]
    fn declared_struct_cannot_complete_twice() {
        let interner = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let name = interner.get_or_intern("Once");
        let (id, _) = pool.declare_struct(name, struct_def("Once", vec![]));
        pool.complete_declared_struct(id, struct_def("Once", vec![]));
        pool.complete_declared_struct(id, struct_def("Once", vec![]));
    }

    #[test]
    #[should_panic(expected = "completed struct changed textual name")]
    fn declared_struct_completion_rejects_name_change() {
        let interner = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let name = interner.get_or_intern("Before");
        let (id, _) = pool.declare_struct(name, struct_def("Before", vec![]));
        pool.complete_declared_struct(id, struct_def("After", vec![]));
    }

    #[test]
    #[should_panic(expected = "is not a declared enum entry")]
    fn declared_enum_cannot_complete_twice() {
        let interner = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let name = interner.get_or_intern("Once");
        let (id, _) = pool.declare_enum(name, enum_def("Once"));
        pool.complete_declared_enum(id, enum_def("Once"));
        pool.complete_declared_enum(id, enum_def("Once"));
    }

    #[test]
    #[should_panic(expected = "completed enum changed textual name")]
    fn declared_enum_completion_rejects_name_change() {
        let interner = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let name = interner.get_or_intern("Before");
        let (id, _) = pool.declare_enum(name, enum_def("Before"));
        pool.complete_declared_enum(id, enum_def("After"));
    }

    #[test]
    #[should_panic(expected = "is not a declared struct entry")]
    fn declared_completion_rejects_wrong_nominal_kind() {
        let interner = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let name = interner.get_or_intern("Choice");
        let (id, _) = pool.declare_enum(name, enum_def("Choice"));
        pool.complete_declared_struct(
            StructId::from_pool_index(id.pool_index()),
            struct_def("Choice", vec![]),
        );
    }

    #[test]
    #[should_panic(expected = "cannot freeze incomplete type-pool entry")]
    fn freeze_rejects_declared_entry() {
        let interner = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let name = interner.get_or_intern("Later");
        pool.declare_struct(name, struct_def("Later", vec![]));
        let _ = pool.freeze();
    }

    #[test]
    #[should_panic(expected = "cannot freeze incomplete type-pool entry")]
    fn freeze_rejects_reserved_entry() {
        let pool = TypeInternPool::new();
        pool.reserve_struct_id();
        let _ = pool.freeze();
    }

    #[test]
    fn error_recovery_structural_types_may_freeze() {
        let pool = TypeInternPool::new();
        let array_id = pool.intern_array_from_type(Type::ERROR, 1);
        let frozen = pool.freeze();
        assert_eq!(frozen.array_def(array_id), (Type::ERROR, 1));
    }

    #[test]
    fn freeze_preserves_complete_nominals_and_borrows_stable_definitions() {
        let declarations = ThreadedRodeo::default();
        let pool = TypeInternPool::new();
        let name = declarations.get_or_intern("Owner");
        let (owner, _) = pool.register_struct(
            name,
            StructDef {
                name: "Owner".into(),
                fields: vec![StructField {
                    name: "value".into(),
                    ty: Type::I64,
                }],
                is_copy: false,
                is_linear: false,
                destructor: Some("Owner.__drop".into()),
                is_builtin: false,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        let owner_type = Type::new_struct(owner);
        let mutable_symbol = pool.struct_symbol_name(owner);
        let mutable_name = owner_type.safe_name_with_pool(Some(&pool));
        let mutable_slots = pool.abi_slot_count(owner_type);
        let mutable_stats = pool.stats();

        let frozen = pool.freeze();
        let first = frozen.struct_def(owner);
        let second = frozen.struct_def(owner);
        assert!(std::ptr::eq(first, second));
        assert_eq!(frozen.all_struct_ids().collect::<Vec<_>>(), [owner]);
        assert_eq!(frozen.struct_symbol_name(owner), mutable_symbol);
        assert_eq!(
            owner_type.safe_name_with_frozen_pool(Some(&frozen)),
            mutable_name
        );
        assert_eq!(frozen.abi_slot_count(owner_type), mutable_slots);
        assert_eq!(frozen.stats(), mutable_stats);

        // Destructor provenance crosses the boundary as a stable string. A
        // backend request chooses its own symbol universe and interns it there.
        let request_symbols = ThreadedRodeo::default();
        let destructor = first.destructor.as_deref().unwrap();
        let request_symbol = request_symbols.get_or_intern(destructor);
        assert_eq!(request_symbols.resolve(&request_symbol), "Owner.__drop");
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
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
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
    fn language_item_reverse_index_is_unique_and_deterministic() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let make_def = |name: &str, file_id| StructDef {
            name: name.to_string(),
            fields: vec![],
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: true,
            file_id,
        };
        let (canonical, _) = pool.register_struct(
            interner.get_or_intern("CanonicalStrBuf"),
            make_def("CanonicalStrBuf", FileId::DEFAULT),
        );
        pool.set_struct_lang_item(canonical, LangItem::StrBuf);
        pool.set_struct_lang_item(canonical, LangItem::StrBuf);
        assert_eq!(
            pool.lang_item_type(LangItem::StrBuf),
            Some(Type::new_struct(canonical))
        );
    }

    #[test]
    #[should_panic(expected = "a language item can only identify one canonical struct")]
    fn duplicate_language_item_assignment_is_rejected() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let make_def = |name: &str, file_id| StructDef {
            name: name.to_string(),
            fields: vec![],
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: true,
            file_id,
        };
        let (canonical, _) = pool.register_struct(
            interner.get_or_intern("CanonicalStrBuf"),
            make_def("CanonicalStrBuf", FileId::DEFAULT),
        );
        pool.set_struct_lang_item(canonical, LangItem::StrBuf);
        let other_file = FileId::new(1);
        let (duplicate, _) = pool.register_struct(
            interner.get_or_intern("OtherStrBuf"),
            make_def("OtherStrBuf", other_file),
        );
        pool.set_struct_lang_item(duplicate, LangItem::StrBuf);
    }

    #[test]
    fn test_pool_register_enum() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let name = interner.get_or_intern("Color");

        let def = EnumDef {
            name: "Color".to_string(),
            variants: vec!["Red".to_string(), "Green".to_string(), "Blue".to_string()],
            variant_payloads: Vec::new(),
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
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
    fn test_pool_get_struct_by_file_name() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let name = interner.get_or_intern("Point");

        assert!(
            pool.get_struct_by_file_name(rue_span::FileId::DEFAULT, name)
                .is_none()
        );

        let def = StructDef {
            name: "Point".to_string(),
            fields: vec![],
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };

        let (struct_id, _) = pool.register_struct(name, def);
        let expected = pool.struct_id_to_interned(struct_id);
        assert_eq!(
            pool.get_struct_by_file_name(rue_span::FileId::DEFAULT, name),
            Some(expected)
        );
    }

    #[test]
    fn test_pool_get_enum_by_file_name() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let name = interner.get_or_intern("Status");

        assert!(
            pool.get_enum_by_file_name(rue_span::FileId::DEFAULT, name)
                .is_none()
        );

        let def = EnumDef {
            name: "Status".to_string(),
            variants: vec!["Active".to_string(), "Inactive".to_string()],
            variant_payloads: Vec::new(),
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };

        let (enum_id, _) = pool.register_enum(name, def);
        let expected = pool.enum_id_to_interned(enum_id);
        assert_eq!(
            pool.get_enum_by_file_name(rue_span::FileId::DEFAULT, name),
            Some(expected)
        );
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
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
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
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        let (struct_id, _) = pool.register_struct(struct_name, struct_def);
        let struct_ty = pool.struct_id_to_interned(struct_id);

        let enum_name = interner.get_or_intern("Color");
        let enum_def = EnumDef {
            name: "Color".to_string(),
            variants: vec!["Red".to_string()],
            variant_payloads: Vec::new(),
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
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
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        let (struct_id, _) = pool.register_struct(name, def.clone());

        // Direct nominal-ID lookup returns the canonical definition.
        let retrieved = pool.struct_def(struct_id);
        assert_eq!(retrieved.name, def.name);
        assert_eq!(retrieved.is_copy, def.is_copy);

        // The pool encoding resolves to the same definition.
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
            variant_payloads: Vec::new(),
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        let (enum_id, _) = pool.register_enum(name, def.clone());

        // Direct nominal-ID lookup returns the canonical definition.
        let retrieved = pool.enum_def(enum_id);
        assert_eq!(retrieved.name, def.name);
        assert_eq!(retrieved.variants.len(), 2);

        // The pool encoding resolves to the same definition.
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
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
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
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
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
                variant_payloads: Vec::new(),
                is_pub: false,
                file_id: rue_span::FileId::DEFAULT,
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
        assert_eq!(pool.type_to_interned(Type::BOOL), Some(InternedType::BOOL));
        assert_eq!(pool.type_to_interned(Type::UNIT), Some(InternedType::UNIT));
        assert_eq!(
            pool.type_to_interned(Type::NEVER),
            Some(InternedType::NEVER)
        );
        assert_eq!(
            pool.type_to_interned(Type::ERROR),
            Some(InternedType::ERROR)
        );

        // Composite types return None (need name lookup)
        assert!(
            pool.type_to_interned(Type::new_struct(crate::types::StructId(0)))
                .is_none()
        );
        assert!(
            pool.type_to_interned(Type::new_enum(crate::types::EnumId(0)))
                .is_none()
        );
        assert!(
            pool.type_to_interned(Type::new_array(crate::types::ArrayTypeId(0)))
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
        assert_eq!(pool.interned_to_type(InternedType::BOOL), Some(Type::BOOL));
        assert_eq!(pool.interned_to_type(InternedType::UNIT), Some(Type::UNIT));
        assert_eq!(
            pool.interned_to_type(InternedType::NEVER),
            Some(Type::NEVER)
        );
        assert_eq!(
            pool.interned_to_type(InternedType::ERROR),
            Some(Type::ERROR)
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
                            is_linear: false,
                            destructor: None,
                            is_builtin: false,
                            is_pub: false,
                            file_id: rue_span::FileId::DEFAULT,
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
            assert!(
                pool.get_struct_by_file_name(rue_span::FileId::DEFAULT, *name)
                    .is_some()
            );
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

    // ========================================================================
    // Struct ID reservation tests
    // ========================================================================

    #[test]
    fn test_pool_reserve_and_complete_struct() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        // Reserve an ID
        let struct_id = pool.reserve_struct_id();
        assert_eq!(struct_id.pool_index(), 0);
        assert_eq!(pool.len(), 1); // Placeholder was pushed

        // Use the ID to create a name
        let name_str = format!("__anon_struct_{}", struct_id.0);
        let name = interner.get_or_intern(&name_str);

        let def = StructDef {
            name: name_str.clone(),
            fields: vec![],
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };

        // Complete registration
        pool.complete_struct_registration(struct_id, name, def);

        // Verify registration succeeded
        assert_eq!(pool.len(), 1); // No new entry, just updated
        assert!(
            pool.get_struct_by_file_name(rue_span::FileId::DEFAULT, name)
                .is_some()
        );

        // Can retrieve the struct definition
        let retrieved = pool.struct_def(struct_id);
        assert_eq!(retrieved.name, name_str);
    }

    /// RUE-571: a struct name registered by two files yields file-qualified
    /// symbol names; a unique name stays bare; builtins are never qualified.
    #[test]
    fn test_struct_symbol_name_qualifies_only_colliding_names() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let mk = |name: &str, file: u32, is_builtin: bool| StructDef {
            name: name.to_string(),
            fields: vec![],
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin,
            is_pub: true,
            file_id: rue_span::FileId::new(file),
        };

        let p_sym = interner.get_or_intern("P");
        let (p1, _) = pool.register_struct(p_sym, mk("P", 1, false));
        let (p2, _) = pool.register_struct(p_sym, mk("P", 2, false));
        let q_sym = interner.get_or_intern("Q");
        let (q, _) = pool.register_struct(q_sym, mk("Q", 1, false));
        let b_sym = interner.get_or_intern("StrBufTest");
        let (b1, _) = pool.register_struct(b_sym, mk("StrBufTest", 0, true));
        let (b2, _) = pool.register_struct(b_sym, mk("StrBufTest", 3, false));

        // Colliding user structs are qualified with their defining file.
        assert_eq!(pool.struct_symbol_name(p1), "P$1");
        assert_eq!(pool.struct_symbol_name(p2), "P$2");
        // A unique name stays bare.
        assert_eq!(pool.struct_symbol_name(q), "Q");
        // A builtin is never qualified, even when its name collides; the
        // colliding user struct still is, so the pair stays distinct.
        assert_eq!(pool.struct_symbol_name(b1), "StrBufTest");
        assert_eq!(pool.struct_symbol_name(b2), "StrBufTest$3");
    }

    #[test]
    fn type_symbol_names_use_stable_paths_and_survive_pool_clone() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();
        let left_id = FileId::new(42);
        let right_id = FileId::new(7);
        pool.set_symbol_paths(HashMap::from([
            (left_id, "left/shared.rue".to_string()),
            (right_id, "right/shared.rue".to_string()),
        ]));

        let payload = interner.get_or_intern("Payload");
        let struct_def = |file_id| StructDef {
            name: "Payload".to_string(),
            fields: vec![],
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: true,
            file_id,
        };
        let (left_struct, _) = pool.register_struct(payload, struct_def(left_id));
        let (right_struct, _) = pool.register_struct(payload, struct_def(right_id));

        let choice = interner.get_or_intern("Choice");
        let enum_def = |file_id| EnumDef {
            name: "Choice".to_string(),
            variants: vec!["Value".to_string()],
            variant_payloads: vec![vec![]],
            is_pub: true,
            file_id,
        };
        let (left_enum, _) = pool.register_enum(choice, enum_def(left_id));
        let (right_enum, _) = pool.register_enum(choice, enum_def(right_id));

        let cloned = pool.clone();
        assert_eq!(
            cloned.struct_symbol_name(left_struct),
            "Payload$left_2fshared_2erue"
        );
        assert_eq!(
            cloned.struct_symbol_name(right_struct),
            "Payload$right_2fshared_2erue"
        );
        assert_eq!(
            cloned.enum_symbol_name(left_enum),
            "Choice$left_2fshared_2erue"
        );
        assert_eq!(
            cloned.enum_symbol_name(right_enum),
            "Choice$right_2fshared_2erue"
        );
    }

    #[test]
    fn test_pool_reserve_multiple_structs() {
        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::default();

        // Reserve multiple IDs
        let id1 = pool.reserve_struct_id();
        let id2 = pool.reserve_struct_id();
        let id3 = pool.reserve_struct_id();

        assert_eq!(id1.pool_index(), 0);
        assert_eq!(id2.pool_index(), 1);
        assert_eq!(id3.pool_index(), 2);
        assert_eq!(pool.len(), 3);

        // Complete them in any order (here: reverse)
        for (i, id) in [(2, id3), (1, id2), (0, id1)] {
            let name_str = format!("__anon_struct_{}", i);
            let name = interner.get_or_intern(&name_str);
            let def = StructDef {
                name: name_str,
                fields: vec![],
                is_copy: false,
                is_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: rue_span::FileId::DEFAULT,
            };
            pool.complete_struct_registration(id, name, def);
        }

        // All three should be registered
        assert_eq!(pool.stats().struct_count, 3);
    }

    // Compile-time assertion that TypeInternPool is Send + Sync
    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn test_pool_is_send_sync() {
        assert_send_sync::<TypeInternPool>();
        assert_send_sync::<FrozenTypeInternPool>();
    }

    #[test]
    fn test_ptr_type_error_name_shows_pointee() {
        // Diagnostics must render the pointee type, not a bare `<ptr const>`
        // placeholder that makes "expected X, found X" messages useless
        // (RUE-8). Verify `safe_name_with_pool` resolves the pointee through
        // the pool for both const and mut pointers, including nested pointers.
        let pool = TypeInternPool::new();

        let pc = pool.intern_ptr_const_from_type(Type::I32);
        assert_eq!(
            Type::new_ptr_const(pc).safe_name_with_pool(Some(&pool)),
            "ptr const i32"
        );

        let pm = pool.intern_ptr_mut_from_type(Type::U64);
        assert_eq!(
            Type::new_ptr_mut(pm).safe_name_with_pool(Some(&pool)),
            "ptr mut u64"
        );

        // Nested: ptr const (ptr mut i32)
        let inner = Type::new_ptr_mut(pool.intern_ptr_mut_from_type(Type::I32));
        let outer = pool.intern_ptr_const_from_type(inner);
        assert_eq!(
            Type::new_ptr_const(outer).safe_name_with_pool(Some(&pool)),
            "ptr const ptr mut i32"
        );

        // Without a pool, fall back to a stable id-tagged placeholder.
        assert_eq!(
            Type::new_ptr_const(pc).safe_name_with_pool(None),
            format!("<ptr const#{}>", pc.0)
        );
    }
}
