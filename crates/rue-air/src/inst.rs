//! AIR instruction definitions.
//!
//! Like RIR, instructions are stored densely and referenced by index.
//!
//! # Place Expressions
//!
//! Memory locations are represented using [`AirPlace`], which consists of:
//! - A base ([`AirPlaceBase`]): either a local variable slot or parameter slot
//! - A list of projections ([`AirProjection`]): field accesses and array indices
//!
//! This design follows Rust MIR's proven approach and eliminates redundant
//! Load instructions for nested access patterns like `arr[i].field`.

use std::fmt;
use std::marker::PhantomData;

// Compile-time size assertions to prevent silent size growth during refactoring.
// These limits are set slightly above current sizes to allow minor changes,
// but will catch significant size regressions.
//
// Current sizes (as of 2025-12):
// - AirInst: 40 bytes (AirInstData + Type + Span)
// - AirInstData: 24 bytes
const _: () = assert!(std::mem::size_of::<AirInst>() <= 48);
const _: () = assert!(std::mem::size_of::<AirInstData>() <= 32);

use crate::types::{StructId, Type};
use crate::{FrozenTypeInternPool, TypeInternPool};
use lasso::{Key, Spur, ThreadedRodeo};
use rue_span::Span;

#[cfg(any(test, feature = "fuzz-support"))]
mod payload_support;

/// The published ceiling on AIR instructions in **one function body**
/// (spec Appendix C.6:1).
///
/// Unlike the RIR instruction ceiling, which counts one shared per-program
/// array, every function body owns a private AIR instruction array addressed by
/// its own [`AirRef`] — a `u32`. The array's length is narrowed to `u32` at the
/// payload-staging boundary (`Air::reserve_instruction`), so a body holds at
/// most this many instructions and the last `u32` index is left unused rather
/// than making the count itself unrepresentable.
///
/// Exceeding the ceiling is a diagnosable compile-time failure (spec C.1:2),
/// surfaced as `E1401` at the semantic AIR boundary ([`Air::finish`]) — never a
/// truncated `AirRef` that aliases instruction 0 onto instruction 2^32.
pub const MAX_AIR_INSTRUCTIONS_PER_BODY: u32 = u32::MAX;

/// Structured failure returned by checked AIR payload decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AirPayloadError {
    pub phase: &'static str,
    pub family: &'static str,
    pub range_start: u32,
    pub range_extent: u32,
    pub record: usize,
    pub expected_width: usize,
    pub actual_width: usize,
    pub reason: &'static str,
}

impl AirPayloadError {
    fn decode(
        family: &'static str,
        range_start: u32,
        range_extent: u32,
        record: usize,
        expected_width: usize,
        actual_width: usize,
        reason: &'static str,
    ) -> Self {
        Self {
            phase: "AIR payload decode",
            family,
            range_start,
            range_extent,
            record,
            expected_width,
            actual_width,
            reason,
        }
    }

    pub fn expected_width(&self) -> usize {
        self.expected_width
    }

    pub fn actual_width(&self) -> usize {
        self.actual_width
    }
}

impl fmt::Display for AirPayloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: corrupt {} range start={} extent={} at record {}: expected width={}, actual width={}: {}",
            self.phase,
            self.family,
            self.range_start,
            self.range_extent,
            self.record,
            self.expected_width(),
            self.actual_width(),
            self.reason
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AirBuildErrorKind {
    ProducerInvariant(&'static str),
    ResourceLimit,
    ResourceExhaustion,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AirBuildError {
    pub phase: &'static str,
    pub family: &'static str,
    pub operation: &'static str,
    pub kind: AirBuildErrorKind,
}

impl fmt::Display for AirBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} {} failed: {:?}",
            self.phase, self.family, self.operation, self.kind
        )
    }
}

impl std::error::Error for AirBuildError {}

impl From<AirBuildError> for rue_error::CompileError {
    fn from(error: AirBuildError) -> Self {
        let kind = match error.kind {
            AirBuildErrorKind::ProducerInvariant(_) => {
                rue_error::ErrorKind::CompilerProducerInvariant(error.to_string())
            }
            AirBuildErrorKind::ResourceLimit => {
                rue_error::ErrorKind::CompilerResourceLimit(error.to_string())
            }
            AirBuildErrorKind::ResourceExhaustion => {
                rue_error::ErrorKind::CompilerResourceExhaustion(error.to_string())
            }
        };
        rue_error::CompileError::without_span(kind)
    }
}

/// Why an AIR owner was refused at its publication boundary.
///
/// The boundary reports two unrelated things. A structural inconsistency is a
/// producer bug and stays an internal compiler error; running past a published
/// implementation limit is a diagnosable compile-time failure that must name
/// the limit (spec C.1:2), so it needs its own typed channel out of
/// [`Air::finish`] rather than collapsing into `E9000`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AirValidationErrorKind {
    /// The owner does not satisfy AIR's structural invariants (`E9000`).
    Structural,
    /// Construction ran past a published implementation limit
    /// (spec Appendix C.6:1). Reported as `E1401` naming the limit.
    ResourceLimit,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AirValidationError {
    pub instruction: Option<usize>,
    pub reason: String,
    pub kind: AirValidationErrorKind,
}

impl AirValidationError {
    /// Whether this rejection is an implementation-limit failure (spec C.1:2)
    /// rather than a producer bug. Consumers use it to pick `E1401` over an
    /// internal-error code.
    pub fn is_resource_limit(&self) -> bool {
        matches!(self.kind, AirValidationErrorKind::ResourceLimit)
    }
}

impl fmt::Display for AirValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A limit rejection already reads as a user-facing sentence naming the
        // exceeded ceiling; the "invalid AIR" prefix belongs only to the
        // structural (compiler-bug) channel.
        match (self.kind, self.instruction) {
            (AirValidationErrorKind::ResourceLimit, _) => f.write_str(&self.reason),
            (AirValidationErrorKind::Structural, Some(index)) => {
                write!(f, "invalid AIR instruction {index}: {}", self.reason)
            }
            (AirValidationErrorKind::Structural, None) => {
                write!(f, "invalid AIR: {}", self.reason)
            }
        }
    }
}

impl std::error::Error for AirValidationError {}

impl From<AirValidationError> for rue_error::CompileError {
    fn from(error: AirValidationError) -> Self {
        let kind = match error.kind {
            AirValidationErrorKind::ResourceLimit => {
                rue_error::ErrorKind::CompilerResourceLimit(error.to_string())
            }
            AirValidationErrorKind::Structural => {
                rue_error::ErrorKind::InternalError(error.to_string())
            }
        };
        rue_error::CompileError::without_span(kind)
    }
}

pub enum AirValidationContext<'a> {
    Semantic(&'a TypeInternPool),
    SemanticWithSymbols(&'a TypeInternPool, &'a ThreadedRodeo),
    Canonical(&'a FrozenTypeInternPool),
    CanonicalWithSymbols(&'a FrozenTypeInternPool, &'a ThreadedRodeo),
}

impl AirValidationContext<'_> {
    fn validate_type(&self, ty: Type) -> Result<(), String> {
        match self {
            Self::Semantic(pool) | Self::SemanticWithSymbols(pool, _) => {
                if matches!(ty.try_kind(), Some(crate::TypeKind::Error)) {
                    return Ok(());
                }
                pool.validate_complete_type(ty)
            }
            Self::Canonical(pool) | Self::CanonicalWithSymbols(pool, _) => {
                pool.validate_complete_type(ty)
            }
        }
        .map_err(|error| format!("{error:?}"))
    }

    fn struct_field_type(&self, id: StructId, field: u32) -> Result<Type, String> {
        self.validate_type(Type::new_struct(id))?;
        let field_type = match self {
            Self::Semantic(pool) | Self::SemanticWithSymbols(pool, _) => pool
                .struct_def(id)
                .fields
                .get(field as usize)
                .map(|field| field.ty),
            Self::Canonical(pool) | Self::CanonicalWithSymbols(pool, _) => pool
                .struct_def(id)
                .fields
                .get(field as usize)
                .map(|field| field.ty),
        };
        field_type.ok_or_else(|| format!("struct field index {field} is out of bounds"))
    }

    fn array_element_type(&self, ty: Type) -> Result<Type, String> {
        let id = ty
            .as_array()
            .ok_or_else(|| format!("projection base {} is not an array", ty.name()))?;
        self.validate_type(ty)?;
        Ok(match self {
            Self::Semantic(pool) | Self::SemanticWithSymbols(pool, _) => pool.array_def(id).0,
            Self::Canonical(pool) | Self::CanonicalWithSymbols(pool, _) => pool.array_def(id).0,
        })
    }

    fn enum_payload_type(
        &self,
        id: crate::EnumId,
        variant: u32,
        field: u32,
    ) -> Result<Type, String> {
        let variant_count = match self {
            Self::Semantic(pool) | Self::SemanticWithSymbols(pool, _) => pool
                .enum_variant_count(id)
                .ok_or_else(|| format!("enum identity {:?} is unavailable", id))?,
            Self::Canonical(pool) | Self::CanonicalWithSymbols(pool, _) => {
                pool.enum_def(id).variant_count()
            }
        };
        if variant as usize >= variant_count {
            return Err(format!("enum variant index {variant} is out of bounds"));
        }
        let ty = match self {
            Self::Semantic(pool) | Self::SemanticWithSymbols(pool, _) => {
                pool.enum_variant_payload_type(id, variant as usize, field as usize)
            }
            Self::Canonical(pool) | Self::CanonicalWithSymbols(pool, _) => pool
                .enum_def(id)
                .variant_payload(variant as usize)
                .get(field as usize)
                .copied(),
        };
        ty.ok_or_else(|| format!("enum variant {variant} payload field {field} is out of bounds"))
    }

    fn enum_payload_len(&self, id: crate::EnumId, variant: u32) -> Result<usize, String> {
        match self {
            Self::Semantic(pool) | Self::SemanticWithSymbols(pool, _) => pool
                .enum_variant_payload_len(id, variant as usize)
                .ok_or_else(|| format!("enum variant index {variant} is out of bounds")),
            Self::Canonical(pool) | Self::CanonicalWithSymbols(pool, _) => {
                let def = pool.enum_def(id);
                if variant as usize >= def.variant_count() {
                    Err(format!("enum variant index {variant} is out of bounds"))
                } else {
                    Ok(def.variant_payload(variant as usize).len())
                }
            }
        }
    }

    fn validate_enum_variant(&self, id: crate::EnumId, variant: u32) -> Result<(), String> {
        let count = match self {
            Self::Semantic(pool) | Self::SemanticWithSymbols(pool, _) => pool
                .enum_variant_count(id)
                .ok_or_else(|| format!("enum identity {:?} is unavailable", id))?,
            Self::Canonical(pool) | Self::CanonicalWithSymbols(pool, _) => {
                pool.enum_def(id).variant_count()
            }
        };
        if variant as usize >= count {
            return Err(format!("enum variant index {variant} is out of bounds"));
        }
        Ok(())
    }

    fn validate_symbol(&self, symbol: Spur) -> Result<(), String> {
        let interner = match self {
            Self::SemanticWithSymbols(_, interner) | Self::CanonicalWithSymbols(_, interner) => {
                Some(*interner)
            }
            Self::Semantic(_) | Self::Canonical(_) => None,
        };
        if interner.is_some_and(|interner| interner.try_resolve(&symbol).is_none()) {
            Err(format!(
                "symbol {} is outside the interner",
                symbol.into_usize()
            ))
        } else {
            Ok(())
        }
    }
}

// ============================================================================
// Place Expressions
// ============================================================================

/// A reference to a place in AIR - stored as index into the places array.
///
/// This is a lightweight handle that can be copied and compared efficiently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AirPlaceRef(u32);

impl AirPlaceRef {
    /// Create a new place reference from a raw index.
    #[inline]
    pub const fn from_raw(index: u32) -> Self {
        Self(index)
    }

    /// Get the raw index.
    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl fmt::Display for AirPlaceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "place#{}", self.0)
    }
}

/// A memory location that can be read from or written to.
///
/// A place represents a path to a memory location, consisting of a base
/// (local variable or parameter) and zero or more projections (field access,
/// array indexing).
///
/// # Examples
///
/// - `x` → `AirPlace { base: Local(0), base_type: T, ... }`
/// - `arr[i]` → `AirPlace { base: Local(0), base_type: Array, ... }` with `Index` projection
/// - `point.x` → `AirPlace { base: Local(0), base_type: Point, ... }` with `Field` projection
/// - `arr[i].x` → the same `Array` base type with `Index` then `Field`
#[derive(Debug, PartialEq, Eq)]
pub struct AirPlace {
    /// The base of the place - either a local slot or parameter slot
    pub base: AirPlaceBase,
    /// The logical type stored at the base before applying projections.
    ///
    /// A physical slot index does not uniquely identify this type: aggregate
    /// parameters are flattened and zero-sized parameters can share an ABI
    /// index. Carrying it on the place keeps the first projection typed.
    pub base_type: Type,
    /// Opaque owner-created range in Air's projection store.
    projections: AirProjectionRange,
}

impl AirPlace {
    /// Number of logical projections in this owner-mediated place.
    #[inline]
    pub const fn projection_count(&self) -> usize {
        self.projections.extent as usize
    }
    /// Create a place for a local variable with no projections.
    #[inline]
    pub const fn local(slot: u32, base_type: Type) -> Self {
        Self {
            base: AirPlaceBase::Local(slot),
            base_type,
            projections: AirProjectionRange::EMPTY,
        }
    }

    /// Create a place for a parameter with no projections.
    #[inline]
    pub const fn param(slot: u32, base_type: Type) -> Self {
        Self {
            base: AirPlaceBase::Param(slot),
            base_type,
            projections: AirProjectionRange::EMPTY,
        }
    }

    /// Returns true if this place has no projections (is just a variable).
    #[inline]
    pub const fn is_simple(&self) -> bool {
        self.projections.is_empty()
    }

    /// Returns the local slot if this is a simple local place with no projections.
    #[inline]
    pub const fn as_local(&self) -> Option<u32> {
        if self.projections.is_empty() {
            match self.base {
                AirPlaceBase::Local(slot) => Some(slot),
                AirPlaceBase::Param(_) | AirPlaceBase::Accessor(_) => None,
            }
        } else {
            None
        }
    }

    /// Returns the param slot if this is a simple param place with no projections.
    #[inline]
    pub const fn as_param(&self) -> Option<u32> {
        if self.projections.is_empty() {
            match self.base {
                AirPlaceBase::Param(slot) => Some(slot),
                AirPlaceBase::Local(_) | AirPlaceBase::Accessor(_) => None,
            }
        } else {
            None
        }
    }
}

/// Logical marker for the AIR place-projection family.  This marker is
/// intentionally distinct from every word payload family.
enum ProjectionFamily {}

/// Opaque range into an AIR owner's projection store.
///
/// The raw position is private: callers can only obtain a range while adding
/// a place to the same owner and can only traverse it through that owner.
#[repr(C)]
#[derive(Debug, PartialEq, Eq)]
struct AirProjectionRange {
    start: u32,
    extent: u32,
    family: PhantomData<fn() -> ProjectionFamily>,
}

macro_rules! word_range {
    ($name:ident) => {
        #[repr(C)]
        #[derive(Debug, PartialEq, Eq)]
        pub struct $name {
            start: u32,
            extent: u32,
        }
        impl $name {
            pub const fn len(&self) -> usize {
                self.extent as usize
            }
            pub const fn is_empty(&self) -> bool {
                self.extent == 0
            }
        }
        const _: () = assert!(std::mem::size_of::<$name>() == 8);
        const _: () = assert!(std::mem::align_of::<$name>() == 4);
    };
}

word_range!(AirMatchArms);
word_range!(AirCallArgs);
word_range!(AirTypeArgs);
word_range!(AirConstValueWords);
word_range!(AirIntrinsicArgs);
word_range!(AirBlockStatements);
word_range!(AirStructFields);
word_range!(AirSourceOrder);
word_range!(AirArrayElements);
word_range!(AirEnumPayload);

impl AirMatchArms {
    const EMPTY: Self = Self {
        start: 0,
        extent: 0,
    };
}

impl AirCallArgs {
    /// Number of logical two-word call-argument records.
    pub const fn count(&self) -> usize {
        self.extent as usize / 2
    }
}

impl AirProjectionRange {
    const EMPTY: Self = Self {
        start: 0,
        extent: 0,
        family: PhantomData,
    };

    const fn is_empty(&self) -> bool {
        self.extent == 0
    }
}

const _: () = assert!(std::mem::size_of::<AirProjectionRange>() == 2 * std::mem::size_of::<u32>());
const _: () = assert!(std::mem::align_of::<AirProjectionRange>() == std::mem::align_of::<u32>());

/// The base of a place - where the memory location starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AirPlaceBase {
    /// Local variable slot
    Local(u32),
    /// Parameter slot (for parameters, including inout)
    Param(u32),
    /// Result of a mandatory-inline `-> borrow T` accessor call. This is a
    /// second-class place producer, not a value-producing ABI call; CFG
    /// construction preserves it only until the accessor CFG splice.
    Accessor(AirRef),
}

/// A projection applied to a place to reach a nested location.
///
/// Projections are stored in `Air` and reached through an opaque typed range
/// owned by the containing place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AirProjection {
    /// Field access: `.field_name`
    ///
    /// The struct_id identifies the struct type, and field_index is the
    /// 0-based index of the field in declaration order.
    Field {
        struct_id: StructId,
        field_index: u32,
    },
    /// Array index: `[index]`
    ///
    /// The array_type is needed for bounds checking and element size calculation.
    /// The index is an AirRef that will be evaluated at runtime.
    Index { array_type: Type, index: AirRef },
}

/// Parameter passing mode in AIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AirParamMode {
    /// Normal pass-by-value parameter
    #[default]
    Normal,
    /// Inout parameter - mutated in place and returned to caller
    Inout,
    /// Borrow parameter - immutable borrow without ownership transfer
    Borrow,
}

/// Argument passing mode in AIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AirArgMode {
    /// Normal pass-by-value argument
    #[default]
    Normal,
    /// Inout argument - mutated in place
    Inout,
    /// Borrow argument - immutable borrow
    Borrow,
}

/// An argument in a function call (AIR level).
#[derive(Debug, Clone)]
pub struct AirCallArg {
    /// The argument expression
    pub value: AirRef,
    /// The passing mode for this argument
    pub mode: AirArgMode,
}

struct CallArgSchema;

impl CallArgSchema {
    const WIDTH: usize = 2;
    const VALUE: usize = 0;
    const MODE: usize = 1;

    fn encode(arg: &AirCallArg) -> [u32; Self::WIDTH] {
        let mut words = [0; Self::WIDTH];
        words[Self::VALUE] = arg.value.as_u32();
        words[Self::MODE] = match arg.mode {
            AirArgMode::Normal => 0,
            AirArgMode::Inout => 1,
            AirArgMode::Borrow => 2,
        };
        words
    }

    fn decode(
        words: &[u32],
        range: &AirCallArgs,
        record: usize,
    ) -> Result<AirCallArg, AirPayloadError> {
        debug_assert_eq!(words.len(), Self::WIDTH);
        let mode = match words[Self::MODE] {
            0 => AirArgMode::Normal,
            1 => AirArgMode::Inout,
            2 => AirArgMode::Borrow,
            _ => {
                return Err(AirPayloadError::decode(
                    "call arguments",
                    range.start,
                    range.extent,
                    record,
                    Self::WIDTH,
                    Self::WIDTH,
                    "invalid argument mode",
                ));
            }
        };
        Ok(AirCallArg {
            value: AirRef::from_raw(words[Self::VALUE]),
            mode,
        })
    }
}

