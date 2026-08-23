//! Type system for Rue.
//!
//! A [`Type`] is a compact `u32` newtype. Primitives use direct tags, while
//! composite types carry a typed payload issued by a [`TypeInternPool`](crate::TypeInternPool)
//! (ADR-0024). Equality is therefore cheap and free of self-referential
//! lifetimes. The system covers integer and boolean primitives, user structs
//! and enums, references and raw pointers, and generic instantiations.

use std::sync::Arc;

use crate::integer_semantics::IntegerType;
use crate::type_encoding::{self, Composite, Decoded, Primitive};

/// Return the capacity encoded by the canonical synthetic `Str(N)` name.
///
/// Fixed strings are nominal for ABI and identity purposes, so every compiler
/// phase must recognize their generated spelling identically.
pub fn fixed_string_capacity(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("Str(")?.strip_suffix(')')?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let capacity: u64 = digits.parse().ok()?;
    (capacity.to_string() == digits).then_some(capacity)
}

/// Whether `name` is the canonical spelling of a synthetic slice struct.
pub fn is_slice_struct_name(name: &str) -> bool {
    name.starts_with('[') && name.ends_with(']') && !name.contains(';')
}

/// Whether `name` is a two-word string-view nominal (`str` or `Str(N)`).
pub fn is_string_view_struct_name(name: &str) -> bool {
    name == "str" || fixed_string_capacity(name).is_some()
}

/// A unique identifier for a struct definition.
///
/// Values are issued by [`TypeInternPool`](crate::TypeInternPool); their raw
/// storage identity is not part of the public API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StructId(pub(crate) u32);

impl StructId {
    /// Create a StructId from a pool index.
    ///
    /// The pool index is the raw index into `TypeInternPool`'s composite-type
    /// storage.
    #[inline]
    pub(crate) fn from_pool_index(pool_index: u32) -> Self {
        StructId(pool_index)
    }

    /// Get the pool index for this struct.
    ///
    /// This is the index into `TypeInternPool.types`.
    #[inline]
    pub(crate) fn pool_index(self) -> u32 {
        self.0
    }
}

/// A unique identifier for an enum definition.
///
/// Values are issued by [`TypeInternPool`](crate::TypeInternPool); their raw
/// storage identity is not part of the public API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnumId(pub(crate) u32);

impl EnumId {
    /// Create an EnumId from a pool index.
    ///
    /// The pool index is the raw index into `TypeInternPool`'s composite-type
    /// storage.
    #[inline]
    pub(crate) fn from_pool_index(pool_index: u32) -> Self {
        EnumId(pool_index)
    }

    /// Get the pool index for this enum.
    ///
    /// This is the index into `TypeInternPool.types`.
    #[inline]
    pub(crate) fn pool_index(self) -> u32 {
        self.0
    }
}

/// An opaque identifier for an array entry issued by the type pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ArrayTypeId(pub(crate) u32);

impl ArrayTypeId {
    /// Create an ArrayTypeId from a pool index.
    ///
    /// The pool index is the raw index into `TypeInternPool`'s composite-type
    /// storage.
    #[inline]
    pub(crate) fn from_pool_index(pool_index: u32) -> Self {
        ArrayTypeId(pool_index)
    }

    /// Get the pool index for this array type.
    ///
    /// Returns the raw index into the TypeInternPool.
    #[inline]
    pub(crate) fn pool_index(self) -> u32 {
        self.0
    }
}

/// An opaque identifier for a `ptr const T` entry issued by the type pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PtrConstTypeId(pub(crate) u32);

impl PtrConstTypeId {
    /// Create a PtrConstTypeId from a pool index.
    #[inline]
    pub(crate) fn from_pool_index(pool_index: u32) -> Self {
        PtrConstTypeId(pool_index)
    }

    /// Get the pool index for this pointer type.
    #[inline]
    pub(crate) fn pool_index(self) -> u32 {
        self.0
    }
}

/// An opaque identifier for a `ptr mut T` entry issued by the type pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PtrMutTypeId(pub(crate) u32);

impl PtrMutTypeId {
    /// Create a PtrMutTypeId from a pool index.
    #[inline]
    pub(crate) fn from_pool_index(pool_index: u32) -> Self {
        PtrMutTypeId(pool_index)
    }

    /// Get the pool index for this pointer type.
    #[inline]
    pub(crate) fn pool_index(self) -> u32 {
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

    /// Sentinel meaning "a module whose identity is not resolved yet".
    ///
    /// Type inference uses this for `@import(...)` in expression position:
    /// resolving an import to a real `ModuleId` needs the module registry and
    /// file-path resolution, which the constraint generator doesn't have.
    /// Inference only needs module-NESS; sema resolves member calls with the
    /// receiver's real module/file identity and replaces this sentinel during
    /// analysis. The value is the maximum id representable in
    /// `Type`'s 24-bit id field, which the registry's sequential allocation
    /// never reaches in practice.
    pub const UNRESOLVED: ModuleId = ModuleId(0xFF_FFFF);
}

/// The kind of a type - used for pattern matching.
///
/// [`Type`] stores a compact encoded index. `TypeKind` is its decoded,
/// pattern-matchable view; callers obtain it with [`Type::kind`]. Composite
/// variants carry pool-backed identifiers.
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
/// Compact encoded type handle (ADR-0024).
/// This enables O(1) type equality via u32 comparison.
///
/// # Encoding
///
/// The u32 value uses a tag-based encoding:
/// Primitive and composite values use the centralized encoding in
/// `type_encoding`; composite payloads are 24-bit pool or module identifiers.
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
            TypeKind::Struct(id) => write!(f, "Type::new_struct({id:?})"),
            TypeKind::Enum(id) => write!(f, "Type::new_enum({id:?})"),
            TypeKind::Array(id) => write!(f, "Type::new_array({id:?})"),
            TypeKind::PtrConst(id) => write!(f, "Type::new_ptr_const({id:?})"),
            TypeKind::PtrMut(id) => write!(f, "Type::new_ptr_mut({id:?})"),
            TypeKind::Module(id) => write!(f, "Type::new_module(ModuleId({}))", id.0),
        }
    }
}