impl AirCallArg {
    /// Returns true when this argument requests exclusive `inout` access.
    pub fn is_inout(&self) -> bool {
        self.mode == AirArgMode::Inout
    }

    /// Returns true if this argument is passed as borrow.
    pub fn is_borrow(&self) -> bool {
        self.mode == AirArgMode::Borrow
    }
}

impl fmt::Display for AirCallArg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.mode {
            AirArgMode::Inout => write!(f, "inout {}", self.value),
            AirArgMode::Borrow => write!(f, "borrow {}", self.value),
            AirArgMode::Normal => write!(f, "{}", self.value),
        }
    }
}

/// A pattern in a match expression (AIR level - typed).
#[derive(Debug, Clone)]
pub enum AirPattern {
    /// Wildcard pattern `_` - matches anything
    Wildcard,
    /// Integer literal pattern (can be positive or negative)
    Int(i64),
    /// Boolean literal pattern
    Bool(bool),
    /// Enum variant pattern (e.g., Color::Red)
    EnumVariant {
        /// The enum type ID
        enum_id: crate::types::EnumId,
        /// The variant index (0-based)
        variant_index: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PatternTag {
    Wildcard,
    Int,
    Bool,
    EnumVariant,
}

#[derive(Debug, Clone, Copy)]
enum PatternFields {
    Wildcard,
    Int { low: usize, high: usize },
    Bool { value: usize },
    EnumVariant { enum_id: usize, variant: usize },
}

#[derive(Debug, Clone, Copy)]
struct PatternLayout {
    tag: usize,
    body: usize,
    width: usize,
    fields: PatternFields,
}

impl PatternTag {
    const TAG_OFFSET: usize = 0;

    const fn word(self) -> u32 {
        match self {
            Self::Wildcard => 0,
            Self::Int => 1,
            Self::Bool => 2,
            Self::EnumVariant => 3,
        }
    }

    const fn layout(self) -> PatternLayout {
        match self {
            Self::Wildcard => PatternLayout {
                tag: 0,
                body: 1,
                width: 2,
                fields: PatternFields::Wildcard,
            },
            Self::Int => PatternLayout {
                tag: 0,
                body: 1,
                width: 4,
                fields: PatternFields::Int { low: 2, high: 3 },
            },
            Self::Bool => PatternLayout {
                tag: 0,
                body: 1,
                width: 3,
                fields: PatternFields::Bool { value: 2 },
            },
            Self::EnumVariant => PatternLayout {
                tag: 0,
                body: 1,
                width: 4,
                fields: PatternFields::EnumVariant {
                    enum_id: 2,
                    variant: 3,
                },
            },
        }
    }

    const fn from_word(word: u32) -> Option<Self> {
        match word {
            0 => Some(Self::Wildcard),
            1 => Some(Self::Int),
            2 => Some(Self::Bool),
            3 => Some(Self::EnumVariant),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConstValueTag {
    Integer,
    Bool,
    Unit,
    Type,
    Function,
}

impl ConstValueTag {
    fn of(value: &crate::sema::ConstValue) -> Self {
        match value {
            crate::sema::ConstValue::Integer(_) => Self::Integer,
            crate::sema::ConstValue::Bool(_) => Self::Bool,
            crate::sema::ConstValue::Unit => Self::Unit,
            crate::sema::ConstValue::Type(_) => Self::Type,
            crate::sema::ConstValue::Function(_) => Self::Function,
            crate::sema::ConstValue::String(_) => {
                unreachable!("string const values are rejected before tag classification")
            }
        }
    }

    const fn word(self) -> u32 {
        match self {
            Self::Integer => 0,
            Self::Bool => 1,
            Self::Unit => 2,
            Self::Type => 3,
            Self::Function => 4,
        }
    }

    const fn payload_width(self) -> usize {
        match self {
            Self::Integer => 4,
            Self::Bool | Self::Type | Self::Function => 1,
            Self::Unit => 0,
        }
    }

    const fn from_word(word: u32) -> Option<Self> {
        match word {
            0 => Some(Self::Integer),
            1 => Some(Self::Bool),
            2 => Some(Self::Unit),
            3 => Some(Self::Type),
            4 => Some(Self::Function),
            _ => None,
        }
    }
}

pub(crate) fn encode_const_values(
    values: &[crate::sema::ConstValue],
) -> Result<Vec<u32>, AirBuildError> {
    let error = |kind| AirBuildError {
        phase: "AIR",
        family: "constant value arguments",
        operation: "stage",
        kind,
    };
    // No comptime parameter has a string type, so the comptime engine never
    // yields a string const as a specialization argument (RUE-957). Reject
    // up front so the tag/payload scheme below stays scalar-only.
    if values
        .iter()
        .any(|value| matches!(value, crate::sema::ConstValue::String(_)))
    {
        return Err(error(AirBuildErrorKind::ProducerInvariant(
            "string constants are not valid comptime argument values",
        )));
    }
    let word_count = values.iter().try_fold(0usize, |count, value| {
        count.checked_add(1 + ConstValueTag::of(value).payload_width())
    });
    let word_count = word_count.ok_or_else(|| error(AirBuildErrorKind::ResourceLimit))?;
    let mut words = Vec::new();
    words
        .try_reserve(word_count)
        .map_err(|_| error(AirBuildErrorKind::ResourceExhaustion))?;
    for value in values {
        match value {
            crate::sema::ConstValue::Integer(value) => {
                words.push(ConstValueTag::Integer.word());
                let bytes = value.to_le_bytes();
                for chunk in bytes.chunks_exact(4) {
                    words.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                }
            }
            crate::sema::ConstValue::Bool(value) => {
                words.push(ConstValueTag::Bool.word());
                words.push(u32::from(*value));
            }
            crate::sema::ConstValue::Unit => words.push(ConstValueTag::Unit.word()),
            crate::sema::ConstValue::Type(value) => {
                words.push(ConstValueTag::Type.word());
                words.push(value.as_u32());
            }
            crate::sema::ConstValue::Function(value) => {
                words.push(ConstValueTag::Function.word());
                words.push(
                    u32::try_from(value.into_usize())
                        .map_err(|_| error(AirBuildErrorKind::ResourceLimit))?,
                );
            }
            crate::sema::ConstValue::String(_) => {
                unreachable!("string const values are rejected before encoding")
            }
        }
    }
    Ok(words)
}

impl AirPattern {
    fn tag(&self) -> PatternTag {
        match self {
            Self::Wildcard => PatternTag::Wildcard,
            Self::Int(_) => PatternTag::Int,
            Self::Bool(_) => PatternTag::Bool,
            Self::EnumVariant { .. } => PatternTag::EnumVariant,
        }
    }

    /// Encode this pattern to the extra array using its canonical layout plan.
    /// Format:
    /// - Wildcard: [tag, body_ref] = 2 words
    /// - Int: [tag, body_ref, lo, hi] = 4 words (i64 as two u32s)
    /// - Bool: [tag, body_ref, value] = 3 words
    /// - EnumVariant: [tag, body_ref, enum_id, variant_index] = 4 words
    fn encode(&self, body: AirRef, out: &mut Vec<u32>) -> Result<(), AirBuildError> {
        let tag = self.tag();
        let layout = tag.layout();
        let mut record = [0; 4];
        record[layout.tag] = tag.word();
        record[layout.body] = body.as_u32();
        match (self, layout.fields) {
            (AirPattern::Wildcard, PatternFields::Wildcard) => {}
            (AirPattern::Int(n), PatternFields::Int { low, high }) => {
                let bytes = n.to_le_bytes();
                record[low] = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                record[high] = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
            }
            (AirPattern::Bool(b), PatternFields::Bool { value }) => {
                record[value] = u32::from(*b);
            }
            (
                AirPattern::EnumVariant {
                    enum_id,
                    variant_index,
                },
                PatternFields::EnumVariant {
                    enum_id: enum_offset,
                    variant,
                },
            ) => {
                record[enum_offset] = enum_id.0;
                record[variant] = *variant_index;
            }
            _ => {
                return Err(AirBuildError {
                    phase: "AIR",
                    family: "match arms",
                    operation: "encode",
                    kind: AirBuildErrorKind::ProducerInvariant("pattern/layout mismatch"),
                });
            }
        }
        out.extend_from_slice(&record[..layout.width]);
        Ok(())
    }
}

/// Iterator for reading match arms from the extra array.
pub struct MatchArmIterator<'a> {
    data: &'a [u32],
    remaining: usize,
}

impl Iterator for MatchArmIterator<'_> {
    type Item = (AirPattern, AirRef);

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;

        let tag = PatternTag::from_word(self.data[PatternTag::TAG_OFFSET])?;
        let layout = tag.layout();
        let body = AirRef::from_raw(self.data[layout.body]);

        let pattern = match layout.fields {
            PatternFields::Wildcard => AirPattern::Wildcard,
            PatternFields::Int { low, high } => {
                let lo = self.data[low].to_le_bytes();
                let hi = self.data[high].to_le_bytes();
                AirPattern::Int(i64::from_le_bytes([
                    lo[0], lo[1], lo[2], lo[3], hi[0], hi[1], hi[2], hi[3],
                ]))
            }
            PatternFields::Bool { value } => AirPattern::Bool(self.data[value] != 0),
            PatternFields::EnumVariant { enum_id, variant } => {
                let enum_id = crate::types::EnumId(self.data[enum_id]);
                let variant_index = self.data[variant];
                AirPattern::EnumVariant {
                    enum_id,
                    variant_index,
                }
            }
        };

        self.data = &self.data[layout.width..];
        Some((pattern, body))
    }
}

impl ExactSizeIterator for MatchArmIterator<'_> {}

#[derive(Debug)]
pub struct ConstValueIterator<'a> {
    words: &'a [u32],
    remaining: usize,
}

impl Iterator for ConstValueIterator<'_> {
    type Item = crate::sema::ConstValue;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let tag = ConstValueTag::from_word(self.words[0])?;
        let payload = &self.words[1..1 + tag.payload_width()];
        self.words = &self.words[1 + tag.payload_width()..];
        self.remaining -= 1;
        Some(match tag {
            ConstValueTag::Integer => {
                let mut bits = 0u128;
                for (index, word) in payload.iter().copied().enumerate() {
                    bits |= u128::from(word) << (32 * index);
                }
                crate::sema::ConstValue::Integer(bits as i128)
            }
            ConstValueTag::Bool => crate::sema::ConstValue::Bool(payload[0] == 1),
            ConstValueTag::Unit => crate::sema::ConstValue::Unit,
            ConstValueTag::Type => crate::sema::ConstValue::Type(
                Type::try_from_u32(payload[0]).expect("validated const type"),
            ),
            ConstValueTag::Function => crate::sema::ConstValue::Function(
                Spur::try_from_usize(payload[0] as usize).expect("validated const symbol"),
            ),
        })
    }
}

impl ExactSizeIterator for ConstValueIterator<'_> {
    fn len(&self) -> usize {
        self.remaining
    }
}

/// A reference to an instruction in the AIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AirRef(u32);

impl AirRef {
    #[inline]
    pub const fn from_raw(index: u32) -> Self {
        Self(index)
    }

    #[inline]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// The complete AIR for a function.
// Clone exists for the loop back-edge move recheck in sema, which re-analyzes
// a loop body against a scratch copy of the Air and then discards it.
#[derive(Debug, Default)]
pub struct Air {
    instructions: Vec<AirInst>,
    /// Extra data for variable-length instruction payloads (args, elements, etc.)
    extra: Vec<u32>,
    /// The return type of this function
    return_type: Type,
    /// Storage for place projections.
    /// AirPlace instructions store (start, len) indices into this array.
    projections: Vec<AirProjection>,
    /// Storage for places.
    /// AirPlaceRef values are indices into this array.
    places: Vec<AirPlace>,
    /// Owned (pass-by-value) parameters of this function: (ABI slot, type).
    /// Ownership of a by-value argument transfers to the callee, so the CFG
    /// builder drops these at function exit unless they were moved out
    /// (see [`AirInstData::MarkMoved`]). Empty for destructors (a destructor
    /// must not re-drop `self`) and for synthesized drop-glue functions.
    param_drops: Vec<(u32, Type)>,
    /// Local slots that hold a non-owning *borrow* value and so must NOT be
    /// dropped at scope exit. Currently the element binder of a `for` loop
    /// over a non-Copy collection: iteration is a shared read (spec 4.8:26),
    /// so the binder aliases an element the collection still owns and drops —
    /// dropping the binder too would double-free (RUE-259).
    borrow_slots: Vec<u32>,
    /// Set once [`Air::push_inst`] was asked for an instruction beyond the
    /// published per-body ceiling ([`MAX_AIR_INSTRUCTIONS_PER_BODY`],
    /// spec Appendix C.6:1).
    ///
    /// `push_inst` runs from every non-payload lowering site in semantic
    /// analysis and returns a bare `AirRef`, so the ceiling cannot be reported
    /// by threading a `Result` back through all of them. The owner records the
    /// rejection here, stops growing, and [`Air::finish`] converts the record
    /// into the `E1401` diagnostic spec C.1:2 requires instead of handing back
    /// an index truncated by `len() as u32`.
    instruction_limit_exceeded: bool,
    #[cfg(test)]
    place_reserve_failure: Option<PlaceReserveFailure>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaceReserveFailure {
    ProjectionStore,
    PlaceStore,
}

/// Read-only storage accounting for benchmark and architecture audits.
///
/// Word families share one allocation, so `family_logical_bytes` reports each
/// family's exact retained contribution while `word_store_capacity_bytes`
/// reports the allocation once rather than inventing per-family capacity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AirPayloadStorageStats {
    pub family_logical_bytes: [usize; 10],
    pub word_store_logical_bytes: usize,
    pub word_store_capacity_bytes: usize,
    pub projection_store_logical_bytes: usize,
    pub projection_store_capacity_bytes: usize,
    pub place_store_logical_bytes: usize,
    pub place_store_capacity_bytes: usize,
    pub nonempty_match_envelopes: usize,
    /// Largest logical scratch payload produced by an atomic builder.
    pub peak_staging_bytes: usize,
}

/// Stable order of `AirPayloadStorageStats::family_logical_bytes`.
pub const AIR_PAYLOAD_FAMILY_NAMES: [&str; 10] = [
    "match_arms",
    "call_args",
    "type_args",
    "const_values",
    "intrinsic_args",
    "block_statements",
    "struct_fields",
    "source_order",
    "array_elements",
    "enum_payload",
];

#[derive(Clone, Copy)]
pub(crate) struct AirCheckpoint {
    instructions: usize,
    extra: usize,
    projections: usize,
    places: usize,
    param_drops: usize,
    borrow_slots: usize,
}

/// The only public mutable AIR form. Payload-bearing instructions are added
/// atomically so their owner-issued ranges cannot be detached or transferred.
///
/// Payload positions deliberately cannot be reconstructed by a consumer:
///
/// ```compile_fail
/// use rue_air::AirCallArgs;
/// let detached = AirCallArgs { start: 0, extent: 0 };
/// ```
#[derive(Debug)]
pub struct AirEditor {
    air: Air,
}

/// Immutable AIR that has crossed the owner/context validation boundary.
#[derive(Debug)]
pub struct ValidatedAir {
    air: Air,
}

impl std::ops::Deref for ValidatedAir {
    type Target = Air;

    fn deref(&self) -> &Self::Target {
        &self.air
    }
}

impl std::ops::Deref for AirEditor {
    type Target = Air;

    fn deref(&self) -> &Self::Target {
        &self.air
    }
}

impl AirEditor {
    pub fn new(return_type: Type) -> Self {
        Self {
            air: Air::new(return_type),
        }
    }

    pub fn add_const(&mut self, value: u64, ty: Type, span: Span) -> AirRef {
        self.air.add_inst(AirInst {
            data: AirInstData::Const(value),
            ty,
            span,
        })
    }

    pub fn add_unit(&mut self, span: Span) -> AirRef {
        self.air.add_inst(AirInst {
            data: AirInstData::UnitConst,
            ty: Type::UNIT,
            span,
        })
    }

    pub fn add_param(&mut self, index: u32, ty: Type, span: Span) -> AirRef {
        self.air.add_inst(AirInst {
            data: AirInstData::Param { index },
            ty,
            span,
        })
    }

    pub fn add_drop(&mut self, value: AirRef, span: Span) -> AirRef {
        self.air.add_inst(AirInst {
            data: AirInstData::Drop { value },
            ty: Type::UNIT,
            span,
        })
    }

    pub fn add_ret(&mut self, value: Option<AirRef>, ty: Type, span: Span) -> AirRef {
        self.air.add_inst(AirInst {
            data: AirInstData::Ret(value),
            ty,
            span,
        })
    }

    pub fn add_add(&mut self, lhs: AirRef, rhs: AirRef, ty: Type, span: Span) -> AirRef {
        self.air.add_inst(AirInst {
            data: AirInstData::Add(lhs, rhs),
            ty,
            span,
        })
    }

    pub fn add_storage_live(&mut self, slot: u32, ty: Type, span: Span) -> AirRef {
        self.air.add_inst(AirInst {
            data: AirInstData::StorageLive { slot },
            ty,
            span,
        })
    }

    pub fn add_alloc(&mut self, slot: u32, init: AirRef, span: Span) -> AirRef {
        self.air.add_inst(AirInst {
            data: AirInstData::Alloc { slot, init },
            ty: Type::UNIT,
            span,
        })
    }

    pub fn add_place_read(&mut self, place: AirPlaceRef, ty: Type, span: Span) -> AirRef {
        self.air.add_inst(AirInst {
            data: AirInstData::PlaceRead { place },
            ty,
            span,
        })
    }

    pub fn add_match(
        &mut self,
        scrutinee: AirRef,
        arms: &[(AirPattern, AirRef)],
        ty: Type,
        span: Span,
    ) -> Result<AirRef, AirBuildError> {
        self.air.add_match(scrutinee, arms, ty, span)
    }

    pub fn add_call(
        &mut self,
        runtime: Option<crate::RuntimeCallKind>,
        name: Spur,
        args: &[AirCallArg],
        ty: Type,
        span: Span,
    ) -> Result<AirRef, AirBuildError> {
        self.air.add_call(runtime, name, args, ty, span)
    }

    pub fn add_accessor_call(
        &mut self,
        name: Spur,
        args: &[AirCallArg],
        ty: Type,
        span: Span,
    ) -> Result<AirRef, AirBuildError> {
        self.air.add_accessor_call(name, args, ty, span)
    }

    pub fn add_call_generic(
        &mut self,
        name: Spur,
        type_args: &[Type],
        value_args: &[crate::sema::ConstValue],
        args: &[AirCallArg],
        ty: Type,
        span: Span,
    ) -> Result<AirRef, AirBuildError> {
        self.air
            .add_call_generic(name, type_args, value_args, args, ty, span)
    }

    pub fn add_intrinsic(
        &mut self,
        runtime: Option<crate::RuntimeCallKind>,
        name: Spur,
        args: &[AirRef],
        ty: Type,
        span: Span,
    ) -> Result<AirRef, AirBuildError> {
        self.air.add_intrinsic(runtime, name, args, ty, span)
    }

    pub fn add_block(
        &mut self,
        statements: &[AirRef],
        value: AirRef,
        ty: Type,
        span: Span,
    ) -> Result<AirRef, AirBuildError> {
        self.air.add_block(statements, value, ty, span)
    }

    pub fn add_struct_init(
        &mut self,
        struct_id: StructId,
        fields: &[AirRef],
        source_order: &[u32],
        ty: Type,
        span: Span,
    ) -> Result<AirRef, AirBuildError> {
        self.air
            .add_struct_init(struct_id, fields, source_order, ty, span)
    }

    pub fn add_array_init(
        &mut self,
        elements: &[AirRef],
        ty: Type,
        span: Span,
    ) -> Result<AirRef, AirBuildError> {
        self.air.add_array_init(elements, ty, span)
    }

    pub fn add_enum_variant(
        &mut self,
        enum_id: crate::EnumId,
        variant_index: u32,
        payload: &[AirRef],
        ty: Type,
        span: Span,
    ) -> Result<AirRef, AirBuildError> {
        self.air
            .add_enum_variant(enum_id, variant_index, payload, ty, span)
    }

    pub fn make_place(
        &mut self,
        base: AirPlaceBase,
        base_type: Type,
        projections: impl IntoIterator<Item = AirProjection>,
    ) -> Result<AirPlaceRef, AirBuildError> {
        self.air.make_place(base, base_type, projections)
    }

    pub fn set_param_drops(&mut self, drops: Vec<(u32, Type)>) {
        self.air.set_param_drops(drops);
    }

    pub(crate) fn rewrite_generic_call_to_call(&mut self, index: usize, name: Spur) {
        self.air.rewrite_generic_call_to_call(index, name);
    }

    pub fn finish(
        self,
        context: AirValidationContext<'_>,
    ) -> Result<ValidatedAir, AirValidationError> {
        self.air.finish(context)
    }
}

impl ValidatedAir {
    pub(crate) fn from_semantic_air(
        air: Air,
        pool: &TypeInternPool,
    ) -> Result<Self, AirValidationError> {
        air.finish(AirValidationContext::Semantic(pool))
    }

    pub(crate) fn from_semantic_air_with_symbols(
        air: Air,
        pool: &TypeInternPool,
        interner: &ThreadedRodeo,
    ) -> Result<Self, AirValidationError> {
        air.finish(AirValidationContext::SemanticWithSymbols(pool, interner))
    }

    pub fn into_editor(self) -> AirEditor {
        AirEditor { air: self.air }
    }
}

impl Air {
    fn finish(self, context: AirValidationContext<'_>) -> Result<ValidatedAir, AirValidationError> {
        // Report the published per-body instruction ceiling first: a latched
        // owner holds a truncated reference graph, so every structural finding
        // downstream would be a consequence of the limit rather than a producer
        // bug, and spec C.1:2 wants the limit named (E1401), not an E9000.
        if let Some(error) = self.latched_capacity_error() {
            return Err(error);
        }
        let fail = |instruction, reason| AirValidationError {
            instruction,
            reason,
            kind: AirValidationErrorKind::Structural,
        };
        let type_cache = std::cell::RefCell::new(([Type::UNIT; 64], 0usize));
        let validate_type = |ty| -> Result<(), String> {
            let mut cache = type_cache.borrow_mut();
            if cache.0[..cache.1].contains(&ty) {
                return Ok(());
            }
            context.validate_type(ty)?;
            if cache.1 < cache.0.len() {
                let index = cache.1;
                cache.0[index] = ty;
                cache.1 += 1;
            }
            Ok(())
        };
        validate_type(self.return_type)
            .map_err(|reason| fail(None, format!("invalid return type: {reason}")))?;
        for (slot, ty) in &self.param_drops {
            validate_type(*ty).map_err(|reason| {
                fail(
                    None,
                    format!("invalid parameter-drop type at slot {slot}: {reason}"),
                )
            })?;
        }
        for (place_index, place) in self.places.iter().enumerate() {
            validate_type(place.base_type).map_err(|reason| {
                fail(
                    None,
                    format!("invalid place {place_index} base type: {reason}"),
                )
            })?;
            let start = place.projections.start as usize;
            let end = start
                .checked_add(place.projections.extent as usize)
                .ok_or_else(|| fail(None, format!("place {place_index} range overflow")))?;
            if place.projections.extent == 0 && place.projections.start != 0 {
                return Err(fail(
                    None,
                    format!("place {place_index} has a non-canonical empty range"),
                ));
            }
            if self.projections.get(start..end).is_none() {
                return Err(fail(
                    None,
                    format!("place {place_index} range is outside the projection store"),
                ));
            }
        }
        for (index, inst) in self.instructions.iter().enumerate() {
            if inst.span.start > inst.span.end {
                return Err(fail(
                    Some(index),
                    "source span start exceeds its end".into(),
                ));
            }
            validate_type(inst.ty)
                .map_err(|reason| fail(Some(index), format!("invalid result type: {reason}")))?;
        }
        // Places are owner-level records, not instructions. Validate every
        // projection path here even when no instruction happens to consume it.
        for (place_index, place) in self.places.iter().enumerate() {
            let mut current = place.base_type;
            for projection in self.get_place_projections(place) {
                current = match *projection {
                    AirProjection::Field {
                        struct_id,
                        field_index,
                    } => {
                        if current != Type::new_struct(struct_id) {
                            return Err(fail(
                                None,
                                format!(
                                    "place {place_index} field projection expects struct {:?}, found {}",
                                    struct_id,
                                    current.name()
                                ),
                            ));
                        }
                        context
                            .struct_field_type(struct_id, field_index)
                            .map_err(|reason| {
                                fail(None, format!("place {place_index}: {reason}"))
                            })?
                    }
                    AirProjection::Index {
                        array_type,
                        index: operand,
                    } => {
                        if current != array_type {
                            return Err(fail(
                                None,
                                format!(
                                    "place {place_index} index projection expects {}, found {}",
                                    array_type.name(),
                                    current.name()
                                ),
                            ));
                        }
                        let operand_inst = self
                            .instructions
                            .get(operand.as_u32() as usize)
                            .ok_or_else(|| {
                                fail(
                                    None,
                                    format!(
                                        "place {place_index} projection index {operand} is outside the instruction store"
                                    ),
                                )
                            })?;
                        if !operand_inst.ty.is_integer() {
                            return Err(fail(
                                None,
                                format!(
                                    "place {place_index} projection index has non-integer type {}",
                                    operand_inst.ty.name()
                                ),
                            ));
                        }
                        context.array_element_type(array_type).map_err(|reason| {
                            fail(None, format!("place {place_index}: {reason}"))
                        })?
                    }
                };
            }
        }
        for (index, inst) in self.instructions.iter().enumerate() {
            let check_ref = |value: AirRef| {
                if value.as_u32() as usize >= index {
                    Err(fail(
                        Some(index),
                        format!("reference {value} is not defined before its use"),
                    ))
                } else {
                    Ok(())
                }
            };
            let check_place = |place_ref: AirPlaceRef| -> Result<Type, AirValidationError> {
                let place = self
                    .places
                    .get(place_ref.as_u32() as usize)
                    .ok_or_else(|| {
                        fail(
                            Some(index),
                            format!("place reference {place_ref} is outside the place store"),
                        )
                    })?;
                if let AirPlaceBase::Accessor(call) = place.base {
                    check_ref(call)?;
                    if !matches!(self.get(call).data, AirInstData::AccessorCall { .. }) {
                        return Err(fail(
                            Some(index),
                            format!("place {place_ref} has a non-accessor producer {call}"),
                        ));
                    }
                    if self.get(call).ty != place.base_type {
                        return Err(fail(
                            Some(index),
                            format!(
                                "place {place_ref} accessor base type does not match its producer"
                            ),
                        ));
                    }
                }
                let mut current = place.base_type;
                for projection in self.get_place_projections(place) {
                    current = match *projection {
                        AirProjection::Field {
                            struct_id,
                            field_index,
                        } => {
                            if current != Type::new_struct(struct_id) {
                                return Err(fail(
                                    Some(index),
                                    format!(
                                        "field projection expects struct {:?}, found {}",
                                        struct_id,
                                        current.name()
                                    ),
                                ));
                            }
                            context
                                .struct_field_type(struct_id, field_index)
                                .map_err(|reason| fail(Some(index), reason))?
                        }
                        AirProjection::Index {
                            array_type,
                            index: operand,
                        } => {
                            if current != array_type {
                                return Err(fail(
                                    Some(index),
                                    format!(
                                        "index projection expects {}, found {}",
                                        array_type.name(),
                                        current.name()
                                    ),
                                ));
                            }
                            check_ref(operand)?;
                            let operand_ty = self.instructions[operand.as_u32() as usize].ty;
                            if !operand_ty.is_integer() {
                                return Err(fail(
                                    Some(index),
                                    format!(
                                        "projection index has non-integer type {}",
                                        operand_ty.name()
                                    ),
                                ));
                            }
                            context
                                .array_element_type(array_type)
                                .map_err(|reason| fail(Some(index), reason))?
                        }
                    };
                }
                Ok(current)
            };

            match &inst.data {
                AirInstData::TypeConst(ty) => validate_type(*ty).map_err(|reason| {
                    fail(Some(index), format!("invalid type constant: {reason}"))
                })?,
                AirInstData::Add(a, b)
                | AirInstData::Sub(a, b)
                | AirInstData::Mul(a, b)
                | AirInstData::WrappingAdd(a, b)
                | AirInstData::WrappingSub(a, b)
                | AirInstData::WrappingMul(a, b)
                | AirInstData::Div(a, b)
                | AirInstData::Mod(a, b)
                | AirInstData::Eq(a, b)
                | AirInstData::Ne(a, b)
                | AirInstData::Lt(a, b)
                | AirInstData::Gt(a, b)
                | AirInstData::Le(a, b)
                | AirInstData::Ge(a, b)
                | AirInstData::And(a, b)
                | AirInstData::Or(a, b)
                | AirInstData::BitAnd(a, b)
                | AirInstData::BitOr(a, b)
                | AirInstData::BitXor(a, b)
                | AirInstData::Shl(a, b)
                | AirInstData::Shr(a, b) => {
                    check_ref(*a)?;
                    check_ref(*b)?;
                }
                AirInstData::Neg(value)
                | AirInstData::Not(value)
                | AirInstData::BitNot(value)
                | AirInstData::Drop { value } => check_ref(*value)?,
                AirInstData::Branch {
                    cond,
                    then_value,
                    else_value,
                } => {
                    check_ref(*cond)?;
                    check_ref(*then_value)?;
                    if let Some(value) = else_value {
                        check_ref(*value)?;
                    }
                }
                AirInstData::Loop { cond, body } => {
                    check_ref(*cond)?;
                    check_ref(*body)?;
                }
                AirInstData::InfiniteLoop { body } => check_ref(*body)?,
                AirInstData::Alloc { init, .. }
                | AirInstData::Store { value: init, .. }
                | AirInstData::ParamStore { value: init, .. } => check_ref(*init)?,
                AirInstData::Ret(value) => {
                    if let Some(value) = value {
                        check_ref(*value)?;
                    }
                }
                AirInstData::PlaceRead { place } => {
                    check_place(*place)?;
                }
                AirInstData::PlaceWrite { place, value } => {
                    check_place(*place)?;
                    check_ref(*value)?;
                }
                AirInstData::EnumPayloadGet {
                    base,
                    enum_id,
                    variant_index,
                    field_index,
                } => {
                    check_ref(*base)?;
                    validate_type(Type::new_enum(*enum_id)).map_err(|reason| {
                        fail(Some(index), format!("invalid enum identity: {reason}"))
                    })?;
                    let payload_ty = context
                        .enum_payload_type(*enum_id, *variant_index, *field_index)
                        .map_err(|reason| fail(Some(index), reason))?;
                    if inst.ty != payload_ty {
                        return Err(fail(
                            Some(index),
                            "enum payload result type does not match its declaration".into(),
                        ));
                    }
                }
                AirInstData::IntCast { value, from_ty } => {
                    check_ref(*value)?;
                    validate_type(*from_ty).map_err(|reason| {
                        fail(Some(index), format!("invalid cast source type: {reason}"))
                    })?;
                }
                AirInstData::MarkMoved { value, place, .. } => {
                    check_ref(*value)?;
                    if let Some(place) = place {
                        check_place(*place)?;
                    }
                }
                _ => {}
            }
            match &inst.data {
                AirInstData::Match { scrutinee, arms } => {
                    check_ref(*scrutinee)?;
                    let scrutinee_ty = self.instructions[scrutinee.as_u32() as usize].ty;
                    for (pattern, body) in self
                        .try_get_match_arms(arms)
                        .map_err(|e| fail(Some(index), e.to_string()))?
                    {
                        check_ref(body)?;
                        match pattern {
                            AirPattern::Wildcard => {}
                            AirPattern::Int(_) if !scrutinee_ty.is_integer() => {
                                return Err(fail(
                                    Some(index),
                                    format!(
                                        "integer pattern does not match scrutinee type {}",
                                        scrutinee_ty.name()
                                    ),
                                ));
                            }
                            AirPattern::Bool(_) if scrutinee_ty != Type::BOOL => {
                                return Err(fail(
                                    Some(index),
                                    format!(
                                        "boolean pattern does not match scrutinee type {}",
                                        scrutinee_ty.name()
                                    ),
                                ));
                            }
                            AirPattern::EnumVariant {
                                enum_id,
                                variant_index,
                            } => {
                                validate_type(Type::new_enum(enum_id)).map_err(|reason| {
                                    fail(Some(index), format!("invalid enum pattern: {reason}"))
                                })?;
                                context
                                    .validate_enum_variant(enum_id, variant_index)
                                    .map_err(|reason| {
                                        fail(Some(index), format!("invalid enum pattern: {reason}"))
                                    })?;
                                if scrutinee_ty != Type::new_enum(enum_id) {
                                    return Err(fail(
                                        Some(index),
                                        format!(
                                            "enum pattern {:?} does not match scrutinee type {}",
                                            enum_id,
                                            scrutinee_ty.name()
                                        ),
                                    ));
                                }
                            }
                            AirPattern::Int(_) | AirPattern::Bool(_) => {}
                        }
                    }
                }
                AirInstData::Call { name, args, .. } | AirInstData::AccessorCall { name, args } => {
                    context
                        .validate_symbol(*name)
                        .map_err(|reason| fail(Some(index), reason))?;
                    for arg in self
                        .try_get_call_args(args)
                        .map_err(|e| fail(Some(index), e.to_string()))?
                    {
                        check_ref(arg.value)?;
                    }
                }
                AirInstData::CallGeneric {
                    name,
                    type_args,
                    value_args,
                    args,
                    ..
                } => {
                    context
                        .validate_symbol(*name)
                        .map_err(|reason| fail(Some(index), reason))?;
                    for &raw in self
                        .try_get_words(type_args.start, type_args.extent, "type arguments")
                        .map_err(|e| fail(Some(index), e.to_string()))?
                    {
                        let ty = Type::try_from_u32(raw).ok_or_else(|| {
                            fail(Some(index), "invalid type argument encoding".into())
                        })?;
                        validate_type(ty).map_err(|reason| {
                            fail(Some(index), format!("invalid type argument: {reason}"))
                        })?;
                    }
                    for value in self
                        .try_get_const_values(value_args)
                        .map_err(|e| fail(Some(index), e.to_string()))?
                    {
                        if let crate::sema::ConstValue::Type(ty) = value {
                            validate_type(ty).map_err(|reason| {
                                fail(
                                    Some(index),
                                    format!("invalid constant type argument: {reason}"),
                                )
                            })?;
                        } else if let crate::sema::ConstValue::Function(symbol) = value {
                            context
                                .validate_symbol(symbol)
                                .map_err(|reason| fail(Some(index), reason))?;
                        }
                    }
                    for arg in self
                        .try_get_call_args(args)
                        .map_err(|e| fail(Some(index), e.to_string()))?
                    {
                        check_ref(arg.value)?;
                    }
                }
                AirInstData::Intrinsic { name, args, .. } => {
                    context
                        .validate_symbol(*name)
                        .map_err(|reason| fail(Some(index), reason))?;
                    for &raw in self
                        .try_get_words(args.start, args.extent, "intrinsic arguments")
                        .map_err(|e| fail(Some(index), e.to_string()))?
                    {
                        check_ref(AirRef::from_raw(raw))?;
                    }
                }
                AirInstData::Block { statements, value } => {
                    for &raw in self
                        .try_get_words(statements.start, statements.extent, "block statements")
                        .map_err(|e| fail(Some(index), e.to_string()))?
                    {
                        check_ref(AirRef::from_raw(raw))?;
                    }
                    check_ref(*value)?;
                }
                AirInstData::StructInit {
                    struct_id,
                    fields,
                    source_order,
                } => {
                    validate_type(Type::new_struct(*struct_id)).map_err(|reason| {
                        fail(Some(index), format!("invalid struct identity: {reason}"))
                    })?;
                    if inst.ty != Type::new_struct(*struct_id) {
                        return Err(fail(
                            Some(index),
                            "struct initialization result type has a different identity".into(),
                        ));
                    }
                    for &raw in self
                        .try_get_words(fields.start, fields.extent, "struct fields")
                        .map_err(|e| fail(Some(index), e.to_string()))?
                    {
                        check_ref(AirRef::from_raw(raw))?;
                    }
                    let order: Vec<_> = self
                        .try_get_words(source_order.start, source_order.extent, "source order")
                        .map_err(|e| fail(Some(index), e.to_string()))?
                        .iter()
                        .map(|&value| value as usize)
                        .collect();
                    if order.len() != fields.len() {
                        return Err(fail(Some(index), "source order length mismatch".into()));
                    }
                    let mut seen = vec![false; fields.len()];
                    for source in order {
                        if source >= seen.len() || std::mem::replace(&mut seen[source], true) {
                            return Err(fail(
                                Some(index),
                                "source order is not a permutation".into(),
                            ));
                        }
                    }
                }
                AirInstData::ArrayInit { elements } => {
                    validate_type(inst.ty).map_err(|reason| {
                        fail(Some(index), format!("invalid array identity: {reason}"))
                    })?;
                    if inst.ty.as_array().is_none() {
                        return Err(fail(
                            Some(index),
                            "array initialization result is not an array type".into(),
                        ));
                    }
                    for &raw in self
                        .try_get_words(elements.start, elements.extent, "array elements")
                        .map_err(|e| fail(Some(index), e.to_string()))?
                    {
                        check_ref(AirRef::from_raw(raw))?;
                    }
                }
                AirInstData::EnumVariant {
                    enum_id,
                    variant_index,
                    payload,
                } => {
                    if inst.ty != Type::new_enum(*enum_id) {
                        return Err(fail(
                            Some(index),
                            "enum construction result type has a different identity".into(),
                        ));
                    }
                    let expected = context
                        .enum_payload_len(*enum_id, *variant_index)
                        .map_err(|reason| fail(Some(index), reason))?;
                    if payload.len() != expected {
                        return Err(fail(
                            Some(index),
                            format!(
                                "enum payload length {} does not match declaration length {expected}",
                                payload.len()
                            ),
                        ));
                    }
                    for &raw in self
                        .try_get_words(payload.start, payload.extent, "enum payload")
                        .map_err(|e| fail(Some(index), e.to_string()))?
                    {
                        check_ref(AirRef::from_raw(raw))?;
                    }
                }
                _ => {}
            }
        }
        Ok(ValidatedAir { air: self })
    }