// Primitive type constants
impl Type {
    pub(crate) const fn raw_encoding(self) -> u32 {
        self.0
    }

    /// 8-bit signed integer
    pub const I8: Type = Type(Primitive::I8.encode());
    /// 16-bit signed integer
    pub const I16: Type = Type(Primitive::I16.encode());
    /// 32-bit signed integer
    pub const I32: Type = Type(Primitive::I32.encode());
    /// 64-bit signed integer
    pub const I64: Type = Type(Primitive::I64.encode());
    /// 8-bit unsigned integer
    pub const U8: Type = Type(Primitive::U8.encode());
    /// 16-bit unsigned integer
    pub const U16: Type = Type(Primitive::U16.encode());
    /// 32-bit unsigned integer
    pub const U32: Type = Type(Primitive::U32.encode());
    /// 64-bit unsigned integer
    pub const U64: Type = Type(Primitive::U64.encode());
    /// Boolean
    pub const BOOL: Type = Type(Primitive::Bool.encode());
    /// The unit type (for functions that don't return a value)
    pub const UNIT: Type = Type(Primitive::Unit.encode());
    /// An error type (used during type checking to continue after errors)
    pub const ERROR: Type = Type(Primitive::Error.encode());
    /// The never type - represents computations that don't return
    pub const NEVER: Type = Type(Primitive::Never.encode());
    /// The comptime type - the type of types themselves
    pub const COMPTIME_TYPE: Type = Type(Primitive::ComptimeType.encode());
}

// Composite type constructors
impl Type {
    #[inline]
    const fn new_composite(kind: Composite, payload: u32) -> Type {
        match type_encoding::encode_composite(kind, payload) {
            Some(raw) => Type(raw),
            None => panic!("type encoding payload exceeds 24 bits"),
        }
    }

    /// Create a struct type from a StructId.
    #[inline]
    pub const fn new_struct(id: StructId) -> Type {
        Self::new_composite(Composite::Struct, id.0)
    }

    /// Create an enum type from an EnumId.
    #[inline]
    pub const fn new_enum(id: EnumId) -> Type {
        Self::new_composite(Composite::Enum, id.0)
    }

    /// Create an array type from an ArrayTypeId.
    #[inline]
    pub const fn new_array(id: ArrayTypeId) -> Type {
        Self::new_composite(Composite::Array, id.0)
    }

    /// Create a raw const pointer type from a PtrConstTypeId.
    #[inline]
    pub const fn new_ptr_const(id: PtrConstTypeId) -> Type {
        Self::new_composite(Composite::PtrConst, id.0)
    }

    /// Create a raw mut pointer type from a PtrMutTypeId.
    #[inline]
    pub const fn new_ptr_mut(id: PtrMutTypeId) -> Type {
        Self::new_composite(Composite::PtrMut, id.0)
    }

    /// Create a module type from a ModuleId.
    #[inline]
    pub const fn new_module(id: ModuleId) -> Type {
        Self::new_composite(Composite::Module, id.0)
    }
}

/// Compiler-recognized identity of a canonical standard-library nominal type.
///
/// This is derived from the nominal's relocation-stable module identity, not
/// its unqualified spelling, so imports and aliases preserve it while an
/// unrelated user declaration with the same name never acquires it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LangItem {
    /// `std.strbuf.StrBuf`.
    StrBuf,
}

impl LangItem {
    /// Classify a nominal only after its module has crossed a trusted
    /// standard-library provenance boundary.
    pub fn from_standard_library_nominal(module_path: &str, name: &str) -> Option<Self> {
        (name == "StrBuf"
            && crate::path_norm::normalize_module_path(module_path) == "\0rue-std/strbuf.rue")
            .then_some(Self::StrBuf)
    }
}

/// Definition of a struct type.
#[derive(Debug, Clone)]
pub struct StructDef {
    /// Struct name.
    ///
    /// Shared rather than owned: declaration metadata reads hand this name to
    /// consumers by refcount instead of copying it out of the pool (RUE-1219).
    pub name: Arc<str>,
    /// Fields in declaration order
    pub fields: Vec<StructField>,
    /// Whether this struct is marked with @copy (can be implicitly duplicated)
    pub is_copy: bool,
    /// Whether this struct is a linear type (must be consumed, cannot be dropped)
    pub is_linear: bool,
    /// Whether this struct was declared `linear` in source, as opposed to
    /// becoming linear only by containing a linear field (infectious
    /// linearity). The containment-facts join may set [`StructDef::is_linear`]
    /// after construction, but it never touches this bit, so it stays the
    /// authoritative record of the source declaration. Anonymous and
    /// compiler-injected nominals cannot be declared linear, so for them this
    /// is always `false`.
    pub declared_linear: bool,
    /// User-defined destructor function name, if any (e.g., "Data.__drop").
    /// Shared for the same reason as [`StructDef::name`].
    pub destructor: Option<Arc<str>>,
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
    /// Get the number of fields in this struct.
    pub fn field_count(&self) -> usize {
        self.fields.len()
    }
}

/// Definition of an enum type.
#[derive(Debug, Clone)]
pub struct EnumDef {
    /// Enum name. Shared for the same reason as [`StructDef::name`].
    pub name: Arc<str>,
    /// Variant names in declaration order.
    ///
    /// Shared as a whole so a declaration-metadata read copies neither the
    /// sequence nor its names (RUE-1219).
    pub variants: Arc<[Arc<str>]>,
    /// Payload field types for each variant, in declaration order (RUE-221,
    /// ADR-0038). Parallel to `variants`: `variant_payloads[i]` is the list of
    /// payload types carried by tuple variant `variants[i]`, or empty for a
    /// discriminant-only variant. An **empty outer vector** means the enum is
    /// entirely discriminant-only (C-like), the common case; use
    /// [`EnumDef::variant_payload`] to read a variant's payload uniformly.
    pub variant_payloads: Vec<Vec<Type>>,
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

    /// Get the payload field types carried by variant `index`.
    ///
    /// Returns an empty slice for a discriminant-only variant (or when this
    /// enum has no payloads at all). Tolerates an empty `variant_payloads`
    /// vector so discriminant-only enums need not populate it.
    pub fn variant_payload(&self, index: usize) -> &[Type] {
        self.variant_payloads
            .get(index)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
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
/// A module records its durable compiler identity and current request's source
/// handle. Member lookup uses that handle to select the defining-file
/// declaration tables and apply visibility; declarations are not duplicated
/// here.
#[derive(Debug, Clone)]
pub struct ModuleDef {
    /// Stable display identity for diagnostics that refer to the module.
    pub import_path: String,
    /// Current request's source path, retained only for presentation.
    pub file_path: String,
    /// Durable compiler module identity for the current semantic epoch.
    pub durable_id: String,
    /// Current request's diagnostic/source handle for this module.
    pub file_id: rue_span::FileId,
}

impl ModuleDef {
    /// Create a new module definition.
    pub fn new(
        import_path: String,
        file_path: String,
        durable_id: String,
        file_id: rue_span::FileId,
    ) -> Self {
        Self {
            import_path,
            file_path,
            durable_id,
            file_id,
        }
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
                 The tag or payload is malformed or reserved.",
                self.0,
                self.0 & type_encoding::TAG_MASK,
                self.0 >> type_encoding::PAYLOAD_SHIFT
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
        match type_encoding::decode(self.0)? {
            Decoded::Primitive(Primitive::I8) => Some(TypeKind::I8),
            Decoded::Primitive(Primitive::I16) => Some(TypeKind::I16),
            Decoded::Primitive(Primitive::I32) => Some(TypeKind::I32),
            Decoded::Primitive(Primitive::I64) => Some(TypeKind::I64),
            Decoded::Primitive(Primitive::U8) => Some(TypeKind::U8),
            Decoded::Primitive(Primitive::U16) => Some(TypeKind::U16),
            Decoded::Primitive(Primitive::U32) => Some(TypeKind::U32),
            Decoded::Primitive(Primitive::U64) => Some(TypeKind::U64),
            Decoded::Primitive(Primitive::Bool) => Some(TypeKind::Bool),
            Decoded::Primitive(Primitive::Unit) => Some(TypeKind::Unit),
            Decoded::Primitive(Primitive::Error) => Some(TypeKind::Error),
            Decoded::Primitive(Primitive::Never) => Some(TypeKind::Never),
            Decoded::Primitive(Primitive::ComptimeType) => Some(TypeKind::ComptimeType),
            Decoded::Composite {
                kind: Composite::Struct,
                payload,
            } => Some(TypeKind::Struct(StructId(payload))),
            Decoded::Composite {
                kind: Composite::Enum,
                payload,
            } => Some(TypeKind::Enum(EnumId(payload))),
            Decoded::Composite {
                kind: Composite::Array,
                payload,
            } => Some(TypeKind::Array(ArrayTypeId(payload))),
            Decoded::Composite {
                kind: Composite::Module,
                payload,
            } => Some(TypeKind::Module(ModuleId(payload))),
            Decoded::Composite {
                kind: Composite::PtrConst,
                payload,
            } => Some(TypeKind::PtrConst(PtrConstTypeId(payload))),
            Decoded::Composite {
                kind: Composite::PtrMut,
                payload,
            } => Some(TypeKind::PtrMut(PtrMutTypeId(payload))),
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

    /// Get a human-readable type name, safely handling anonymous structs and missing definitions.
    ///
    /// Unlike `name()`, this method can access the type pool to get actual struct/enum names
    /// and array shapes (`[i32; 3]`) instead of returning generic placeholders like
    /// `"<struct>"` or `"<array>"`.
    ///
    /// This is primarily used for error messages where we want to show meaningful type names
    /// even if the type pool lookup fails (returns safe fallback in that case).
    ///
    /// # Safety
    ///
    /// This method is safe even if the struct/enum ID is invalid or the pool is None.
    /// It will return a fallback string like `"<struct#123>"` in those cases.
    pub fn safe_name_with_pool(&self, pool: Option<&crate::intern_pool::TypeInternPool>) -> String {
        pool.map(|pool| pool.safe_type_name(*self))
            .unwrap_or_else(|| self.safe_name_without_pool())
    }

    /// Backend-facing counterpart to [`Self::safe_name_with_pool`] for the
    /// immutable type universe produced by semantic analysis.
    pub fn safe_name_with_frozen_pool(
        &self,
        pool: Option<&crate::intern_pool::FrozenTypeInternPool>,
    ) -> String {
        pool.map(|pool| pool.safe_type_name(*self))
            .unwrap_or_else(|| self.safe_name_without_pool())
    }

    fn safe_name_without_pool(&self) -> String {
        match self.try_kind() {
            Some(TypeKind::Struct(id)) => format!("<struct#{}>", id.0),
            Some(TypeKind::Enum(id)) => format!("<enum#{}>", id.0),
            Some(TypeKind::Array(id)) => format!("<array#{}>", id.0),
            Some(TypeKind::PtrConst(id)) => format!("<ptr const#{}>", id.0),
            Some(TypeKind::PtrMut(id)) => format!("<ptr mut#{}>", id.0),
            Some(_) => self.name().to_string(),
            None => format!("<invalid type encoding: {:#x}>", self.0),
        }
    }

    /// Resolve a primitive type name to its `Type`.
    ///
    /// This is the **single source of truth** for the primitive-name table.
    /// Every type-name resolution path (signature resolution, let-annotation
    /// validation, HM inference, comptime/const evaluation) must consult this
    /// function instead of keeping its own `match` — there used to be seven
    /// duplicated copies, which is how `usize`/`isize` ended up accepted in
    /// some positions and rejected in others (RUE-151, RUE-155).
    ///
    /// Returns `None` for non-primitive names (struct/enum names, array and
    /// pointer syntax), which callers resolve against their own tables.
    #[must_use]
    pub fn from_primitive_name(name: &str) -> Option<Type> {
        Some(match name {
            "i8" => Type::I8,
            "i16" => Type::I16,
            "i32" => Type::I32,
            "i64" => Type::I64,
            "u8" => Type::U8,
            "u16" => Type::U16,
            "u32" => Type::U32,
            "u64" => Type::U64,
            // Pointer-width integers. All supported targets are 64-bit, so
            // these resolve to the 64-bit types (RUE-151).
            "usize" => Type::U64,
            "isize" => Type::I64,
            "bool" => Type::BOOL,
            "()" => Type::UNIT,
            "!" => Type::NEVER,
            // The type of types - used for comptime type parameters
            "type" => Type::COMPTIME_TYPE,
            _ => return None,
        })
    }

    /// Check if this type is an integer type.
    /// Optimized: checks tag range directly (0-7 are integer types).
    #[inline]
    pub fn is_integer(&self) -> bool {
        matches!(
            type_encoding::decode(self.0),
            Some(Decoded::Primitive(
                Primitive::I8
                    | Primitive::I16
                    | Primitive::I32
                    | Primitive::I64
                    | Primitive::U8
                    | Primitive::U16
                    | Primitive::U32
                    | Primitive::U64
            ))
        )
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
        type_encoding::has_composite_kind(self.0, Composite::Struct)
    }

    /// Get the struct ID if this is a struct type.
    #[inline]
    pub fn as_struct(&self) -> Option<StructId> {
        if self.is_struct() {
            Some(StructId(self.0 >> type_encoding::PAYLOAD_SHIFT))
        } else {
            None
        }
    }

    /// Check if this is an array type.
    #[inline]
    pub fn is_array(&self) -> bool {
        type_encoding::has_composite_kind(self.0, Composite::Array)
    }

    /// Get the array type ID if this is an array type.
    #[inline]
    pub fn as_array(&self) -> Option<ArrayTypeId> {
        if self.is_array() {
            Some(ArrayTypeId(self.0 >> type_encoding::PAYLOAD_SHIFT))
        } else {
            None
        }
    }

    /// Check if this is an enum type.
    #[inline]
    pub fn is_enum(&self) -> bool {
        type_encoding::has_composite_kind(self.0, Composite::Enum)
    }

    /// Get the enum ID if this is an enum type.
    #[inline]
    pub fn as_enum(&self) -> Option<EnumId> {
        if self.is_enum() {
            Some(EnumId(self.0 >> type_encoding::PAYLOAD_SHIFT))
        } else {
            None
        }
    }

    /// Check if this is a module type.
    #[inline]
    pub fn is_module(&self) -> bool {
        type_encoding::has_composite_kind(self.0, Composite::Module)
    }

    /// Get the module ID if this is a module type.
    #[inline]
    pub fn as_module(&self) -> Option<ModuleId> {
        if self.is_module() {
            Some(ModuleId(self.0 >> type_encoding::PAYLOAD_SHIFT))
        } else {
            None
        }
    }

    /// Check if this is a raw const pointer type.
    #[inline]
    pub fn is_ptr_const(&self) -> bool {
        type_encoding::has_composite_kind(self.0, Composite::PtrConst)
    }

    /// Get the pointer type ID if this is a ptr const type.
    #[inline]
    pub fn as_ptr_const(&self) -> Option<PtrConstTypeId> {
        if self.is_ptr_const() {
            Some(PtrConstTypeId(self.0 >> type_encoding::PAYLOAD_SHIFT))
        } else {
            None
        }
    }

    /// Check if this is a raw mut pointer type.
    #[inline]
    pub fn is_ptr_mut(&self) -> bool {
        type_encoding::has_composite_kind(self.0, Composite::PtrMut)
    }

    /// Get the pointer type ID if this is a ptr mut type.
    #[inline]
    pub fn as_ptr_mut(&self) -> Option<PtrMutTypeId> {
        if self.is_ptr_mut() {
            Some(PtrMutTypeId(self.0 >> type_encoding::PAYLOAD_SHIFT))
        } else {
            None
        }
    }

    /// Check if this is any raw pointer type (ptr const or ptr mut).
    #[inline]
    pub fn is_ptr(&self) -> bool {
        self.is_ptr_const() || self.is_ptr_mut()
    }

    /// Check if this is a signed integer type.
    /// Optimized: checks tag range directly (0-3 are signed integers).
    #[inline]
    pub fn is_signed(&self) -> bool {
        matches!(
            type_encoding::decode(self.0),
            Some(Decoded::Primitive(
                Primitive::I8 | Primitive::I16 | Primitive::I32 | Primitive::I64
            ))
        )
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
    /// - Array types (unless element type is Copy, checked by the body host)
    ///
    /// Note: This method can't check struct's is_copy attribute or array element
    /// types since it doesn't have access to StructDefs or array type information.
    /// Use the body host's `is_type_copy` for full checking.
    pub fn is_copy(&self) -> bool {
        match type_encoding::decode(self.0) {
            Some(Decoded::Primitive(_)) => true,
            Some(Decoded::Composite {
                kind: Composite::Enum | Composite::Module,
                ..
            }) => true,
            Some(Decoded::Composite { .. }) | None => false,
        }
    }

    /// Check if this type is Copy, with access to TypeInternPool for struct checking.
    ///
    /// This is used during anonymous struct creation to determine if the new struct
    /// should be Copy based on its field types.
    pub fn is_copy_in_pool(&self, type_pool: &crate::intern_pool::TypeInternPool) -> bool {
        type_pool.is_copy_type(*self)
    }

    pub fn is_copy_in_frozen_pool(
        &self,
        type_pool: &crate::intern_pool::FrozenTypeInternPool,
    ) -> bool {
        type_pool.is_copy_type(*self)
    }

    /// Check if this is a 64-bit type (uses 64-bit operations).
    /// Optimized: checks for I64 (3) or U64 (7).
    #[inline]
    pub fn is_64_bit(&self) -> bool {
        matches!(*self, Type::I64 | Type::U64)
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
        matches!(
            type_encoding::decode(self.0),
            Some(Decoded::Primitive(
                Primitive::U8 | Primitive::U16 | Primitive::U32 | Primitive::U64
            ))
        )
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
        self.int_max().is_some_and(|max| i128::from(value) <= max)
    }

    /// Get the bit width of this integer type (8, 16, 32, or 64).
    ///
    /// Returns `None` for non-integer types.
    #[must_use]
    pub fn int_bit_width(&self) -> Option<u32> {
        self.integer_semantics().map(IntegerType::bits)
    }

    /// Return the width and signedness descriptor used by all integer
    /// semantics consumers.
    #[must_use]
    pub fn integer_semantics(&self) -> Option<IntegerType> {
        let bits = match self.try_kind()? {
            TypeKind::I8 | TypeKind::U8 => 8,
            TypeKind::I16 | TypeKind::U16 => 16,
            TypeKind::I32 | TypeKind::U32 => 32,
            TypeKind::I64 | TypeKind::U64 => 64,
            _ => return None,
        };
        IntegerType::new(bits, self.is_signed())
    }

    /// Get the minimum representable value of this integer type.
    ///
    /// Returns `None` for non-integer types.
    #[must_use]
    pub fn int_min(&self) -> Option<i128> {
        self.integer_semantics().map(IntegerType::min_i128)
    }

    /// Get the maximum representable value of this integer type.
    ///
    /// Returns `None` for non-integer types.
    #[must_use]
    pub fn int_max(&self) -> Option<i128> {
        self.integer_semantics().map(IntegerType::max_i128)
    }

    /// Check if a u64 value can be negated to fit within the range of this signed integer type.
    ///
    /// This is used to allow literals like `2147483648` when negated to `-2147483648` (i32::MIN).
    /// Returns `true` if the negated value fits, `false` otherwise.
    #[must_use]
    pub fn negated_literal_fits(&self, value: u64) -> bool {
        self.integer_semantics().is_some_and(|integer| {
            integer
                .checked_neg_literal_i128(i128::from(value))
                .is_some()
        })
    }

    /// Encode this type as a u32 for storage in extra arrays.
    ///
    /// This is crate-private because packed AIR storage is an invariant-proven
    /// implementation boundary.
    #[inline]
    pub(crate) fn as_u32(&self) -> u32 {
        self.0
    }

    /// Decode a type from a u32 value.
    ///
    /// This does not validate the encoding and is only for values previously
    /// produced by `as_u32()` inside AIR.
    ///
    /// # Safety (not unsafe, but correctness)
    ///
    /// This method trusts that the input is a valid encoding. For untrusted data,
    /// use [`try_from_u32`](Self::try_from_u32) which validates the encoding.
    #[inline]
    #[cfg(test)]
    pub(crate) fn from_u32(v: u32) -> Self {
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
        type_encoding::decode(v).is_some()
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

/// The length component of an array type syntax `[T; N]`.
///
/// The length is either a literal (`[i32; 4]`) or a name referring to a
/// file-level `const` or a `comptime` value parameter (`[i32; N]`). Named
/// lengths are resolved to a concrete value during sema using the const
/// evaluator / comptime substitution machinery (RUE-16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArrayLen {
    /// A literal length parsed directly from the type name (`4`).
    Literal(u64),
    /// A name that must be resolved to a compile-time constant (`N`).
    Named(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_string_and_slice_names_have_one_canonical_classifier() {
        assert_eq!(fixed_string_capacity("Str(0)"), Some(0));
        assert_eq!(fixed_string_capacity("Str(42)"), Some(42));
        for invalid in [
            "str", "Str()", "Str(-1)", "Str(+1)", "Str(01)", "Str(1", "Str(1)x",
        ] {
            assert_eq!(fixed_string_capacity(invalid), None, "{invalid}");
        }

        assert!(is_slice_struct_name("[u8]"));
        assert!(!is_slice_struct_name("[u8; 4]"));
        assert!(!is_slice_struct_name("u8"));
        assert!(is_string_view_struct_name("str"));
        assert!(is_string_view_struct_name("Str(42)"));
        assert!(!is_string_view_struct_name("Str(042)"));
    }

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

    // ========== Type::from_primitive_name() tests ==========

    #[test]
    fn test_from_primitive_name_all_primitives() {
        assert_eq!(Type::from_primitive_name("i8"), Some(Type::I8));
        assert_eq!(Type::from_primitive_name("i16"), Some(Type::I16));
        assert_eq!(Type::from_primitive_name("i32"), Some(Type::I32));
        assert_eq!(Type::from_primitive_name("i64"), Some(Type::I64));
        assert_eq!(Type::from_primitive_name("u8"), Some(Type::U8));
        assert_eq!(Type::from_primitive_name("u16"), Some(Type::U16));
        assert_eq!(Type::from_primitive_name("u32"), Some(Type::U32));
        assert_eq!(Type::from_primitive_name("u64"), Some(Type::U64));
        // Pointer-width names alias the 64-bit types (RUE-151).
        assert_eq!(Type::from_primitive_name("usize"), Some(Type::U64));
        assert_eq!(Type::from_primitive_name("isize"), Some(Type::I64));
        assert_eq!(Type::from_primitive_name("bool"), Some(Type::BOOL));
        assert_eq!(Type::from_primitive_name("()"), Some(Type::UNIT));
        assert_eq!(Type::from_primitive_name("!"), Some(Type::NEVER));
        assert_eq!(Type::from_primitive_name("type"), Some(Type::COMPTIME_TYPE));
    }

    #[test]
    fn test_from_primitive_name_non_primitives() {
        // Struct/enum names, array and pointer syntax are resolved by callers.
        assert_eq!(Type::from_primitive_name("String"), None);
        assert_eq!(Type::from_primitive_name("zzz_bogus"), None);
        assert_eq!(Type::from_primitive_name("[i32; 3]"), None);
        assert_eq!(Type::from_primitive_name("ptr const i32"), None);
        assert_eq!(Type::from_primitive_name(""), None);
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
            name: "Point".into(),
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
            is_linear: false,
            declared_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };

        let def = crate::intern_pool::StructDefEntry::new(def);
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
            name: "Empty".into(),
            fields: vec![],
            is_copy: false,
            is_linear: false,
            declared_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        assert_eq!(empty.field_count(), 0);

        let with_fields = StructDef {
            name: "Data".into(),
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
            is_linear: false,
            declared_linear: false,
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
            name: "Empty".into(),
            variants: Arc::from([]),
            variant_payloads: Vec::new(),
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        assert_eq!(empty.variant_count(), 0);

        let color = EnumDef {
            name: "Color".into(),
            variants: Arc::from(["Red".into(), "Green".into(), "Blue".into()]),
            variant_payloads: Vec::new(),
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        assert_eq!(color.variant_count(), 3);
    }

    #[test]
    fn test_enum_def_find_variant() {
        let color = EnumDef {
            name: "Color".into(),
            variants: Arc::from(["Red".into(), "Green".into(), "Blue".into()]),
            variant_payloads: Vec::new(),
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };

        let color = crate::intern_pool::EnumDefEntry::new(color);
        assert_eq!(color.find_variant("Red"), Some(0));
        assert_eq!(color.find_variant("Green"), Some(1));
        assert_eq!(color.find_variant("Blue"), Some(2));
        assert_eq!(color.find_variant("Yellow"), None);
    }

    #[test]
    fn test_enum_def_discriminant_type_empty() {
        let empty = EnumDef {
            name: "Empty".into(),
            variants: Arc::from([]),
            variant_payloads: Vec::new(),
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        assert_eq!(empty.discriminant_type(), Type::NEVER);
    }

    #[test]
    fn test_enum_def_discriminant_type_small() {
        // 1-256 variants -> U8
        let small = EnumDef {
            name: "Small".into(),
            variants: Arc::from(["A".into()]),
            variant_payloads: Vec::new(),
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        assert_eq!(small.discriminant_type(), Type::U8);

        let max_u8 = EnumDef {
            name: "MaxU8".into(),
            variants: (0..256)
                .map(|i| Arc::from(format!("V{}", i).as_str()))
                .collect(),
            variant_payloads: Vec::new(),
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        assert_eq!(max_u8.discriminant_type(), Type::U8);
    }

    #[test]
    fn test_enum_def_discriminant_type_medium() {
        // 257-65536 variants -> U16
        let medium = EnumDef {
            name: "Medium".into(),
            variants: (0..257)
                .map(|i| Arc::from(format!("V{}", i).as_str()))
                .collect(),
            variant_payloads: Vec::new(),
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
        for tag in type_encoding::RESERVED_AFTER_PRIMITIVES_START
            ..=type_encoding::RESERVED_AFTER_PRIMITIVES_END
        {
            assert!(
                !Type::is_valid_encoding(tag),
                "tag {} should be invalid",
                tag
            );
        }

        // Tags above composites are invalid (106+)
        for tag in type_encoding::RESERVED_AFTER_COMPOSITES_START
            ..=type_encoding::RESERVED_AFTER_COMPOSITES_END
        {
            assert!(
                !Type::is_valid_encoding(tag),
                "tag {} should be invalid",
                tag
            );
        }

        // Primitive tags cannot carry a composite payload. This used to
        // silently decode as I8 because only the low byte was inspected.
        assert!(!Type::is_valid_encoding(1 << type_encoding::PAYLOAD_SHIFT));
        assert!(!Type::is_valid_encoding(
            (17 << type_encoding::PAYLOAD_SHIFT) | Primitive::Never.encode()
        ));
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
        assert!(Type::try_from_u32(1 << type_encoding::PAYLOAD_SHIFT).is_none());
    }

    #[test]
    #[should_panic(expected = "type encoding payload exceeds 24 bits")]
    fn composite_constructor_rejects_payload_overflow() {
        let _ = Type::new_struct(StructId(type_encoding::MAX_PAYLOAD + 1));
    }

    #[test]
    fn composite_constructor_accepts_maximum_payload() {
        let ty = Type::new_module(ModuleId(type_encoding::MAX_PAYLOAD));
        assert_eq!(
            ty.try_kind(),
            Some(TypeKind::Module(ModuleId(type_encoding::MAX_PAYLOAD)))
        );
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

    #[test]
    fn int_bit_width_min_max() {
        assert_eq!(Type::I8.int_bit_width(), Some(8));
        assert_eq!(Type::U16.int_bit_width(), Some(16));
        assert_eq!(Type::I32.int_bit_width(), Some(32));
        assert_eq!(Type::U64.int_bit_width(), Some(64));
        assert_eq!(Type::BOOL.int_bit_width(), None);

        assert_eq!(Type::I8.int_min(), Some(-128));
        assert_eq!(Type::I8.int_max(), Some(127));
        assert_eq!(Type::U8.int_min(), Some(0));
        assert_eq!(Type::U8.int_max(), Some(255));
        assert_eq!(Type::I64.int_min(), Some(i64::MIN as i128));
        assert_eq!(Type::I64.int_max(), Some(i64::MAX as i128));
        assert_eq!(Type::U64.int_min(), Some(0));
        assert_eq!(Type::U64.int_max(), Some(u64::MAX as i128));
        assert_eq!(Type::UNIT.int_min(), None);
        assert_eq!(Type::UNIT.int_max(), None);
    }

    #[derive(Debug, PartialEq, Eq)]
    enum StructuredType {
        Named(String),
        Qualified(Vec<String>),
        Unit,
        Never,
        Array(Box<StructuredType>, Box<StructuredType>),
        Slice(Box<StructuredType>),
        PointerConst(Box<StructuredType>),
        PointerMut(Box<StructuredType>),
        TypeCall(Vec<String>, Vec<StructuredType>),
        ValueCall(String, Vec<StructuredType>),
        Integer(i128),
    }

    /// The parser-structured type RIR carries for `f`'s single parameter.
    ///
    /// Tests intentionally keep this in its structured form. Rendering it and
    /// feeding the resulting text back through the semantic grammar would
    /// recreate the production peer path this artifact replaces.
    fn declared_parameter_type(
        annotation: &str,
    ) -> (
        rue_rir::RirTypeSyntaxArena<lasso::Spur>,
        rue_rir::RirTypeSyntaxRef,
        lasso::ThreadedRodeo,
    ) {
        let source = format!("fn f(p: {annotation}) -> i32 {{ 0 }}");
        let (tokens, interner) = rue_lexer::Lexer::new(&source).tokenize().unwrap();
        let (ast, interner) = rue_parser::Parser::new(tokens, interner).parse().unwrap();
        let mut astgen = rue_rir::AstGen::with_symbol_normalizer(&interner, |symbol| symbol);
        astgen.append_items(&ast.items);
        let rir = astgen.finish();
        let params = rir
            .iter()
            .find_map(|(_, instruction)| match &instruction.data {
                rue_rir::InstData::FnDecl { params, .. } => Some(params.clone()),
                _ => None,
            })
            .expect("lowered function declaration");
        let params = rir.params(&params);
        assert_eq!(params.len(), 1, "one declared parameter for {annotation}");
        (
            rir.type_syntax().clone(),
            params.get(0).unwrap().ty,
            interner,
        )
    }

    fn symbols(
        arena: &rue_rir::RirTypeSyntaxArena<lasso::Spur>,
        interner: &lasso::ThreadedRodeo,
        range: rue_rir::RirTypeSyntaxRange,
    ) -> Vec<String> {
        arena
            .words(range)
            .expect("valid symbol range")
            .iter()
            .map(|word| {
                interner
                    .resolve(
                        arena
                            .symbol(rue_rir::RirTypeSyntaxSymbol::from_u32(*word))
                            .expect("valid symbol ordinal"),
                    )
                    .to_owned()
            })
            .collect()
    }

    fn structured_type(
        arena: &rue_rir::RirTypeSyntaxArena<lasso::Spur>,
        interner: &lasso::ThreadedRodeo,
        reference: rue_rir::RirTypeSyntaxRef,
    ) -> StructuredType {
        use rue_rir::RirTypeSyntaxNode as Node;

        let child = |reference| structured_type(arena, interner, reference);
        match arena
            .node(reference)
            .expect("valid structured type reference")
        {
            Node::Named(symbol) => StructuredType::Named(
                interner
                    .resolve(arena.symbol(*symbol).expect("valid named symbol"))
                    .to_owned(),
            ),
            Node::Qualified { path } => StructuredType::Qualified(symbols(arena, interner, *path)),
            Node::Unit => StructuredType::Unit,
            Node::Never => StructuredType::Never,
            Node::Array { element, length } => {
                StructuredType::Array(Box::new(child(*element)), Box::new(child(*length)))
            }
            Node::Slice { element } => StructuredType::Slice(Box::new(child(*element))),
            Node::PointerConst { pointee } => {
                StructuredType::PointerConst(Box::new(child(*pointee)))
            }
            Node::PointerMut { pointee } => StructuredType::PointerMut(Box::new(child(*pointee))),
            Node::TypeCall { path, arguments } => StructuredType::TypeCall(
                symbols(arena, interner, *path),
                arena
                    .words(*arguments)
                    .expect("valid type-call arguments")
                    .iter()
                    .map(|word| child(rue_rir::RirTypeSyntaxRef::from_u32(*word)))
                    .collect(),
            ),
            Node::ValueCall { name, arguments } => StructuredType::ValueCall(
                interner
                    .resolve(arena.symbol(*name).expect("valid value-call name"))
                    .to_owned(),
                arena
                    .words(*arguments)
                    .expect("valid value-call arguments")
                    .iter()
                    .map(|word| child(rue_rir::RirTypeSyntaxRef::from_u32(*word)))
                    .collect(),
            ),
            Node::Integer(value) => StructuredType::Integer(*value),
            Node::AnonymousStruct { .. } | Node::AnonymousEnum { .. } => {
                panic!("anonymous types are not accepted in declared annotation position")
            }
        }
    }

    fn named(name: &str) -> StructuredType {
        StructuredType::Named(name.to_owned())
    }

    /// Parser `TypeExpr` structure is retained exactly in the declaration RIR
    /// and consumed directly by semantic analysis. Every declarable shape is
    /// pinned here without rendering or reparsing a second type grammar.
    #[test]
    fn declared_type_syntax_stays_structured_through_rir_intake() {
        use StructuredType as T;

        let cases = [
            ("i32", named("i32")),
            ("MyType", named("MyType")),
            (
                "shapes.Point",
                T::Qualified(vec!["shapes".to_owned(), "Point".to_owned()]),
            ),
            ("()", T::Unit),
            ("!", T::Never),
            (
                "[i32; 4]",
                T::Array(Box::new(named("i32")), Box::new(T::Integer(4))),
            ),
            (
                "[i32; N]",
                T::Array(Box::new(named("i32")), Box::new(named("N"))),
            ),
            (
                "[i32; fact(4)]",
                T::Array(
                    Box::new(named("i32")),
                    Box::new(T::ValueCall("fact".to_owned(), vec![T::Integer(4)])),
                ),
            ),
            (
                "[[u8; 2]; 3]",
                T::Array(
                    Box::new(T::Array(Box::new(named("u8")), Box::new(T::Integer(2)))),
                    Box::new(T::Integer(3)),
                ),
            ),
            ("[i32]", T::Slice(Box::new(named("i32")))),
            (
                "ptr const [i32; 4]",
                T::PointerConst(Box::new(T::Array(
                    Box::new(named("i32")),
                    Box::new(T::Integer(4)),
                ))),
            ),
            (
                "[ptr mut u8; 2]",
                T::Array(
                    Box::new(T::PointerMut(Box::new(named("u8")))),
                    Box::new(T::Integer(2)),
                ),
            ),
            (
                "Result(Option(i32), bool)",
                T::TypeCall(
                    vec!["Result".to_owned()],
                    vec![
                        T::TypeCall(vec!["Option".to_owned()], vec![named("i32")]),
                        named("bool"),
                    ],
                ),
            ),
            (
                "Vector(i32, 3)",
                T::TypeCall(vec!["Vector".to_owned()], vec![named("i32"), T::Integer(3)]),
            ),
            (
                "shapes.Pair(i32)",
                T::TypeCall(
                    vec!["shapes".to_owned(), "Pair".to_owned()],
                    vec![named("i32")],
                ),
            ),
            (
                "[Str(8); 2]",
                T::Array(
                    Box::new(T::TypeCall(vec!["Str".to_owned()], vec![T::Integer(8)])),
                    Box::new(T::Integer(2)),
                ),
            ),
        ];

        for (annotation, expected) in cases {
            let (arena, root, interner) = declared_parameter_type(annotation);
            for (owner, _) in arena.nodes().iter().enumerate() {
                assert!(arena.visit_child_references(
                    rue_rir::RirTypeSyntaxRef::from_u32(owner as u32),
                    |child| assert!(child.index() < owner, "postorder child for `{annotation}`"),
                ));
            }
            assert_eq!(
                structured_type(&arena, &interner, root),
                expected,
                "structured declaration type for `{annotation}`"
            );
        }
    }
}