    fn append_words(
        &mut self,
        family: &'static str,
        words: &[u32],
    ) -> Result<(u32, u32), AirBuildError> {
        if words.is_empty() {
            return Ok((0, 0));
        }
        let capacity = || AirBuildError {
            phase: "AIR",
            family,
            operation: "append",
            kind: AirBuildErrorKind::ResourceLimit,
        };
        let start = u32::try_from(self.extra.len()).map_err(|_| capacity())?;
        let extent = u32::try_from(words.len()).map_err(|_| capacity())?;
        self.extra
            .len()
            .checked_add(words.len())
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(capacity)?;
        self.extra
            .try_reserve(words.len())
            .map_err(|_| AirBuildError {
                phase: "AIR",
                family,
                operation: "reserve",
                kind: AirBuildErrorKind::ResourceExhaustion,
            })?;
        self.extra.extend_from_slice(words);
        Ok((start, extent))
    }

    fn append_encoded<T>(
        &mut self,
        family: &'static str,
        values: &[T],
        encode: impl Fn(&T) -> u32,
    ) -> Result<(u32, u32), AirBuildError> {
        if values.is_empty() {
            return Ok((0, 0));
        }
        let capacity = || AirBuildError {
            phase: "AIR",
            family,
            operation: "append",
            kind: AirBuildErrorKind::ResourceLimit,
        };
        let start = u32::try_from(self.extra.len()).map_err(|_| capacity())?;
        let extent = u32::try_from(values.len()).map_err(|_| capacity())?;
        self.extra
            .len()
            .checked_add(values.len())
            .and_then(|end| u32::try_from(end).ok())
            .ok_or_else(capacity)?;
        self.extra
            .try_reserve(values.len())
            .map_err(|_| AirBuildError {
                phase: "AIR",
                family,
                operation: "reserve",
                kind: AirBuildErrorKind::ResourceExhaustion,
            })?;
        self.extra.extend(values.iter().map(encode));
        Ok((start, extent))
    }

    fn add_call_args(&mut self, args: &[AirCallArg]) -> Result<AirCallArgs, AirBuildError> {
        self.preflight_refs("call arguments", args.iter().map(|arg| arg.value))?;
        if args.is_empty() {
            return Ok(AirCallArgs {
                start: 0,
                extent: 0,
            });
        }
        let overflow = || AirBuildError {
            phase: "AIR",
            family: "call arguments",
            operation: "append",
            kind: AirBuildErrorKind::ResourceLimit,
        };
        let word_count = args
            .len()
            .checked_mul(CallArgSchema::WIDTH)
            .ok_or_else(overflow)?;
        let start = u32::try_from(self.extra.len()).map_err(|_| overflow())?;
        let extent = u32::try_from(word_count).map_err(|_| overflow())?;
        self.extra
            .len()
            .checked_add(word_count)
            .and_then(|end| u32::try_from(end).ok())
            .ok_or_else(overflow)?;
        self.extra
            .try_reserve(word_count)
            .map_err(|_| AirBuildError {
                phase: "AIR",
                family: "call arguments",
                operation: "reserve",
                kind: AirBuildErrorKind::ResourceExhaustion,
            })?;
        self.extra
            .extend(args.iter().flat_map(CallArgSchema::encode));
        Ok(AirCallArgs { start, extent })
    }

    fn add_intrinsic_args(&mut self, args: &[AirRef]) -> Result<AirIntrinsicArgs, AirBuildError> {
        let (start, extent) =
            self.append_encoded("intrinsic arguments", args, |value| value.as_u32())?;
        Ok(AirIntrinsicArgs { start, extent })
    }

    fn add_block_statements(
        &mut self,
        values: &[AirRef],
    ) -> Result<AirBlockStatements, AirBuildError> {
        let (start, extent) =
            self.append_encoded("block statements", values, |value| value.as_u32())?;
        Ok(AirBlockStatements { start, extent })
    }

    fn add_struct_fields(&mut self, values: &[AirRef]) -> Result<AirStructFields, AirBuildError> {
        let (start, extent) =
            self.append_encoded("struct fields", values, |value| value.as_u32())?;
        Ok(AirStructFields { start, extent })
    }

    fn add_source_order(&mut self, values: &[u32]) -> Result<AirSourceOrder, AirBuildError> {
        let (start, extent) = self.append_words("source order", values)?;
        Ok(AirSourceOrder { start, extent })
    }

    fn add_array_elements(&mut self, values: &[AirRef]) -> Result<AirArrayElements, AirBuildError> {
        let (start, extent) =
            self.append_encoded("array elements", values, |value| value.as_u32())?;
        Ok(AirArrayElements { start, extent })
    }

    fn add_enum_payload(&mut self, values: &[AirRef]) -> Result<AirEnumPayload, AirBuildError> {
        let (start, extent) =
            self.append_encoded("enum payload", values, |value| value.as_u32())?;
        Ok(AirEnumPayload { start, extent })
    }

    fn add_type_args(&mut self, values: &[Type]) -> Result<AirTypeArgs, AirBuildError> {
        let (start, extent) =
            self.append_encoded("type arguments", values, |value| value.as_u32())?;
        Ok(AirTypeArgs { start, extent })
    }

    fn add_const_values(
        &mut self,
        values: &[crate::sema::ConstValue],
    ) -> Result<AirConstValueWords, AirBuildError> {
        let words = encode_const_values(values)?;
        let (start, extent) = self.append_words("constant value arguments", &words)?;
        Ok(AirConstValueWords { start, extent })
    }

    fn add_match_arms(
        &mut self,
        arms: &[(AirPattern, AirRef)],
        arm_count: u32,
    ) -> Result<AirMatchArms, AirBuildError> {
        self.preflight_refs("match arms", arms.iter().map(|(_, body)| *body))?;
        if arms.is_empty() {
            return Ok(AirMatchArms::EMPTY);
        }
        let word_count = arms.iter().try_fold(1usize, |count, (pattern, _)| {
            count.checked_add(pattern.tag().layout().width)
        });
        let word_count = word_count.ok_or(AirBuildError {
            phase: "AIR",
            family: "match arms",
            operation: "stage",
            kind: AirBuildErrorKind::ResourceLimit,
        })?;
        let start = u32::try_from(self.extra.len()).map_err(|_| AirBuildError {
            phase: "AIR",
            family: "match arms",
            operation: "append",
            kind: AirBuildErrorKind::ResourceLimit,
        })?;
        let extent = u32::try_from(word_count).map_err(|_| AirBuildError {
            phase: "AIR",
            family: "match arms",
            operation: "append",
            kind: AirBuildErrorKind::ResourceLimit,
        })?;
        self.extra
            .len()
            .checked_add(word_count)
            .and_then(|end| u32::try_from(end).ok())
            .ok_or(AirBuildError {
                phase: "AIR",
                family: "match arms",
                operation: "append",
                kind: AirBuildErrorKind::ResourceLimit,
            })?;
        self.extra
            .try_reserve(word_count)
            .map_err(|_| AirBuildError {
                phase: "AIR",
                family: "match arms",
                operation: "reserve",
                kind: AirBuildErrorKind::ResourceExhaustion,
            })?;
        let checkpoint = self.extra.len();
        self.extra.push(arm_count);
        for (pattern, body) in arms {
            if let Err(error) = pattern.encode(*body, &mut self.extra) {
                self.extra.truncate(checkpoint);
                return Err(error);
            }
        }
        Ok(AirMatchArms { start, extent })
    }

    pub(crate) fn add_match(
        &mut self,
        scrutinee: AirRef,
        arms: &[(AirPattern, AirRef)],
        ty: Type,
        span: Span,
    ) -> Result<AirRef, AirBuildError> {
        self.preflight_refs(
            "match arms",
            std::iter::once(scrutinee).chain(arms.iter().map(|(_, body)| *body)),
        )?;
        let arm_count = u32::try_from(arms.len()).map_err(|_| AirBuildError {
            phase: "AIR",
            family: "match arms",
            operation: "preflight",
            kind: AirBuildErrorKind::ResourceLimit,
        })?;
        self.reserve_instruction("match arms")?;
        let arms = self.add_match_arms(arms, arm_count)?;
        Ok(self.push_inst(AirInst {
            data: AirInstData::Match { scrutinee, arms },
            ty,
            span,
        }))
    }

    pub(crate) fn add_call(
        &mut self,
        runtime: Option<crate::RuntimeCallKind>,
        name: Spur,
        args: &[AirCallArg],
        ty: Type,
        span: Span,
    ) -> Result<AirRef, AirBuildError> {
        self.preflight_refs("call arguments", args.iter().map(|arg| arg.value))?;
        self.reserve_instruction("call arguments")?;
        let args = self.add_call_args(args)?;
        Ok(self.push_inst(AirInst {
            data: AirInstData::Call {
                runtime,
                name,
                args,
            },
            ty,
            span,
        }))
    }

    pub(crate) fn add_accessor_call(
        &mut self,
        name: Spur,
        args: &[AirCallArg],
        ty: Type,
        span: Span,
    ) -> Result<AirRef, AirBuildError> {
        self.preflight_refs("accessor call arguments", args.iter().map(|arg| arg.value))?;
        self.reserve_instruction("accessor call arguments")?;
        let args = self.add_call_args(args)?;
        Ok(self.push_inst(AirInst {
            data: AirInstData::AccessorCall { name, args },
            ty,
            span,
        }))
    }

    pub(crate) fn add_call_generic(
        &mut self,
        name: Spur,
        type_args: &[Type],
        value_args: &[crate::sema::ConstValue],
        args: &[AirCallArg],
        ty: Type,
        span: Span,
    ) -> Result<AirRef, AirBuildError> {
        self.preflight_refs("call arguments", args.iter().map(|arg| arg.value))?;
        self.reserve_instruction("generic call")?;
        let checkpoint = self.extra.len();
        let ranges = (|| {
            Ok::<_, AirBuildError>((
                self.add_type_args(type_args)?,
                self.add_const_values(value_args)?,
                self.add_call_args(args)?,
            ))
        })();
        let (type_args, value_args, args) = match ranges {
            Ok(ranges) => ranges,
            Err(error) => {
                self.extra.truncate(checkpoint);
                return Err(error);
            }
        };
        Ok(self.push_inst(AirInst {
            data: AirInstData::CallGeneric {
                name,
                type_args,
                value_args,
                args,
            },
            ty,
            span,
        }))
    }

    pub(crate) fn add_intrinsic(
        &mut self,
        runtime: Option<crate::RuntimeCallKind>,
        name: Spur,
        args: &[AirRef],
        ty: Type,
        span: Span,
    ) -> Result<AirRef, AirBuildError> {
        self.preflight_refs("intrinsic arguments", args.iter().copied())?;
        self.reserve_instruction("intrinsic arguments")?;
        let args = self.add_intrinsic_args(args)?;
        Ok(self.push_inst(AirInst {
            data: AirInstData::Intrinsic {
                runtime,
                name,
                args,
            },
            ty,
            span,
        }))
    }

    pub(crate) fn add_block(
        &mut self,
        statements: &[AirRef],
        value: AirRef,
        ty: Type,
        span: Span,
    ) -> Result<AirRef, AirBuildError> {
        self.preflight_refs(
            "block statements",
            statements.iter().copied().chain(std::iter::once(value)),
        )?;
        self.reserve_instruction("block statements")?;
        let statements = self.add_block_statements(statements)?;
        Ok(self.push_inst(AirInst {
            data: AirInstData::Block { statements, value },
            ty,
            span,
        }))
    }

    fn source_order_is_permutation(source_order: &[u32], field_count: usize) -> bool {
        Self::source_order_is_permutation_with_observer(source_order, field_count, || {})
    }

    #[inline(always)]
    fn source_order_is_permutation_with_observer(
        source_order: &[u32],
        field_count: usize,
        mut observe_field: impl FnMut(),
    ) -> bool {
        let mut seen = vec![false; field_count];
        for &field in source_order {
            observe_field();
            let field = field as usize;
            if field >= field_count || std::mem::replace(&mut seen[field], true) {
                return false;
            }
        }
        true
    }

    pub(crate) fn add_struct_init(
        &mut self,
        struct_id: StructId,
        fields: &[AirRef],
        source_order: &[u32],
        ty: Type,
        span: Span,
    ) -> Result<AirRef, AirBuildError> {
        if fields.len() != source_order.len() {
            return Err(AirBuildError {
                phase: "AIR",
                family: "struct initialization",
                operation: "stage",
                kind: AirBuildErrorKind::ProducerInvariant("field/source-order length mismatch"),
            });
        }
        if !Self::source_order_is_permutation(source_order, fields.len()) {
            return Err(AirBuildError {
                phase: "AIR",
                family: "struct initialization",
                operation: "preflight",
                kind: AirBuildErrorKind::ProducerInvariant("source order is not a permutation"),
            });
        }
        self.preflight_refs("struct fields", fields.iter().copied())?;
        self.reserve_instruction("struct initialization")?;
        let checkpoint = self.extra.len();
        let fields = self.add_struct_fields(fields)?;
        let source_order = match self.add_source_order(source_order) {
            Ok(source_order) => source_order,
            Err(error) => {
                self.extra.truncate(checkpoint);
                return Err(error);
            }
        };
        Ok(self.push_inst(AirInst {
            data: AirInstData::StructInit {
                struct_id,
                fields,
                source_order,
            },
            ty,
            span,
        }))
    }

    pub(crate) fn add_array_init(
        &mut self,
        elements: &[AirRef],
        ty: Type,
        span: Span,
    ) -> Result<AirRef, AirBuildError> {
        self.preflight_refs("array elements", elements.iter().copied())?;
        self.reserve_instruction("array elements")?;
        let elements = self.add_array_elements(elements)?;
        Ok(self.push_inst(AirInst {
            data: AirInstData::ArrayInit { elements },
            ty,
            span,
        }))
    }

    pub(crate) fn add_enum_variant(
        &mut self,
        enum_id: crate::EnumId,
        variant_index: u32,
        payload: &[AirRef],
        ty: Type,
        span: Span,
    ) -> Result<AirRef, AirBuildError> {
        self.preflight_refs("enum payload", payload.iter().copied())?;
        self.reserve_instruction("enum payload")?;
        let payload = self.add_enum_payload(payload)?;
        Ok(self.push_inst(AirInst {
            data: AirInstData::EnumVariant {
                enum_id,
                variant_index,
                payload,
            },
            ty,
            span,
        }))
    }
    /// Create a new empty AIR.
    pub(crate) fn new(return_type: Type) -> Self {
        Self {
            instructions: Vec::new(),
            extra: Vec::new(),
            return_type,
            projections: Vec::new(),
            places: Vec::new(),
            param_drops: Vec::new(),
            borrow_slots: Vec::new(),
            instruction_limit_exceeded: false,
            #[cfg(test)]
            place_reserve_failure: None,
        }
    }

    /// Record a local slot as a non-owning borrow binding (see `borrow_slots`).
    pub(crate) fn add_borrow_slot(&mut self, slot: u32) {
        if !self.borrow_slots.contains(&slot) {
            self.borrow_slots.push(slot);
        }
    }

    /// True when `slot` holds a non-owning borrow value that drop elaboration
    /// must not drop (see `borrow_slots`).
    #[inline]
    pub fn is_borrow_slot(&self, slot: u32) -> bool {
        self.borrow_slots.contains(&slot)
    }

    /// Add an instruction and return its reference.
    pub(crate) fn add_inst(&mut self, inst: AirInst) -> AirRef {
        assert!(
            !matches!(
                inst.data,
                AirInstData::Match { .. }
                    | AirInstData::Call { .. }
                    | AirInstData::CallGeneric { .. }
                    | AirInstData::Intrinsic { .. }
                    | AirInstData::Block { .. }
                    | AirInstData::StructInit { .. }
                    | AirInstData::ArrayInit { .. }
                    | AirInstData::EnumVariant { .. }
            ),
            "payload-bearing AIR nodes require an atomic owner builder"
        );
        self.push_inst(inst)
    }

    /// Preflight one instruction slot for a payload-bearing owner builder.
    ///
    /// Payload builders are already fallible, so they reject the published
    /// per-body ceiling directly instead of relying on `push_inst`'s latch. The
    /// admissible indices are `0..MAX_AIR_INSTRUCTIONS_PER_BODY`, which is
    /// exactly the range `push_inst` will accept.
    fn reserve_instruction(&mut self, family: &'static str) -> Result<(), AirBuildError> {
        let over_limit = match u32::try_from(self.instructions.len()) {
            Ok(index) => index == MAX_AIR_INSTRUCTIONS_PER_BODY,
            Err(_) => true,
        };
        if over_limit {
            self.instruction_limit_exceeded = true;
            return Err(AirBuildError {
                phase: "AIR",
                family,
                operation: "insert instruction",
                kind: AirBuildErrorKind::ResourceLimit,
            });
        }
        self.instructions.try_reserve(1).map_err(|_| AirBuildError {
            phase: "AIR",
            family,
            operation: "reserve instruction",
            kind: AirBuildErrorKind::ResourceExhaustion,
        })
    }

    fn preflight_refs(
        &self,
        family: &'static str,
        refs: impl IntoIterator<Item = AirRef>,
    ) -> Result<(), AirBuildError> {
        if refs
            .into_iter()
            .any(|value| value.as_u32() as usize >= self.instructions.len())
        {
            return Err(AirBuildError {
                phase: "AIR",
                family,
                operation: "preflight",
                kind: AirBuildErrorKind::ProducerInvariant(
                    "instruction reference is outside the current AIR owner",
                ),
            });
        }
        Ok(())
    }

    /// Append an instruction and return its reference.
    ///
    /// An `AirRef` is a `u32` index into this body's instruction array, so a
    /// body holds at most [`MAX_AIR_INSTRUCTIONS_PER_BODY`] instructions.
    /// Beyond that the reference is not representable: `len() as u32` would
    /// wrap instruction 2^32 onto instruction 0 and silently alias two
    /// distinct values, which spec C.1:2 forbids. This method has no fallible
    /// callers, so it latches [`Air::instruction_limit_exceeded`] and hands
    /// back an already-valid reference; [`Air::finish`] turns the latch into an
    /// `E1401` diagnostic before the body is published.
    fn push_inst(&mut self, inst: AirInst) -> AirRef {
        let over_limit = match u32::try_from(self.instructions.len()) {
            Ok(index) => index == MAX_AIR_INSTRUCTIONS_PER_BODY,
            Err(_) => true,
        };
        if over_limit {
            self.instruction_limit_exceeded = true;
            // The array is full, so index 0 is always a live instruction.
            return AirRef::from_raw(0);
        }
        let index = self.instructions.len() as u32;
        self.instructions.push(inst);
        AirRef::from_raw(index)
    }

    /// The implementation-limit rejection latched during construction, if this
    /// body ran past the published per-body instruction ceiling. Checked at the
    /// publication boundary ([`Air::finish`]).
    fn latched_capacity_error(&self) -> Option<AirValidationError> {
        self.instruction_limit_exceeded.then(|| AirValidationError {
            instruction: None,
            reason: format!(
                "this function body has more AIR instructions than the implementation limit \
                     of {MAX_AIR_INSTRUCTIONS_PER_BODY} per body — an AIR instruction reference \
                     is a u32 index into one body's instruction array (spec Appendix C.6:1)"
            ),
            kind: AirValidationErrorKind::ResourceLimit,
        })
    }

    /// Get an instruction by reference.
    #[inline]
    pub fn get(&self, inst_ref: AirRef) -> &AirInst {
        &self.instructions[inst_ref.0 as usize]
    }

    /// Set the owned-parameter drop list: (ABI slot, type) for each
    /// pass-by-value parameter that the callee owns (and must drop at exit
    /// unless moved out).
    pub(crate) fn set_param_drops(&mut self, param_drops: Vec<(u32, Type)>) {
        self.param_drops = param_drops;
    }

    /// Clear the owned-parameter drop list. Used for destructors: the
    /// destructor consumes `self`, and the drop glue (not the destructor
    /// itself) is responsible for dropping the fields afterwards, so the
    /// destructor must not re-drop its own parameter.
    pub(crate) fn clear_param_drops(&mut self) {
        self.param_drops.clear();
    }

    /// Owned (pass-by-value) parameters dropped by the callee at function
    /// exit: (ABI slot, type).
    #[inline]
    pub fn param_drops(&self) -> &[(u32, Type)] {
        &self.param_drops
    }

    /// Cancel a [`AirInstData::MarkMoved`] marker, turning it back into a
    /// plain use of the value. Used when a use that was initially treated as
    /// a move turns out to be a borrow (e.g. the receiver of a builtin query
    /// method like `String.len`). The marker instruction is rewritten in
    /// place to a copy of the wrapped instruction, preserving the value it
    /// produces. No-op when `inst_ref` is not a `MarkMoved`.
    pub(crate) fn cancel_move_marker(&mut self, inst_ref: AirRef) {
        if let AirInstData::MarkMoved { value, .. } = self.instructions[inst_ref.0 as usize].data {
            let replacement = match &self.instructions[value.0 as usize].data {
                AirInstData::Const(value) => AirInstData::Const(*value),
                AirInstData::BoolConst(value) => AirInstData::BoolConst(*value),
                AirInstData::StringConst(value) => AirInstData::StringConst(*value),
                AirInstData::UnitConst => AirInstData::UnitConst,
                AirInstData::TypeConst(value) => AirInstData::TypeConst(*value),
                AirInstData::Load { slot } => AirInstData::Load { slot: *slot },
                AirInstData::Param { index } => AirInstData::Param { index: *index },
                AirInstData::PlaceRead { place } => AirInstData::PlaceRead { place: *place },
                other => panic!("cannot cancel move marker around {other:?}"),
            };
            self.instructions[inst_ref.0 as usize].data = replacement;
        }
    }

    pub(crate) fn checkpoint(&self) -> AirCheckpoint {
        AirCheckpoint {
            instructions: self.instructions.len(),
            extra: self.extra.len(),
            projections: self.projections.len(),
            places: self.places.len(),
            param_drops: self.param_drops.len(),
            borrow_slots: self.borrow_slots.len(),
        }
    }

    pub(crate) fn rollback(&mut self, checkpoint: AirCheckpoint) {
        self.instructions.truncate(checkpoint.instructions);
        self.extra.truncate(checkpoint.extra);
        self.projections.truncate(checkpoint.projections);
        self.places.truncate(checkpoint.places);
        self.param_drops.truncate(checkpoint.param_drops);
        self.borrow_slots.truncate(checkpoint.borrow_slots);
    }

    /// The return type of this function.
    #[inline]
    pub fn return_type(&self) -> Type {
        self.return_type
    }

    /// The number of instructions.
    #[inline]
    pub fn len(&self) -> usize {
        self.instructions.len()
    }

    /// Whether there are no instructions.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.instructions.is_empty()
    }

    /// Iterate over all instructions with their references.
    pub fn iter(&self) -> impl Iterator<Item = (AirRef, &AirInst)> {
        self.instructions
            .iter()
            .enumerate()
            .map(|(i, inst)| (AirRef::from_raw(i as u32), inst))
    }

    /// Account for the physical payload stores without exposing offsets.
    #[doc(hidden)]
    pub fn payload_store_stats(&self) -> AirPayloadStorageStats {
        AirPayloadStorageStats {
            word_store_logical_bytes: self.extra.len() * std::mem::size_of::<u32>(),
            word_store_capacity_bytes: self.extra.capacity() * std::mem::size_of::<u32>(),
            projection_store_logical_bytes: self.projections.len()
                * std::mem::size_of::<AirProjection>(),
            projection_store_capacity_bytes: self.projections.capacity()
                * std::mem::size_of::<AirProjection>(),
            place_store_logical_bytes: self.places.len() * std::mem::size_of::<AirPlace>(),
            place_store_capacity_bytes: self.places.capacity() * std::mem::size_of::<AirPlace>(),
            ..AirPayloadStorageStats::default()
        }
    }

    fn try_get_words(
        &self,
        start: u32,
        extent: u32,
        family: &'static str,
    ) -> Result<&[u32], AirPayloadError> {
        if extent == 0 {
            if start == 0 {
                return Ok(&[]);
            }
            return Err(AirPayloadError::decode(
                family,
                start,
                extent,
                0,
                0,
                0,
                "non-canonical empty range",
            ));
        }
        let raw_start = start;
        let start = start as usize;
        let end = start.checked_add(extent as usize).ok_or_else(|| {
            AirPayloadError::decode(
                family,
                raw_start,
                extent,
                0,
                extent as usize,
                0,
                "range end overflow",
            )
        })?;
        self.extra.get(start..end).ok_or_else(|| {
            AirPayloadError::decode(
                family,
                raw_start,
                extent,
                0,
                extent as usize,
                self.extra.len().saturating_sub(start).min(extent as usize),
                "range outside word store",
            )
        })
    }

    #[cfg(any(test, feature = "fuzz-support"))]
    fn try_get_refs(
        &self,
        start: u32,
        extent: u32,
        family: &'static str,
    ) -> Result<&[u32], AirPayloadError> {
        let words = self.try_get_words(start, extent, family)?;
        for (record, word) in words.iter().copied().enumerate() {
            if word as usize >= self.instructions.len() {
                return Err(AirPayloadError::decode(
                    family,
                    start,
                    extent,
                    record,
                    1,
                    1,
                    "instruction reference is outside the owner",
                ));
            }
        }
        Ok(words)
    }

    #[cfg(any(test, feature = "fuzz-support"))]
    fn try_get_types(&self, range: &AirTypeArgs) -> Result<&[u32], AirPayloadError> {
        let words = self.try_get_words(range.start, range.extent, "type arguments")?;
        for (record, word) in words.iter().copied().enumerate() {
            if Type::try_from_u32(word).is_none() {
                return Err(AirPayloadError::decode(
                    "type arguments",
                    range.start,
                    range.extent,
                    record,
                    1,
                    1,
                    "invalid type encoding",
                ));
            }
        }
        Ok(words)
    }

    /// Get call arguments from extra array.
    /// Each call arg is encoded as 2 u32s: (air_ref, mode).
    #[inline]
    pub fn get_call_args(
        &self,
        range: &AirCallArgs,
    ) -> impl ExactSizeIterator<Item = AirCallArg> + '_ {
        self.try_get_call_args(range)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    pub fn try_get_call_args(
        &self,
        range: &AirCallArgs,
    ) -> Result<impl ExactSizeIterator<Item = AirCallArg> + '_, AirPayloadError> {
        let data = self.validate_call_args(range)?;
        let range = AirCallArgs {
            start: range.start,
            extent: range.extent,
        };
        Ok(data
            .chunks_exact(CallArgSchema::WIDTH)
            .enumerate()
            .map(move |(record, chunk)| {
                CallArgSchema::decode(chunk, &range, record).expect("validated call argument")
            }))
    }

    pub fn get_intrinsic_args(
        &self,
        range: &AirIntrinsicArgs,
    ) -> impl ExactSizeIterator<Item = AirRef> + '_ {
        self.try_get_words(range.start, range.extent, "intrinsic arguments")
            .unwrap_or_else(|e| panic!("{e}"))
            .iter()
            .copied()
            .map(AirRef::from_raw)
    }

    pub fn get_block_statements(
        &self,
        range: &AirBlockStatements,
    ) -> impl ExactSizeIterator<Item = AirRef> + '_ {
        self.try_get_words(range.start, range.extent, "block statements")
            .unwrap_or_else(|e| panic!("{e}"))
            .iter()
            .copied()
            .map(AirRef::from_raw)
    }

    pub fn get_struct_fields(
        &self,
        range: &AirStructFields,
    ) -> impl ExactSizeIterator<Item = AirRef> + '_ {
        self.try_get_words(range.start, range.extent, "struct fields")
            .unwrap_or_else(|e| panic!("{e}"))
            .iter()
            .copied()
            .map(AirRef::from_raw)
    }

    pub fn get_source_order(
        &self,
        range: &AirSourceOrder,
    ) -> impl ExactSizeIterator<Item = usize> + '_ {
        self.try_get_words(range.start, range.extent, "source order")
            .unwrap_or_else(|e| panic!("{e}"))
            .iter()
            .map(|&v| v as usize)
    }

    pub fn get_array_elements(
        &self,
        range: &AirArrayElements,
    ) -> impl ExactSizeIterator<Item = AirRef> + '_ {
        self.try_get_words(range.start, range.extent, "array elements")
            .unwrap_or_else(|e| panic!("{e}"))
            .iter()
            .copied()
            .map(AirRef::from_raw)
    }

    pub fn get_enum_payload(
        &self,
        range: &AirEnumPayload,
    ) -> impl ExactSizeIterator<Item = AirRef> + '_ {
        self.try_get_words(range.start, range.extent, "enum payload")
            .unwrap_or_else(|e| panic!("{e}"))
            .iter()
            .copied()
            .map(AirRef::from_raw)
    }

    pub fn get_type_args(&self, range: &AirTypeArgs) -> impl ExactSizeIterator<Item = Type> + '_ {
        self.try_get_words(range.start, range.extent, "type arguments")
            .unwrap_or_else(|e| panic!("{e}"))
            .iter()
            .map(|&v| Type::try_from_u32(v).expect("validated AIR type argument"))
    }

    pub fn get_const_values(
        &self,
        range: &AirConstValueWords,
    ) -> impl ExactSizeIterator<Item = crate::sema::ConstValue> + '_ {
        self.try_get_const_values(range)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    pub fn try_get_const_values(
        &self,
        range: &AirConstValueWords,
    ) -> Result<ConstValueIterator<'_>, AirPayloadError> {
        let words = self.try_get_words(range.start, range.extent, "constant value arguments")?;
        let mut cursor = words;
        let mut count = 0usize;
        while let Some((&tag_word, rest)) = cursor.split_first() {
            let tag = ConstValueTag::from_word(tag_word).ok_or_else(|| {
                AirPayloadError::decode(
                    "constant value arguments",
                    range.start,
                    range.extent,
                    count,
                    1,
                    1,
                    "invalid constant tag",
                )
            })?;
            let payload_width = tag.payload_width();
            let payload = rest.get(..payload_width).ok_or_else(|| {
                AirPayloadError::decode(
                    "constant value arguments",
                    range.start,
                    range.extent,
                    count,
                    1 + payload_width,
                    1 + rest.len().min(payload_width),
                    "truncated constant payload",
                )
            })?;
            match tag {
                ConstValueTag::Bool if !matches!(payload[0], 0 | 1) => {
                    return Err(AirPayloadError::decode(
                        "constant value arguments",
                        range.start,
                        range.extent,
                        count,
                        1 + payload_width,
                        1 + payload_width,
                        "invalid boolean",
                    ));
                }
                ConstValueTag::Type if Type::try_from_u32(payload[0]).is_none() => {
                    return Err(AirPayloadError::decode(
                        "constant value arguments",
                        range.start,
                        range.extent,
                        count,
                        1 + payload_width,
                        1 + payload_width,
                        "invalid type encoding",
                    ));
                }
                ConstValueTag::Function if Spur::try_from_usize(payload[0] as usize).is_none() => {
                    return Err(AirPayloadError::decode(
                        "constant value arguments",
                        range.start,
                        range.extent,
                        count,
                        1 + payload_width,
                        1 + payload_width,
                        "invalid function symbol encoding",
                    ));
                }
                _ => {}
            }
            cursor = &rest[tag.payload_width()..];
            count += 1;
        }
        Ok(ConstValueIterator {
            words,
            remaining: count,
        })
    }

    fn validate_call_args(&self, range: &AirCallArgs) -> Result<&[u32], AirPayloadError> {
        if range.extent as usize % CallArgSchema::WIDTH != 0 {
            return Err(AirPayloadError::decode(
                "call arguments",
                range.start,
                range.extent,
                range.extent as usize / CallArgSchema::WIDTH,
                CallArgSchema::WIDTH,
                range.extent as usize % CallArgSchema::WIDTH,
                "partial fixed-width record",
            ));
        }
        let data = self.try_get_words(range.start, range.extent, "call arguments")?;
        for (record, chunk) in data.chunks_exact(CallArgSchema::WIDTH).enumerate() {
            CallArgSchema::decode(chunk, range, record)?;
        }
        Ok(data)
    }

    fn validate_match_arms(
        &self,
        range: &AirMatchArms,
    ) -> Result<(&[u32], usize), AirPayloadError> {
        if range.is_empty() {
            return Ok((&[], 0));
        }
        let bounded = self.try_get_words(range.start, range.extent, "match arms")?;
        let (&count, data) = bounded.split_first().ok_or_else(|| {
            AirPayloadError::decode(
                "match arms",
                range.start,
                range.extent,
                0,
                1,
                0,
                "missing envelope",
            )
        })?;
        let count = usize::try_from(count).map_err(|_| {
            AirPayloadError::decode(
                "match arms",
                range.start,
                range.extent,
                0,
                1,
                1,
                "record count exceeds addressable memory",
            )
        })?;
        let mut cursor = data;
        for record in 0..count {
            let tag = match cursor
                .get(PatternTag::TAG_OFFSET)
                .copied()
                .and_then(PatternTag::from_word)
            {
                Some(tag) => tag,
                None if cursor.is_empty() => {
                    return Err(AirPayloadError::decode(
                        "match arms",
                        range.start,
                        range.extent,
                        record,
                        1,
                        0,
                        "truncated record",
                    ));
                }
                None => {
                    return Err(AirPayloadError::decode(
                        "match arms",
                        range.start,
                        range.extent,
                        record,
                        1,
                        1,
                        "invalid pattern tag",
                    ));
                }
            };
            let layout = tag.layout();
            let record_words = cursor.get(..layout.width).ok_or_else(|| {
                AirPayloadError::decode(
                    "match arms",
                    range.start,
                    range.extent,
                    record,
                    layout.width,
                    cursor.len().min(layout.width),
                    "truncated record",
                )
            })?;
            if let PatternFields::Bool { value } = layout.fields {
                if !matches!(record_words[value], 0 | 1) {
                    return Err(AirPayloadError::decode(
                        "match arms",
                        range.start,
                        range.extent,
                        record,
                        layout.width,
                        layout.width,
                        "invalid boolean",
                    ));
                }
            }
            cursor = &cursor[layout.width..];
        }
        if !cursor.is_empty() {
            return Err(AirPayloadError::decode(
                "match arms",
                range.start,
                range.extent,
                count,
                0,
                cursor.len(),
                "trailing words",
            ));
        }
        Ok((data, count))
    }

    /// Get match arms from extra array.
    /// Each match arm is encoded based on pattern type plus the body AirRef.
    #[inline]
    pub fn get_match_arms(
        &self,
        range: &AirMatchArms,
    ) -> impl ExactSizeIterator<Item = (AirPattern, AirRef)> + '_ {
        self.try_get_match_arms(range)
            .unwrap_or_else(|error| panic!("{error}"))
    }

    pub fn try_get_match_arms(
        &self,
        range: &AirMatchArms,
    ) -> Result<impl ExactSizeIterator<Item = (AirPattern, AirRef)> + '_, AirPayloadError> {
        let (data, count) = self.validate_match_arms(range)?;
        Ok(MatchArmIterator {
            data,
            remaining: count,
        })
    }

    /// Get a reference to all instructions.
    #[inline]
    pub fn instructions(&self) -> &[AirInst] {
        &self.instructions
    }

    pub(crate) fn rewrite_generic_call_to_call(&mut self, index: usize, name: Spur) {
        let args = match &self.instructions[index].data {
            AirInstData::CallGeneric { args, .. } => AirCallArgs {
                start: args.start,
                extent: args.extent,
            },
            _ => panic!("AIR rewrite target is not a generic call"),
        };
        self.instructions[index].data = AirInstData::Call {
            runtime: None,
            name,
            args,
        };
    }

    // ========================================================================
    // Place operations
    // ========================================================================

    /// Get projections for a place.
    #[inline]
    pub fn get_place_projections(&self, place: &AirPlace) -> &[AirProjection] {
        if place.projections.is_empty() {
            return &[];
        }
        let start = place.projections.start as usize;
        let end = start
            .checked_add(place.projections.extent as usize)
            .expect("corrupt AIR projection range overflow");
        self.projections
            .get(start..end)
            .expect("corrupt AIR projection range outside its owner")
    }

    /// Create a place with the given base and projections.
    ///
    /// This adds the projections to the projections array and returns a PlaceRef
    /// that references the place.
    pub(crate) fn make_place(
        &mut self,
        base: AirPlaceBase,
        base_type: Type,
        projs: impl IntoIterator<Item = AirProjection>,
    ) -> Result<AirPlaceRef, AirBuildError> {
        // Materialize and validate the entire owner locally before either
        // owner store is touched.
        let projs = projs.into_iter();
        let mut staged = Vec::new();
        let (lower, _) = projs.size_hint();
        staged.try_reserve(lower).map_err(|_| AirBuildError {
            phase: "AIR",
            family: "place projections",
            operation: "stage",
            kind: AirBuildErrorKind::ResourceExhaustion,
        })?;
        for projection in projs {
            if let AirProjection::Index { index, .. } = projection {
                if index.as_u32() as usize >= self.instructions.len() {
                    return Err(AirBuildError {
                        phase: "AIR",
                        family: "place projections",
                        operation: "preflight",
                        kind: AirBuildErrorKind::ProducerInvariant(
                            "instruction reference is outside the current AIR owner",
                        ),
                    });
                }
            }
            if staged.len() == staged.capacity() {
                staged.try_reserve(1).map_err(|_| AirBuildError {
                    phase: "AIR",
                    family: "place projections",
                    operation: "stage",
                    kind: AirBuildErrorKind::ResourceExhaustion,
                })?;
            }
            staged.push(projection);
        }

        let projection_limit = || AirBuildError {
            phase: "AIR",
            family: "place projections",
            operation: "preflight",
            kind: AirBuildErrorKind::ResourceLimit,
        };
        let projection_start =
            u32::try_from(self.projections.len()).map_err(|_| projection_limit())?;
        let projection_extent = u32::try_from(staged.len()).map_err(|_| projection_limit())?;
        self.projections
            .len()
            .checked_add(staged.len())
            .and_then(|end| u32::try_from(end).ok())
            .ok_or_else(projection_limit)?;
        let index = u32::try_from(self.places.len()).map_err(|_| AirBuildError {
            phase: "AIR",
            family: "places",
            operation: "preflight",
            kind: AirBuildErrorKind::ResourceLimit,
        })?;
        self.places
            .len()
            .checked_add(1)
            .and_then(|end| u32::try_from(end).ok())
            .ok_or(AirBuildError {
                phase: "AIR",
                family: "places",
                operation: "preflight",
                kind: AirBuildErrorKind::ResourceLimit,
            })?;

        #[cfg(test)]
        if self.place_reserve_failure == Some(PlaceReserveFailure::ProjectionStore) {
            self.place_reserve_failure = None;
            return Err(AirBuildError {
                phase: "AIR",
                family: "place projections",
                operation: "reserve",
                kind: AirBuildErrorKind::ResourceExhaustion,
            });
        }
        self.projections
            .try_reserve(staged.len())
            .map_err(|_| AirBuildError {
                phase: "AIR",
                family: "place projections",
                operation: "reserve",
                kind: AirBuildErrorKind::ResourceExhaustion,
            })?;
        #[cfg(test)]
        if self.place_reserve_failure == Some(PlaceReserveFailure::PlaceStore) {
            self.place_reserve_failure = None;
            return Err(AirBuildError {
                phase: "AIR",
                family: "places",
                operation: "reserve",
                kind: AirBuildErrorKind::ResourceExhaustion,
            });
        }
        self.places.try_reserve(1).map_err(|_| AirBuildError {
            phase: "AIR",
            family: "places",
            operation: "reserve",
            kind: AirBuildErrorKind::ResourceExhaustion,
        })?;
        let projections = if staged.is_empty() {
            AirProjectionRange::EMPTY
        } else {
            AirProjectionRange {
                start: projection_start,
                extent: projection_extent,
                family: PhantomData,
            }
        };
        // Both stores now have sufficient capacity; these commits cannot fail.
        self.projections.extend(staged);
        let place = AirPlace {
            base,
            base_type,
            projections,
        };
        self.places.push(place);
        Ok(AirPlaceRef::from_raw(index))
    }

    #[cfg(test)]
    fn fail_next_place_reserve(&mut self, failure: PlaceReserveFailure) {
        self.place_reserve_failure = Some(failure);
    }

    /// Get a place by reference.
    #[inline]
    pub fn get_place(&self, place_ref: AirPlaceRef) -> &AirPlace {
        &self.places[place_ref.0 as usize]
    }

    /// Get all places.
    #[inline]
    pub fn places(&self) -> &[AirPlace] {
        &self.places
    }

    #[cfg(test)]
    pub(crate) fn projections(&self) -> &[AirProjection] {
        &self.projections
    }
}

/// A single AIR instruction.
#[derive(Debug)]
pub struct AirInst {
    pub data: AirInstData,
    pub ty: Type,
    pub span: Span,
}

/// AIR instruction data - fully typed operations.
#[derive(Debug)]
pub enum AirInstData {
    /// Integer constant (typed)
    Const(u64),

    /// Boolean constant
    BoolConst(bool),

    /// String constant (index into string table)
    StringConst(u32),

    /// Unit constant
    UnitConst,

    /// Type constant - a compile-time type value.
    /// This is used for comptime type parameters (e.g., passing `i32` to `fn foo(comptime T: type)`).
    /// The contained Type is the type being passed as a value.
    /// This instruction has type `Type::COMPTIME_TYPE` and is erased during specialization.
    TypeConst(crate::Type),

    // Binary arithmetic operations
    /// Addition
    Add(AirRef, AirRef),
    /// Subtraction
    Sub(AirRef, AirRef),
    /// Multiplication
    Mul(AirRef, AirRef),
    /// Wrapping addition — two's-complement result mod 2^N, never traps
    /// (`@wrapping_add`, RUE-647).
    WrappingAdd(AirRef, AirRef),
    /// Wrapping subtraction — two's-complement result mod 2^N, never traps
    /// (`@wrapping_sub`, RUE-647).
    WrappingSub(AirRef, AirRef),
    /// Wrapping multiplication — two's-complement result mod 2^N, never traps
    /// (`@wrapping_mul`, RUE-647).
    WrappingMul(AirRef, AirRef),
    /// Division
    Div(AirRef, AirRef),
    /// Modulo
    Mod(AirRef, AirRef),

    // Comparison operations (return bool)
    /// Equality
    Eq(AirRef, AirRef),
    /// Inequality
    Ne(AirRef, AirRef),
    /// Less than
    Lt(AirRef, AirRef),
    /// Greater than
    Gt(AirRef, AirRef),
    /// Less than or equal
    Le(AirRef, AirRef),
    /// Greater than or equal
    Ge(AirRef, AirRef),

    // Logical operations (return bool)
    /// Logical AND
    And(AirRef, AirRef),
    /// Logical OR
    Or(AirRef, AirRef),

    // Bitwise operations
    /// Bitwise AND
    BitAnd(AirRef, AirRef),
    /// Bitwise OR
    BitOr(AirRef, AirRef),
    /// Bitwise XOR
    BitXor(AirRef, AirRef),
    /// Left shift
    Shl(AirRef, AirRef),
    /// Right shift (arithmetic for signed, logical for unsigned)
    Shr(AirRef, AirRef),

    // Unary operations
    /// Negation
    Neg(AirRef),
    /// Logical NOT
    Not(AirRef),
    /// Bitwise NOT
    BitNot(AirRef),

    // Control flow
    /// Conditional branch
    Branch {
        cond: AirRef,
        then_value: AirRef,
        else_value: Option<AirRef>,
    },

    /// While loop
    Loop { cond: AirRef, body: AirRef },

    /// Infinite loop (produces Never type)
    InfiniteLoop { body: AirRef },

    /// Match expression
    Match {
        /// The value being matched (scrutinee)
        scrutinee: AirRef,
        /// Start index into extra array for match arms
        arms: AirMatchArms,
    },

    /// Break: exits the innermost loop
    Break,

    /// Continue: jumps to the next iteration of the innermost loop
    Continue,

    // Variable operations
    /// Allocate local variable with initial value
    /// Returns the slot index
    Alloc {
        /// Local variable slot index (0, 1, 2, ...)
        slot: u32,
        /// Initial value
        init: AirRef,
    },

    /// Load value from local variable
    Load {
        /// Local variable slot index
        slot: u32,
    },

    /// Store value to local variable
    Store {
        /// Local variable slot index
        slot: u32,
        /// Value to store
        value: AirRef,
    },

    /// Store value to a parameter (for inout params)
    ParamStore {
        /// Parameter's ABI slot (relative to params, not locals)
        param_slot: u32,
        /// Value to store
        value: AirRef,
    },

    /// Return from function (None for `return;` in unit-returning functions)
    Ret(Option<AirRef>),

    /// Function call
    Call {
        /// Typed runtime identity, absent for source-language calls.
        runtime: Option<crate::RuntimeCallKind>,
        /// Function name (interned symbol)
        name: Spur,
        /// Start index into extra array for arguments
        args: AirCallArgs,
    },

    /// Mandatory-inline place-producing accessor call (ADR-0062/RUE-1208).
    /// Its result may only be used as [`AirPlaceBase::Accessor`].
    AccessorCall { name: Spur, args: AirCallArgs },

    /// Generic function call - requires specialization before codegen.
    ///
    /// This is emitted when calling a function with `comptime` parameters
    /// (type parameters like `comptime T: type` or value parameters like
    /// `comptime n: i32`, RUE-166). During a post-analysis specialization
    /// pass, this is rewritten to a regular `Call` to a specialized version
    /// of the function (e.g., `identity.i32`, `fact.v5`; the `.` separator is
    /// illegal in Rue identifiers so specialized names can't collide with a
    /// user-spellable function name, RUE-41).
    ///
    /// The type_args are encoded in the extra array as raw Type discriminant
    /// values. The comptime value arguments are encoded in the extra array as
    /// a tagged word stream (see `specialize::encode_const_values`). The
    /// runtime args are also in the extra array; comptime value arguments
    /// remain part of the runtime argument list (they are still passed at
    /// runtime), while type arguments are erased.
    CallGeneric {
        /// Base function name (interned symbol)
        name: Spur,
        /// Start index into extra array for type arguments (raw Type values)
        type_args: AirTypeArgs,
        /// Start index into extra array for encoded comptime value arguments
        value_args: AirConstValueWords,
        /// Start index into extra array for runtime arguments
        args: AirCallArgs,
    },

    /// Intrinsic call (e.g., @dbg)
    Intrinsic {
        /// Runtime helper/adaptation selected for runtime-backed intrinsics.
        runtime: Option<crate::RuntimeCallKind>,
        /// Intrinsic name (without @, interned)
        name: Spur,
        /// Start index into extra array for arguments
        args: AirIntrinsicArgs,
    },

    /// Reference to a function parameter
    Param {
        /// Parameter index (0-based)
        index: u32,
    },

    /// Block expression with statements and final value.
    /// Used to group side-effect statements with their result value,
    /// enabling demand-driven lowering for short-circuit evaluation.
    Block {
        /// Start index into extra array for statement refs
        statements: AirBlockStatements,
        /// The block's resulting value
        value: AirRef,
    },

    // Struct operations
    /// Create a new struct instance with initialized fields
    StructInit {
        /// The struct type being created
        struct_id: StructId,
        /// Start index into extra array for field refs (in declaration order)
        fields: AirStructFields,
        /// Start index into extra array for source order indices
        /// Each entry is an index into fields, specifying evaluation order
        source_order: AirSourceOrder,
    },

    // Array operations
    /// Create a new array with initialized elements.
    /// The array type is stored in `AirInst.ty` as `Type::new_array(...)`.
    ArrayInit {
        /// Start index into extra array for element refs
        elements: AirArrayElements,
    },

    // Place operations
    /// Read a value from a memory location.
    ///
    /// Projected memory reads are represented canonically by this instruction,
    /// including arbitrarily nested access patterns like `arr[i].field`.
    PlaceRead {
        /// Reference to the place to read from
        place: AirPlaceRef,
    },

    /// Write a value to a memory location.
    ///
    /// Projected memory writes are represented canonically by this instruction,
    /// including nested local and parameter writes.
    PlaceWrite {
        /// Reference to the place to write to
        place: AirPlaceRef,
        /// Value to write
        value: AirRef,
    },

    // Enum operations
    /// Create an enum variant value
    EnumVariant {
        /// The enum type ID
        enum_id: crate::types::EnumId,
        /// The variant index (0-based)
        variant_index: u32,
        /// Start index into the extra array for payload operand `AirRef`s, in
        /// payload declaration order (RUE-221). Zero-length for a
        /// discriminant-only variant.
        payload: AirEnumPayload,
    },

    /// Read payload field `field_index` of a tuple variant from an enum value
    /// (RUE-221). Materialized when lowering a match arm whose pattern binds
    /// the payload (`Circle(r)`); the discriminant is assumed to match the
    /// variant (the enclosing `match` dispatched on it). The result type is
    /// the payload field's type.
    EnumPayloadGet {
        /// The enum value being read.
        base: AirRef,
        /// The enum type ID.
        enum_id: crate::types::EnumId,
        /// The variant whose payload layout applies.
        variant_index: u32,
        /// The payload field index (0-based).
        field_index: u32,
    },

    // Type conversion operations
    /// Integer cast: convert between integer types with runtime range check.
    /// Panics if the value cannot be represented in the target type.
    /// The target type is stored in AirInst.ty.
    IntCast {
        /// The value to cast
        value: AirRef,
        /// The source type (for determining signedness and size)
        from_ty: Type,
    },

    // Drop/destructor operations
    /// Drop a value, running its destructor if the type has one.
    /// For trivially droppable types, this is a no-op.
    /// The type is stored in the AirInst.ty field.
    Drop {
        /// The value to drop
        value: AirRef,
    },

    // Storage liveness operations (for drop elaboration)
    /// Marks that a local slot becomes live (storage allocated).
    /// Emitted when a variable binding is created.
    /// The type is stored in AirInst.ty for drop elaboration.
    StorageLive {
        /// The slot that becomes live
        slot: u32,
    },

    /// Marks that a local slot becomes dead (storage can be deallocated).
    /// Emitted at scope exit for variables declared in that scope.
    /// The type is stored in AirInst.ty for drop elaboration.
    /// Drop elaboration will insert a Drop before this if the type needs drop
    /// and the value wasn't moved.
    StorageDead {
        /// The slot that becomes dead
        slot: u32,
    },

    /// Marks that the value produced by `value` was moved out of its home
    /// slot on this control-flow path (passed by value to a call, bound to
    /// another variable, returned, ...). A pure passthrough at runtime: it
    /// produces the same value as `value`. Drop elaboration in the CFG
    /// builder uses these markers to suppress the scope-exit drop of slots
    /// whose contents were moved out (the new owner is responsible for the
    /// drop). Emitted by sema wherever the move checker marks a whole
    /// variable as moved, and — with `place` set — wherever it marks a
    /// struct field path (a partial move, RUE-62/RUE-157) or a
    /// constant-index array element (RUE-186) as moved.
    MarkMoved {
        /// The use of the variable (or field/element) that constitutes the move
        value: AirRef,
        /// The local slot (or parameter ABI slot when `is_param`) moved from
        slot: u32,
        /// True when `slot` refers to a parameter ABI slot
        is_param: bool,
        /// `None`: the slot's whole value moved out. `Some(place)`: only
        /// the path named by the place's projections moved out — a pure
        /// `Field`/constant-`Index` projection chain of any depth (`o.a`,
        /// `o.a.b`, `xs[K]`, `xs[K].a`); every index operand in the marker is a
        /// dedicated `Const` instruction so drop elaboration can decode the
        /// path. Drop elaboration then drops the struct/array path-granularly,
        /// skipping the moved path.
        place: Option<AirPlaceRef>,
    },
}

impl fmt::Display for AirRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

/// Interner-aware AIR display adapter for stable, human-readable symbols.
pub struct AirDisplay<'a> {
    air: &'a Air,
    interner: &'a ThreadedRodeo,
}

impl Air {
    /// Display AIR with interned call and intrinsic symbols resolved to names.
    pub fn display_with_interner<'a>(&'a self, interner: &'a ThreadedRodeo) -> AirDisplay<'a> {
        AirDisplay {
            air: self,
            interner,
        }
    }

    fn fmt_with_interner(
        &self,
        f: &mut fmt::Formatter<'_>,
        interner: Option<&ThreadedRodeo>,
    ) -> fmt::Result {
        writeln!(f, "air (return_type: {}) {{", self.return_type.name())?;
        for (inst_ref, inst) in self.iter() {
            write!(f, "    {} : {} = ", inst_ref, inst.ty.name())?;
            match &inst.data {
                AirInstData::Const(v) => writeln!(f, "const {}", v)?,
                AirInstData::BoolConst(v) => writeln!(f, "const {}", v)?,
                AirInstData::StringConst(idx) => writeln!(f, "string_const @{}", idx)?,
                AirInstData::UnitConst => writeln!(f, "const ()")?,
                AirInstData::TypeConst(ty) => writeln!(f, "type_const {}", ty.name())?,
                AirInstData::Add(lhs, rhs) => writeln!(f, "add {}, {}", lhs, rhs)?,
                AirInstData::Sub(lhs, rhs) => writeln!(f, "sub {}, {}", lhs, rhs)?,
                AirInstData::Mul(lhs, rhs) => writeln!(f, "mul {}, {}", lhs, rhs)?,
                AirInstData::WrappingAdd(lhs, rhs) => writeln!(f, "wrapping_add {}, {}", lhs, rhs)?,
                AirInstData::WrappingSub(lhs, rhs) => writeln!(f, "wrapping_sub {}, {}", lhs, rhs)?,
                AirInstData::WrappingMul(lhs, rhs) => writeln!(f, "wrapping_mul {}, {}", lhs, rhs)?,
                AirInstData::Div(lhs, rhs) => writeln!(f, "div {}, {}", lhs, rhs)?,
                AirInstData::Mod(lhs, rhs) => writeln!(f, "mod {}, {}", lhs, rhs)?,
                AirInstData::Eq(lhs, rhs) => writeln!(f, "eq {}, {}", lhs, rhs)?,
                AirInstData::Ne(lhs, rhs) => writeln!(f, "ne {}, {}", lhs, rhs)?,
                AirInstData::Lt(lhs, rhs) => writeln!(f, "lt {}, {}", lhs, rhs)?,
                AirInstData::Gt(lhs, rhs) => writeln!(f, "gt {}, {}", lhs, rhs)?,
                AirInstData::Le(lhs, rhs) => writeln!(f, "le {}, {}", lhs, rhs)?,
                AirInstData::Ge(lhs, rhs) => writeln!(f, "ge {}, {}", lhs, rhs)?,
                AirInstData::And(lhs, rhs) => writeln!(f, "and {}, {}", lhs, rhs)?,
                AirInstData::Or(lhs, rhs) => writeln!(f, "or {}, {}", lhs, rhs)?,
                AirInstData::BitAnd(lhs, rhs) => writeln!(f, "bit_and {}, {}", lhs, rhs)?,
                AirInstData::BitOr(lhs, rhs) => writeln!(f, "bit_or {}, {}", lhs, rhs)?,
                AirInstData::BitXor(lhs, rhs) => writeln!(f, "bit_xor {}, {}", lhs, rhs)?,
                AirInstData::Shl(lhs, rhs) => writeln!(f, "shl {}, {}", lhs, rhs)?,
                AirInstData::Shr(lhs, rhs) => writeln!(f, "shr {}, {}", lhs, rhs)?,
                AirInstData::Neg(operand) => writeln!(f, "neg {}", operand)?,
                AirInstData::Not(operand) => writeln!(f, "not {}", operand)?,
                AirInstData::BitNot(operand) => writeln!(f, "bit_not {}", operand)?,
                AirInstData::Branch {
                    cond,
                    then_value,
                    else_value,
                } => {
                    if let Some(else_v) = else_value {
                        writeln!(f, "branch {}, {}, {}", cond, then_value, else_v)?
                    } else {
                        writeln!(f, "branch {}, {}", cond, then_value)?
                    }
                }
                AirInstData::Loop { cond, body } => writeln!(f, "loop {}, {}", cond, body)?,
                AirInstData::InfiniteLoop { body } => writeln!(f, "infinite_loop {}", body)?,
                AirInstData::Match { scrutinee, arms } => {
                    write!(f, "match {} {{ ", scrutinee)?;
                    for (i, (pat, body)) in self.get_match_arms(arms).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        let pat_str = match pat {
                            AirPattern::Wildcard => "_".to_string(),
                            AirPattern::Int(n) => n.to_string(),
                            AirPattern::Bool(b) => b.to_string(),
                            AirPattern::EnumVariant {
                                enum_id,
                                variant_index,
                            } => format!("enum#{enum_id:?}::{variant_index}"),
                        };
                        write!(f, "{} => {}", pat_str, body)?;
                    }
                    writeln!(f, " }}")?;
                }
                AirInstData::Break => writeln!(f, "break")?,
                AirInstData::Continue => writeln!(f, "continue")?,
                AirInstData::Alloc { slot, init } => writeln!(f, "alloc ${} = {}", slot, init)?,
                AirInstData::Load { slot } => writeln!(f, "load ${}", slot)?,
                AirInstData::Store { slot, value } => writeln!(f, "store ${} = {}", slot, value)?,
                AirInstData::ParamStore { param_slot, value } => {
                    writeln!(f, "param_store %{} = {}", param_slot, value)?
                }
                AirInstData::Ret(inner) => {
                    if let Some(inner) = inner {
                        writeln!(f, "ret {}", inner)?
                    } else {
                        writeln!(f, "ret")?
                    }
                }
                AirInstData::Call {
                    runtime,
                    name,
                    args,
                } => {
                    if let Some(runtime) = runtime {
                        write!(f, "runtime.{runtime:?} ")?;
                    }
                    match interner {
                        Some(interner) => write!(f, "call @{}(", interner.resolve(name))?,
                        None => write!(f, "call @{}(", name.into_usize())?,
                    }
                    for (i, arg) in self.get_call_args(args).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", arg)?;
                    }
                    writeln!(f, ")")?;
                }
                AirInstData::AccessorCall { name, args } => {
                    match interner {
                        Some(interner) => write!(f, "accessor_call @{}(", interner.resolve(name))?,
                        None => write!(f, "accessor_call @{}(", name.into_usize())?,
                    }
                    for (i, arg) in self.get_call_args(args).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", arg)?;
                    }
                    writeln!(f, ")")?;
                }
                AirInstData::CallGeneric {
                    name,
                    type_args,
                    value_args,
                    args,
                } => {
                    match interner {
                        Some(interner) => write!(f, "call_generic @{}<", interner.resolve(name))?,
                        None => write!(f, "call_generic @{}<", name.into_usize())?,
                    }
                    // Show type arguments
                    for (i, type_val) in self.get_type_args(type_args).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "type#{}", type_val)?;
                    }
                    for (i, value) in self.get_const_values(value_args).enumerate() {
                        if i > 0 || !type_args.is_empty() {
                            write!(f, ", ")?;
                        }
                        write!(f, "value {:?}", value)?;
                    }
                    write!(f, ">(")?;
                    // Show runtime arguments
                    for (i, arg) in self.get_call_args(args).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", arg)?;
                    }
                    writeln!(f, ")")?;
                }
                AirInstData::Intrinsic {
                    runtime,
                    name,
                    args,
                } => {
                    if let Some(runtime) = runtime {
                        write!(f, "runtime.{runtime:?} ")?;
                    }
                    match interner {
                        Some(interner) => write!(f, "intrinsic @{}(", interner.resolve(name))?,
                        None => write!(f, "intrinsic @sym:{}(", name.into_usize())?,
                    }
                    for (i, arg) in self.get_intrinsic_args(args).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", arg)?;
                    }
                    writeln!(f, ")")?;
                }
                AirInstData::Param { index } => writeln!(f, "param {}", index)?,
                AirInstData::Block { statements, value } => {
                    write!(f, "block [")?;
                    for (i, s) in self.get_block_statements(statements).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", s)?;
                    }
                    writeln!(f, "], {}", value)?;
                }
                AirInstData::StructInit {
                    struct_id,
                    fields,
                    source_order,
                } => {
                    write!(f, "struct_init #{struct_id:?} {{")?;
                    for (i, field) in self.get_struct_fields(fields).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", field)?;
                    }
                    write!(f, "}} eval_order=[")?;
                    for (i, idx) in self.get_source_order(source_order).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", idx)?;
                    }
                    writeln!(f, "]")?;
                }
                AirInstData::ArrayInit { elements } => {
                    write!(f, "array_init [")?;
                    for (i, elem) in self.get_array_elements(elements).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", elem)?;
                    }
                    writeln!(f, "]")?;
                }
                AirInstData::PlaceRead { place } => {
                    write!(f, "place_read ")?;
                    self.fmt_place(f, *place)?;
                    writeln!(f)?;
                }
                AirInstData::PlaceWrite { place, value } => {
                    write!(f, "place_write ")?;
                    self.fmt_place(f, *place)?;
                    writeln!(f, " = {}", value)?;
                }
                AirInstData::EnumVariant {
                    enum_id,
                    variant_index,
                    payload,
                } => {
                    if payload.is_empty() {
                        writeln!(f, "enum_variant #{enum_id:?}::{variant_index}")?;
                    } else {
                        let payload: Vec<String> = self
                            .get_enum_payload(payload)
                            .map(|r| r.to_string())
                            .collect();
                        writeln!(
                            f,
                            "enum_variant #{:?}::{}({})",
                            enum_id,
                            variant_index,
                            payload.join(", ")
                        )?;
                    }
                }
                AirInstData::EnumPayloadGet {
                    base,
                    enum_id,
                    variant_index,
                    field_index,
                } => {
                    writeln!(
                        f,
                        "enum_payload_get {} #{:?}::{}.{}",
                        base, enum_id, variant_index, field_index
                    )?;
                }
                AirInstData::IntCast { value, from_ty } => {
                    writeln!(f, "intcast {} from {}", value, from_ty.name())?;
                }
                AirInstData::Drop { value } => {
                    writeln!(f, "drop {}", value)?;
                }
                AirInstData::StorageLive { slot } => {
                    writeln!(f, "storage_live ${}", slot)?;
                }
                AirInstData::StorageDead { slot } => {
                    writeln!(f, "storage_dead ${}", slot)?;
                }
                AirInstData::MarkMoved {
                    value,
                    slot,
                    is_param,
                    place,
                } => {
                    let prefix = if *is_param { "param%" } else { "$" };
                    match place {
                        Some(place_ref) => {
                            write!(f, "mark_moved {} (from {}{} fields", value, prefix, slot)?;
                            let p = self.get_place(*place_ref);
                            for proj in self.get_place_projections(p) {
                                match proj {
                                    AirProjection::Field { field_index, .. } => {
                                        write!(f, " .{}", field_index)?
                                    }
                                    AirProjection::Index { .. } => write!(f, " [_]")?,
                                }
                            }
                            writeln!(f, ")")?;
                        }
                        None => writeln!(f, "mark_moved {} (from {}{})", value, prefix, slot)?,
                    }
                }
            }
        }
        writeln!(f, "}}")
    }
}

impl fmt::Display for AirDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.air.fmt_with_interner(f, Some(self.interner))
    }
}

impl fmt::Display for Air {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_with_interner(f, None)
    }
}

impl Air {
    /// Format a place for display, showing the base and projections.
    fn fmt_place(&self, f: &mut fmt::Formatter<'_>, place_ref: AirPlaceRef) -> fmt::Result {
        let place = self.get_place(place_ref);

        // Write the base
        match place.base {
            AirPlaceBase::Local(slot) => write!(f, "${}", slot)?,
            AirPlaceBase::Param(slot) => write!(f, "param%{}", slot)?,
            AirPlaceBase::Accessor(call) => write!(f, "accessor%{}", call.as_u32())?,
        }

        // Write the projections
        let projections = self.get_place_projections(place);
        for proj in projections {
            match proj {
                AirProjection::Field {
                    struct_id,
                    field_index,
                } => {
                    write!(f, ".#{}.{}", struct_id.0, field_index)?;
                }
                AirProjection::Index { array_type, index } => {
                    write!(f, "({})[{}]", array_type.name(), index)?;
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod resource_limit_tests {
    use super::*;

    #[test]
    fn published_air_body_ceiling_matches_the_addressable_reference_space() {
        // Spec Appendix C.6:1: an `AirRef` is a u32 index into one body's own
        // instruction array, and the array length is narrowed to u32 at the
        // payload-staging boundary, so the count itself stays representable.
        assert_eq!(MAX_AIR_INSTRUCTIONS_PER_BODY, u32::MAX);
        assert_eq!(u64::from(MAX_AIR_INSTRUCTIONS_PER_BODY), 4_294_967_295);
    }

    #[test]
    fn ordinary_air_owner_never_latches_the_body_ceiling() {
        let pool = TypeInternPool::new().freeze();
        let mut editor = AirEditor::new(Type::UNIT);
        editor.add_unit(Span::new(0, 0));
        assert!(editor.air.latched_capacity_error().is_none());
        assert!(
            editor
                .finish(AirValidationContext::Canonical(&pool))
                .is_ok()
        );
    }

    #[test]
    fn a_latched_body_is_refused_with_a_diagnostic_naming_the_limit() {
        // RUE-1226 / spec C.1:2: `push_inst` is infallible, so the per-body
        // ceiling is latched during lowering and reported here, naming the
        // limit, rather than truncating `len() as u32` onto instruction 0.
        let pool = TypeInternPool::new().freeze();
        let mut editor = AirEditor::new(Type::UNIT);
        editor.add_unit(Span::new(0, 0));
        editor.air.instruction_limit_exceeded = true;

        let error = editor
            .finish(AirValidationContext::Canonical(&pool))
            .unwrap_err();
        assert!(error.is_resource_limit());
        assert!(error.to_string().contains("4294967295"));
        assert!(error.to_string().contains("spec Appendix C.6:1"));
        assert!(!error.to_string().contains("invalid AIR"));
    }

    #[test]
    fn air_boundary_failures_are_classified_apart_from_internal_errors() {
        let limit = AirValidationError {
            instruction: None,
            reason: "over the ceiling".to_string(),
            kind: AirValidationErrorKind::ResourceLimit,
        };
        let structural = AirValidationError {
            instruction: Some(3),
            reason: "bad operand".to_string(),
            kind: AirValidationErrorKind::Structural,
        };
        assert!(limit.is_resource_limit());
        assert!(!structural.is_resource_limit());
        assert_eq!(
            rue_error::CompileError::from(limit).kind.code(),
            rue_error::ErrorCode::COMPILER_RESOURCE_LIMIT
        );
        assert_eq!(
            rue_error::CompileError::from(structural).kind.code(),
            rue_error::ErrorCode::INTERNAL_ERROR
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn test_air_ref_size() {
        assert_eq!(std::mem::size_of::<AirRef>(), 4);
    }

    #[test]
    fn test_air_inst_size() {
        // Document actual sizes for future reference.
        // If this test fails, update the const assertions at the top of this file.
        let air_inst_size = std::mem::size_of::<AirInst>();
        let air_inst_data_size = std::mem::size_of::<AirInstData>();

        // These assertions document the current sizes.
        // If the layout changes, update both these values and the const assertions.
        assert!(
            air_inst_size <= 48,
            "AirInst grew beyond 48 bytes: {}",
            air_inst_size
        );
        assert!(
            air_inst_data_size <= 32,
            "AirInstData grew beyond 32 bytes: {}",
            air_inst_data_size
        );
    }

    #[test]
    fn test_add_and_get_inst() {
        let mut air = Air::new(Type::I32);
        let inst = AirInst {
            data: AirInstData::Const(42),
            ty: Type::I32,
            span: Span::new(0, 2),
        };
        let inst_ref = air.add_inst(inst);

        let retrieved = air.get(inst_ref);
        assert!(matches!(retrieved.data, AirInstData::Const(42)));
        assert_eq!(retrieved.ty, Type::I32);
    }

    #[test]
    fn place_preserves_logical_base_type() {
        let mut air = Air::new(Type::I32);
        let struct_id = StructId::from_pool_index(7);
        let base_type = Type::new_struct(struct_id);
        let place_ref = air
            .make_place(
                AirPlaceBase::Param(3),
                base_type,
                [AirProjection::Field {
                    struct_id,
                    field_index: 0,
                }],
            )
            .unwrap();

        let place = air.get_place(place_ref);
        assert_eq!(place.base, AirPlaceBase::Param(3));
        assert_eq!(place.base_type, base_type);
    }

    #[test]
    fn interner_aware_display_resolves_call_symbols() {
        let interner = ThreadedRodeo::new();
        let call = interner.get_or_intern("callee");
        let generic = interner.get_or_intern("generic");
        let intrinsic = interner.get_or_intern("assert");
        let mut air = Air::new(Type::UNIT);

        air.add_call(None, call, &[], Type::UNIT, Span::new(0, 0))
            .unwrap();
        air.add_call_generic(generic, &[], &[], &[], Type::UNIT, Span::new(0, 0))
            .unwrap();
        air.add_intrinsic(None, intrinsic, &[], Type::UNIT, Span::new(0, 0))
            .unwrap();

        let resolved = air.display_with_interner(&interner).to_string();
        assert!(resolved.contains("call @callee()"));
        assert!(resolved.contains("call_generic @generic<>()"));
        assert!(resolved.contains("intrinsic @assert()"));

        // The context-free Display API remains backward compatible.
        let raw = air.to_string();
        assert!(raw.contains(&format!("call @{}()", call.into_usize())));
        assert!(raw.contains(&format!("call_generic @{}<>()", generic.into_usize())));
        assert!(raw.contains(&format!("intrinsic @sym:{}()", intrinsic.into_usize())));
    }

    #[test]
    fn empty_payloads_are_canonical_and_append_nothing() {
        let mut air = Air::new(Type::UNIT);
        assert_eq!(air.append_words("test", &[7]).unwrap(), (0, 1));
        let before = air.extra.len();
        assert_eq!(air.append_words("test", &[]).unwrap(), (0, 0));
        assert_eq!(air.extra.len(), before);
        assert!(air.try_get_words(0, 0, "test").unwrap().is_empty());
        assert_eq!(
            air.try_get_words(1, 0, "test").unwrap_err().reason,
            "non-canonical empty range"
        );
    }

    #[test]
    fn every_payload_family_round_trips() {
        use crate::sema::ConstValue;

        let mut air = Air::new(Type::UNIT);
        let value = air.add_inst(AirInst {
            data: AirInstData::Const(7),
            ty: Type::I32,
            span: Span::new(0, 0),
        });
        let match_arms = air
            .add_match_arms(&[(AirPattern::Int(7), value)], 1)
            .unwrap();
        let call_args = air
            .add_call_args(&[AirCallArg {
                value,
                mode: AirArgMode::Borrow,
            }])
            .unwrap();
        let type_args = air.add_type_args(&[Type::I32]).unwrap();
        let const_values = air.add_const_values(&[ConstValue::Integer(7)]).unwrap();
        let intrinsic_args = air.add_intrinsic_args(&[value]).unwrap();
        let block_statements = air.add_block_statements(&[value]).unwrap();
        let struct_fields = air.add_struct_fields(&[value]).unwrap();
        let source_order = air.add_source_order(&[0]).unwrap();
        let array_elements = air.add_array_elements(&[value]).unwrap();
        let enum_payload = air.add_enum_payload(&[value]).unwrap();

        assert_eq!(air.get_match_arms(&match_arms).count(), 1);
        assert_eq!(
            air.get_call_args(&call_args).collect::<Vec<_>>()[0].value,
            value
        );
        assert_eq!(
            air.get_type_args(&type_args).collect::<Vec<_>>(),
            [Type::I32]
        );
        assert_eq!(
            air.get_const_values(&const_values).collect::<Vec<_>>(),
            [ConstValue::Integer(7)]
        );
        assert_eq!(
            air.get_intrinsic_args(&intrinsic_args).collect::<Vec<_>>(),
            [value]
        );
        assert_eq!(
            air.get_block_statements(&block_statements)
                .collect::<Vec<_>>(),
            [value]
        );
        assert_eq!(
            air.get_struct_fields(&struct_fields).collect::<Vec<_>>(),
            [value]
        );
        assert_eq!(air.get_source_order(&source_order).collect::<Vec<_>>(), [0]);
        assert_eq!(
            air.get_array_elements(&array_elements).collect::<Vec<_>>(),
            [value]
        );
        assert_eq!(
            air.get_enum_payload(&enum_payload).collect::<Vec<_>>(),
            [value]
        );
        assert_eq!(AIR_PAYLOAD_FAMILY_NAMES.len(), 10);
    }

    #[test]
    fn checked_call_arg_decoder_rejects_truncation_and_invalid_modes() {
        let mut truncated = Air::new(Type::UNIT);
        truncated.extra.push(4);
        let truncated_range = AirCallArgs {
            start: 0,
            extent: 2,
        };
        let error = truncated.validate_call_args(&truncated_range).unwrap_err();
        assert_eq!(error.reason, "range outside word store");
        assert_eq!((error.expected_width(), error.actual_width()), (2, 1));

        let mut invalid = Air::new(Type::UNIT);
        invalid.extra.extend([4, 99]);
        let invalid_range = AirCallArgs {
            start: 0,
            extent: 2,
        };
        assert_eq!(
            invalid
                .validate_call_args(&invalid_range)
                .unwrap_err()
                .reason,
            "invalid argument mode"
        );

        let partial_range = AirCallArgs {
            start: 0,
            extent: 1,
        };
        let error = invalid.validate_call_args(&partial_range).unwrap_err();
        assert_eq!(error.reason, "partial fixed-width record");
        assert_eq!((error.expected_width(), error.actual_width()), (2, 1));
    }

    #[test]
    fn checked_match_arm_decoder_rejects_malformed_envelopes() {
        fn error(words: &[u32]) -> AirPayloadError {
            let mut air = Air::new(Type::UNIT);
            let (start, extent) = air.append_words("test", words).unwrap();
            let range = AirMatchArms { start, extent };
            air.validate_match_arms(&range).unwrap_err()
        }

        assert_eq!(error(&[1, 99, 0]).reason, "invalid pattern tag");
        let truncated = error(&[1, PatternTag::Int.word(), 0]);
        assert_eq!(truncated.reason, "truncated record");
        assert_eq!(truncated.expected_width(), PatternTag::Int.layout().width);
        assert_eq!(truncated.actual_width(), 2);
        assert_eq!(
            error(&[1, PatternTag::Bool.word(), 0, 2]).reason,
            "invalid boolean"
        );
        assert_eq!(
            error(&[1, PatternTag::Wildcard.word(), 0, 123]).reason,
            "trailing words"
        );
    }

    #[test]
    fn checked_const_decoder_rejects_every_malformed_record_shape() {
        fn error(words: &[u32]) -> AirPayloadError {
            let mut air = Air::new(Type::UNIT);
            let (start, extent) = air.append_words("test", words).unwrap();
            let range = AirConstValueWords { start, extent };
            air.try_get_const_values(&range).unwrap_err()
        }

        assert_eq!(error(&[99]).reason, "invalid constant tag");
        let truncated = error(&[ConstValueTag::Integer.word(), 1, 2, 3]);
        assert_eq!(truncated.reason, "truncated constant payload");
        assert_eq!(truncated.expected_width(), 5);
        assert_eq!(truncated.actual_width(), 4);
        assert_eq!(
            error(&[ConstValueTag::Bool.word(), 2]).reason,
            "invalid boolean"
        );
        assert_eq!(
            error(&[ConstValueTag::Type.word(), u32::MAX]).reason,
            "invalid type encoding"
        );
    }

    #[test]
    fn every_payload_family_reports_boundary_widths_without_panicking() {
        let air = Air::new(Type::UNIT);
        for &family in &AIR_PAYLOAD_FAMILY_NAMES {
            let error = air.try_get_words(0, 1, family).unwrap_err();
            assert_eq!(error.phase, "AIR payload decode");
            assert_eq!(error.family, family);
            assert_eq!(error.range_start, 0);
            assert_eq!(error.record, 0);
            assert_eq!(error.actual_width(), 0);
            assert_eq!(error.expected_width(), 1);
            assert!(error.to_string().contains("expected width="));
            assert!(error.to_string().contains("actual width=0"));
        }
    }

    #[test]
    fn atomic_owner_builder_failure_leaves_all_stores_unchanged() {
        let mut air = Air::new(Type::UNIT);
        let before = air.checkpoint();
        let error = air
            .add_struct_init(
                StructId::from_pool_index(0),
                &[],
                &[0],
                Type::UNIT,
                Span::new(0, 0),
            )
            .unwrap_err();
        assert_eq!(
            error.kind,
            AirBuildErrorKind::ProducerInvariant("field/source-order length mismatch")
        );
        assert_eq!(air.checkpoint().instructions, before.instructions);
        assert_eq!(air.checkpoint().extra, before.extra);
    }

    #[test]
    fn wide_struct_init_source_order_preflight_is_linear_and_preserves_errors() {
        const WIDE_FIELDS: usize = 8_192;
        let mut air = Air::new(Type::UNIT);
        let field = air.add_inst(AirInst {
            data: AirInstData::UnitConst,
            ty: Type::UNIT,
            span: Span::new(0, 0),
        });
        let fields = vec![field; WIDE_FIELDS];
        let source_order = (0..WIDE_FIELDS as u32).rev().collect::<Vec<_>>();
        let mut fields_examined = 0;
        assert!(Air::source_order_is_permutation_with_observer(
            &source_order,
            fields.len(),
            || fields_examined += 1,
        ));
        assert_eq!(fields_examined, WIDE_FIELDS);
        air.add_struct_init(
            StructId::from_pool_index(0),
            &fields,
            &source_order,
            Type::UNIT,
            Span::new(0, 0),
        )
        .unwrap();

        let before_errors = air.checkpoint();
        for (fields, source_order, operation, reason) in [
            (
                vec![field],
                vec![0, 1],
                "stage",
                "field/source-order length mismatch",
            ),
            (
                vec![field, field],
                vec![0, 0],
                "preflight",
                "source order is not a permutation",
            ),
            (
                vec![field, field],
                vec![0, 2],
                "preflight",
                "source order is not a permutation",
            ),
        ] {
            let error = air
                .add_struct_init(
                    StructId::from_pool_index(0),
                    &fields,
                    &source_order,
                    Type::UNIT,
                    Span::new(0, 0),
                )
                .unwrap_err();
            assert_eq!(error.family, "struct initialization");
            assert_eq!(error.operation, operation);
            assert_eq!(error.kind, AirBuildErrorKind::ProducerInvariant(reason));
            assert_eq!(air.checkpoint().instructions, before_errors.instructions);
            assert_eq!(air.checkpoint().extra, before_errors.extra);
        }
    }

    #[test]
    fn validation_rejects_ordinary_forward_refs_and_invalid_places() {
        let pool = TypeInternPool::new().freeze();
        let mut forward = Air::new(Type::I32);
        forward.push_inst(AirInst {
            data: AirInstData::Add(AirRef::from_raw(0), AirRef::from_raw(0)),
            ty: Type::I32,
            span: Span::new(0, 0),
        });
        assert!(
            forward
                .finish(AirValidationContext::Canonical(&pool))
                .unwrap_err()
                .reason
                .contains("not defined before")
        );

        let mut place = Air::new(Type::I32);
        place.push_inst(AirInst {
            data: AirInstData::PlaceRead {
                place: AirPlaceRef::from_raw(7),
            },
            ty: Type::I32,
            span: Span::new(0, 0),
        });
        assert!(
            place
                .finish(AirValidationContext::Canonical(&pool))
                .unwrap_err()
                .reason
                .contains("outside the place store")
        );
    }

    #[test]
    fn authority_aware_validation_rejects_foreign_symbols() {
        let foreign = Spur::try_from_usize(0).unwrap();
        let mut air = AirEditor::new(Type::UNIT);
        air.add_call(None, foreign, &[], Type::UNIT, Span::new(0, 0))
            .unwrap();
        let pool = TypeInternPool::new().freeze();
        let interner = ThreadedRodeo::new();
        let error = air
            .finish(AirValidationContext::CanonicalWithSymbols(&pool, &interner))
            .unwrap_err();
        assert!(error.reason.contains("outside the interner"));
    }

    #[test]
    fn validation_rejects_foreign_enum_ids_in_match_patterns() {
        let mut editor = AirEditor::new(Type::UNIT);
        let scrutinee = editor.add_unit(Span::new(0, 0));
        let body = editor.add_unit(Span::new(0, 0));
        editor
            .add_match(
                scrutinee,
                &[(
                    AirPattern::EnumVariant {
                        enum_id: crate::EnumId::from_pool_index(0),
                        variant_index: 0,
                    },
                    body,
                )],
                Type::UNIT,
                Span::new(0, 0),
            )
            .unwrap();

        let pool = TypeInternPool::new().freeze();
        let error = editor
            .finish(AirValidationContext::Canonical(&pool))
            .unwrap_err();
        assert!(
            error.reason.contains("invalid enum pattern")
                && error.reason.contains("PoolIndexOutOfRange"),
            "unexpected validation error: {}",
            error.reason
        );
    }

    #[test]
    fn validation_rejects_out_of_range_enum_variants_in_match_patterns() {
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let (enum_id, _) = pool.register_enum(
            interner.get_or_intern("Choice"),
            crate::EnumDef {
                name: "Choice".into(),
                variants: Arc::from(["Only".into()]),
                variant_payloads: Vec::new(),
                is_pub: false,
                file_id: rue_span::FileId::DEFAULT,
            },
        );
        let pool = pool.freeze();

        let mut editor = AirEditor::new(Type::UNIT);
        let scrutinee = editor.air.push_inst(AirInst {
            data: AirInstData::UnitConst,
            ty: Type::new_enum(enum_id),
            span: Span::new(0, 0),
        });
        let body = editor.add_unit(Span::new(0, 0));
        editor
            .add_match(
                scrutinee,
                &[(
                    AirPattern::EnumVariant {
                        enum_id,
                        variant_index: 1,
                    },
                    body,
                )],
                Type::UNIT,
                Span::new(0, 0),
            )
            .unwrap();

        let error = editor
            .finish(AirValidationContext::Canonical(&pool))
            .unwrap_err();
        assert!(error.reason.contains("variant index 1 is out of bounds"));
    }

    #[test]
    fn validation_rejects_enum_patterns_for_a_different_scrutinee_type() {
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let (enum_id, _) = pool.register_enum(
            interner.get_or_intern("Choice"),
            crate::EnumDef {
                name: "Choice".into(),
                variants: Arc::from(["Only".into()]),
                variant_payloads: Vec::new(),
                is_pub: false,
                file_id: rue_span::FileId::DEFAULT,
            },
        );
        let pool = pool.freeze();

        let mut editor = AirEditor::new(Type::UNIT);
        let scrutinee = editor.add_unit(Span::new(0, 0));
        let body = editor.add_unit(Span::new(0, 0));
        editor
            .add_match(
                scrutinee,
                &[(
                    AirPattern::EnumVariant {
                        enum_id,
                        variant_index: 0,
                    },
                    body,
                )],
                Type::UNIT,
                Span::new(0, 0),
            )
            .unwrap();

        let error = editor
            .finish(AirValidationContext::Canonical(&pool))
            .unwrap_err();
        assert!(error.reason.contains("does not match scrutinee type"));
    }

    #[test]
    fn validation_rejects_literal_patterns_for_the_wrong_scrutinee_type() {
        let pool = TypeInternPool::new().freeze();
        for (scrutinee_ty, pattern, expected) in [
            (Type::BOOL, AirPattern::Int(1), "integer pattern"),
            (Type::I32, AirPattern::Bool(true), "boolean pattern"),
            (Type::UNIT, AirPattern::Int(1), "integer pattern"),
        ] {
            let mut editor = AirEditor::new(Type::UNIT);
            let scrutinee = editor.air.push_inst(AirInst {
                data: AirInstData::UnitConst,
                ty: scrutinee_ty,
                span: Span::new(0, 0),
            });
            let body = editor.add_unit(Span::new(0, 0));
            editor
                .add_match(scrutinee, &[(pattern, body)], Type::UNIT, Span::new(0, 0))
                .unwrap();
            let error = editor
                .finish(AirValidationContext::Canonical(&pool))
                .unwrap_err();
            assert!(error.reason.contains(expected), "{}", error.reason);
        }
    }

    fn projection_test_pool() -> (FrozenTypeInternPool, StructId, StructId, Type) {
        let interner = ThreadedRodeo::new();
        let pool = TypeInternPool::new();
        let make_struct = |name: &str| crate::StructDef {
            name: name.into(),
            fields: vec![crate::StructField {
                name: "field".into(),
                ty: Type::I32,
            }],
            is_copy: false,
            is_linear: false,
            destructor: None,
            is_builtin: false,
            is_pub: false,
            file_id: rue_span::FileId::DEFAULT,
        };
        let (first, _) =
            pool.register_struct(interner.get_or_intern("First"), make_struct("First"));
        let (second, _) =
            pool.register_struct(interner.get_or_intern("Second"), make_struct("Second"));
        let array = Type::new_array(pool.intern_array_from_type(Type::I32, 4));
        (pool.freeze(), first, second, array)
    }

    #[test]
    fn finish_contextually_validates_every_unused_place() {
        let (pool, first, second, array) = projection_test_pool();

        let mut wrong_field_owner = AirEditor::new(Type::UNIT);
        wrong_field_owner
            .make_place(
                AirPlaceBase::Local(0),
                Type::new_struct(first),
                [AirProjection::Field {
                    struct_id: second,
                    field_index: 0,
                }],
            )
            .unwrap();
        assert!(
            wrong_field_owner
                .finish(AirValidationContext::Canonical(&pool))
                .unwrap_err()
                .reason
                .contains("field projection expects")
        );

        let mut field_oob = AirEditor::new(Type::UNIT);
        field_oob
            .make_place(
                AirPlaceBase::Local(0),
                Type::new_struct(first),
                [AirProjection::Field {
                    struct_id: first,
                    field_index: 1,
                }],
            )
            .unwrap();
        assert!(
            field_oob
                .finish(AirValidationContext::Canonical(&pool))
                .unwrap_err()
                .reason
                .contains("field index 1 is out of bounds")
        );

        let mut wrong_array_owner = AirEditor::new(Type::UNIT);
        let index = wrong_array_owner.add_const(0, Type::I32, Span::new(0, 0));
        wrong_array_owner
            .make_place(
                AirPlaceBase::Local(0),
                Type::I32,
                [AirProjection::Index {
                    array_type: array,
                    index,
                }],
            )
            .unwrap();
        assert!(
            wrong_array_owner
                .finish(AirValidationContext::Canonical(&pool))
                .unwrap_err()
                .reason
                .contains("index projection expects")
        );

        let mut bad_index_ref = AirEditor::new(Type::UNIT);
        // Public builders reject this before mutation; inject owner corruption
        // here to exercise finish-time defense in depth.
        bad_index_ref.air.projections.push(AirProjection::Index {
            array_type: array,
            index: AirRef::from_raw(7),
        });
        bad_index_ref.air.places.push(AirPlace {
            base: AirPlaceBase::Local(0),
            base_type: array,
            projections: AirProjectionRange {
                start: 0,
                extent: 1,
                family: PhantomData,
            },
        });
        assert!(
            bad_index_ref
                .finish(AirValidationContext::Canonical(&pool))
                .unwrap_err()
                .reason
                .contains("outside the instruction store")
        );

        let mut bad_index_type = AirEditor::new(Type::UNIT);
        let index = bad_index_type.add_unit(Span::new(0, 0));
        bad_index_type
            .make_place(
                AirPlaceBase::Local(0),
                array,
                [AirProjection::Index {
                    array_type: array,
                    index,
                }],
            )
            .unwrap();
        assert!(
            bad_index_type
                .finish(AirValidationContext::Canonical(&pool))
                .unwrap_err()
                .reason
                .contains("non-integer type")
        );
    }

    #[test]
    fn payload_builder_preflight_failures_do_not_mutate_the_owner() {
        let symbol = Spur::try_from_usize(0).unwrap();
        let mut call = Air::new(Type::UNIT);
        let before = call.checkpoint();
        let error = call
            .add_call(
                None,
                symbol,
                &[AirCallArg {
                    value: AirRef::from_raw(7),
                    mode: AirArgMode::Normal,
                }],
                Type::UNIT,
                Span::new(0, 0),
            )
            .unwrap_err();
        assert!(matches!(
            error.kind,
            AirBuildErrorKind::ProducerInvariant(_)
        ));
        assert_eq!(call.checkpoint().instructions, before.instructions);
        assert_eq!(call.checkpoint().extra, before.extra);

        let mut matched = Air::new(Type::UNIT);
        let scrutinee = matched.add_inst(AirInst {
            data: AirInstData::UnitConst,
            ty: Type::UNIT,
            span: Span::new(0, 0),
        });
        let before = matched.checkpoint();
        let error = matched
            .add_match(
                scrutinee,
                &[(AirPattern::Wildcard, AirRef::from_raw(7))],
                Type::UNIT,
                Span::new(0, 0),
            )
            .unwrap_err();
        assert!(matches!(
            error.kind,
            AirBuildErrorKind::ProducerInvariant(_)
        ));
        assert_eq!(matched.checkpoint().instructions, before.instructions);
        assert_eq!(matched.checkpoint().extra, before.extra);

        let mut structure = Air::new(Type::UNIT);
        let field = structure.add_inst(AirInst {
            data: AirInstData::UnitConst,
            ty: Type::UNIT,
            span: Span::new(0, 0),
        });
        let before = structure.checkpoint();
        let error = structure
            .add_struct_init(
                StructId::from_pool_index(0),
                &[field, field],
                &[0, 0],
                Type::UNIT,
                Span::new(0, 0),
            )
            .unwrap_err();
        assert_eq!(
            error.kind,
            AirBuildErrorKind::ProducerInvariant("source order is not a permutation")
        );
        assert_eq!(structure.checkpoint().instructions, before.instructions);
        assert_eq!(structure.checkpoint().extra, before.extra);
    }

    #[test]
    fn place_builder_failures_leave_both_owner_stores_unchanged() {
        let mut invalid = Air::new(Type::UNIT);
        let before = invalid.checkpoint();
        let error = invalid
            .make_place(
                AirPlaceBase::Local(0),
                Type::UNIT,
                [AirProjection::Index {
                    array_type: Type::UNIT,
                    index: AirRef::from_raw(0),
                }],
            )
            .unwrap_err();
        assert!(matches!(
            error.kind,
            AirBuildErrorKind::ProducerInvariant(_)
        ));
        assert_eq!(invalid.checkpoint().projections, before.projections);
        assert_eq!(invalid.checkpoint().places, before.places);

        for failure in [
            PlaceReserveFailure::ProjectionStore,
            PlaceReserveFailure::PlaceStore,
        ] {
            let mut air = Air::new(Type::UNIT);
            let index = air.add_inst(AirInst {
                data: AirInstData::Const(0),
                ty: Type::I32,
                span: Span::new(0, 0),
            });
            let before = air.checkpoint();
            air.fail_next_place_reserve(failure);
            let error = air
                .make_place(
                    AirPlaceBase::Local(0),
                    Type::UNIT,
                    [AirProjection::Index {
                        array_type: Type::UNIT,
                        index,
                    }],
                )
                .unwrap_err();
            assert_eq!(error.kind, AirBuildErrorKind::ResourceExhaustion);
            assert_eq!(air.checkpoint().projections, before.projections);
            assert_eq!(air.checkpoint().places, before.places);
            assert!(air.projections.is_empty());
            assert!(air.places.is_empty());
        }
    }

    #[test]
    fn payload_errors_report_phase_and_range_context() {
        let mut air = Air::new(Type::UNIT);
        air.extra.extend([4, 99]);
        let error = air
            .validate_call_args(&AirCallArgs {
                start: 0,
                extent: 2,
            })
            .unwrap_err();
        assert_eq!(error.phase, "AIR payload decode");
        assert_eq!((error.range_start, error.range_extent), (0, 2));
        assert!(error.to_string().contains("range start=0 extent=2"));
    }

    #[test]
    fn build_failure_categories_survive_the_compile_error_boundary() {
        for (kind, expected_code, is_ice) in [
            (
                AirBuildErrorKind::ProducerInvariant("malformed producer data"),
                rue_error::ErrorCode::INTERNAL_ERROR,
                true,
            ),
            (
                AirBuildErrorKind::ResourceLimit,
                rue_error::ErrorCode::COMPILER_RESOURCE_LIMIT,
                false,
            ),
            (
                AirBuildErrorKind::ResourceExhaustion,
                rue_error::ErrorCode::COMPILER_RESOURCE_EXHAUSTION,
                false,
            ),
        ] {
            let error: rue_error::CompileError = AirBuildError {
                phase: "AIR",
                family: "test",
                operation: "test",
                kind,
            }
            .into();
            assert_eq!(error.kind.code(), expected_code);
            assert_eq!(
                matches!(
                    error.kind,
                    rue_error::ErrorKind::CompilerProducerInvariant(_)
                ),
                is_ice
            );
        }
    }

    #[test]
    fn projection_ranges_round_trip_without_exposing_positions() {
        let mut air = Air::new(Type::UNIT);
        let id = StructId::from_pool_index(1);
        let place = air
            .make_place(
                AirPlaceBase::Local(0),
                Type::new_struct(id),
                [AirProjection::Field {
                    struct_id: id,
                    field_index: 2,
                }],
            )
            .unwrap();
        assert_eq!(air.get_place(place).projection_count(), 1);
        assert_eq!(
            air.get_place_projections(air.get_place(place)),
            [AirProjection::Field {
                struct_id: id,
                field_index: 2
            }]
        );
    }

    #[test]
    fn editor_finishes_into_an_immutable_validated_owner() {
        let mut editor = AirEditor::new(Type::UNIT);
        let value = editor.add_unit(Span::new(0, 0));
        editor.add_ret(Some(value), Type::UNIT, Span::new(0, 0));

        let pool = TypeInternPool::new().freeze();
        let air = editor
            .finish(AirValidationContext::Canonical(&pool))
            .unwrap();
        assert_eq!(air.len(), 2);
        assert!(matches!(
            air.get(AirRef::from_raw(1)).data,
            AirInstData::Ret(_)
        ));
    }

    #[test]
    fn validation_rejects_a_payload_range_detached_from_its_owner() {
        let symbol = Spur::try_from_usize(0).unwrap();
        let mut source = AirEditor::new(Type::UNIT);
        let source_value = source.add_unit(Span::new(0, 0));
        let call = source
            .add_call(
                None,
                symbol,
                &[AirCallArg {
                    value: source_value,
                    mode: AirArgMode::Normal,
                }],
                Type::UNIT,
                Span::new(0, 0),
            )
            .unwrap();
        let foreign = match &source.air.get(call).data {
            AirInstData::Call { args, .. } => AirCallArgs {
                start: args.start,
                extent: args.extent,
            },
            _ => unreachable!(),
        };

        // This corruption is only expressible inside the owner module. Public
        // producers cannot construct a range or insert an arbitrary node.
        let mut destination = AirEditor::new(Type::UNIT);
        destination.add_unit(Span::new(0, 0));
        destination.air.push_inst(AirInst {
            data: AirInstData::Call {
                runtime: None,
                name: symbol,
                args: foreign,
            },
            ty: Type::UNIT,
            span: Span::new(0, 0),
        });

        let pool = TypeInternPool::new().freeze();
        let error = destination
            .finish(AirValidationContext::Canonical(&pool))
            .unwrap_err();
        assert!(error.reason.contains("range outside word store"));
    }
}
