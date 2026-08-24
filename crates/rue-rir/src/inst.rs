//! RIR instruction definitions.
//!
//! Instructions are stored in a dense array and referenced by index.
//! This provides good cache locality and efficient traversal.

use std::fmt;
use std::marker::PhantomData;

use lasso::{Key, Spur};
use rue_span::{FileId, Span};

use crate::type_syntax::{RirTypeSyntaxArena, RirTypeSyntaxBuilder, RirTypeSyntaxRef};

mod packed;
mod payload_support;

pub use packed::{
    PackedRirAppend, PackedRirAppendError, PackedRirAppendMetadata, PackedRirDecodeError,
    PackedRirEncodeError, PackedRirMetadata, PackedRirMethodOwner, PackedRirProjection,
    PackedRirSymbols, PackedValidatedRir, RirFallibleIntrinsic, RirFallibleIntrinsicSet,
};

/// The published per-program ceiling shared by the RIR instruction array and
/// the RIR payload word store (spec Appendix C.6:1). Both are indexed by `u32`,
/// so a program may hold at most this many instructions and at most this many
/// payload words. Exceeding either is a diagnosable compile-time failure
/// (C.1:2), surfaced as `E1401` at the canonical lowering boundary — never a
/// wrapped `InstRef` or a truncated `(start, extent)` range.
pub const MAX_RIR_ENTRIES_PER_PROGRAM: u32 = u32::MAX;

/// A failure while staging a compact RIR payload.
#[derive(Debug)]
pub enum RirPayloadBuildError {
    ResourceLimitExceeded {
        family: &'static str,
    },
    CapacityFailure {
        family: &'static str,
    },
    InternerFailure {
        family: &'static str,
        kind: lasso::LassoErrorKind,
    },
    InvalidBuilderInput {
        family: &'static str,
        reason: &'static str,
    },
}

impl RirPayloadBuildError {
    /// Whether this failure is an implementation-limit rejection (spec C.1:2)
    /// rather than a producer bug or an allocation failure. Consumers use this
    /// to pick `E1401` over an internal-error code.
    pub fn is_resource_limit(&self) -> bool {
        matches!(self, Self::ResourceLimitExceeded { .. })
            || matches!(self, Self::InternerFailure { kind, .. } if !kind.is_failed_alloc())
    }

    /// Whether this failure is an allocation failure for an otherwise
    /// representable request (`E1402`), not a limit rejection.
    pub fn is_resource_exhaustion(&self) -> bool {
        matches!(self, Self::CapacityFailure { .. })
            || matches!(self, Self::InternerFailure { kind, .. } if kind.is_failed_alloc())
    }
}

impl fmt::Display for RirPayloadBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ResourceLimitExceeded { family } => {
                write!(
                    f,
                    "RIR {family} exceeded the implementation limit of \
                     {MAX_RIR_ENTRIES_PER_PROGRAM} per program (spec Appendix C.6:1)"
                )
            }
            Self::CapacityFailure { family } => {
                write!(f, "could not reserve storage for RIR {family} payload")
            }
            Self::InternerFailure { family, kind } => {
                write!(f, "could not intern RIR {family} spelling: {kind:?}")
            }
            Self::InvalidBuilderInput { family, reason } => {
                write!(f, "invalid RIR {family} builder input: {reason}")
            }
        }
    }
}

impl std::error::Error for RirPayloadBuildError {}

/// Structured corruption reported by the production RIR payload decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RirPayloadError {
    pub family: &'static str,
    pub start: u32,
    pub extent: u32,
    pub record: Option<u32>,
    pub expected_width: usize,
    pub actual_width: usize,
    pub reason: &'static str,
}

macro_rules! rir_payload_error {
    (
        $family:ident,
        $start:ident,
        $extent:ident,
        record: $record:expr,
        expected: $expected:expr,
        actual: $actual:expr,
        reason: $reason:expr $(,)?
    ) => {
        RirPayloadError::new(
            $family, $start, $extent, $record, $expected, $actual, $reason,
        )
    };
    (
        family: $family:expr,
        start: $start:expr,
        extent: $extent:expr,
        record: $record:expr,
        expected: $expected:expr,
        actual: $actual:expr,
        $reason:ident $(,)?
    ) => {
        RirPayloadError::new(
            $family, $start, $extent, $record, $expected, $actual, $reason,
        )
    };
    (
        family: $family:expr,
        start: $start:expr,
        extent: $extent:expr,
        record: $record:expr,
        expected: $expected:expr,
        actual: $actual:expr,
        reason: $reason:expr $(,)?
    ) => {
        RirPayloadError::new(
            $family, $start, $extent, $record, $expected, $actual, $reason,
        )
    };
}

/// Canonical source/interner bounds required to publish an immutable RIR.
pub struct RirValidationContext<'a> {
    pub symbol_count: usize,
    pub source_lengths: &'a [(FileId, u32)],
}

#[repr(C)]
#[derive(Clone, PartialEq, Eq)]
struct PayloadRange<Family> {
    start: u32,
    extent: u32,
    family: PhantomData<fn() -> Family>,
}

macro_rules! payload_family {
    ($name:ident, $marker:ident, $family:literal) => {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum $marker {}

        /// Opaque range into the RIR word store for this payload family.
        #[repr(transparent)]
        #[derive(Clone, PartialEq, Eq)]
        pub struct $name(PayloadRange<$marker>);

        impl $name {
            const FAMILY: &'static str = $family;

            const fn from_parts(start: u32, extent: u32) -> Self {
                Self(PayloadRange {
                    start,
                    extent,
                    family: PhantomData,
                })
            }

            const fn start(&self) -> u32 {
                self.0.start
            }
            const fn extent(&self) -> u32 {
                self.0.extent
            }
        }

        impl PayloadFallback for $name {
            fn payload_fallback() -> Self {
                Self::from_parts(0, 0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct(stringify!($name))
                    .field("words", &format_args!("{}+{}", self.0.start, self.0.extent))
                    .finish()
            }
        }

        const _: () = assert!(std::mem::size_of::<$name>() == 2 * std::mem::size_of::<u32>());
        const _: () = assert!(std::mem::align_of::<$name>() == std::mem::align_of::<u32>());
    };
}

pub(crate) trait PayloadFallback {
    fn payload_fallback() -> Self;
}

payload_family!(RirMatchArmsRange, MatchArmsFamily, "match arms");
payload_family!(RirDirectivesRange, DirectivesFamily, "directives");
payload_family!(RirParamsRange, ParamsFamily, "parameters");
payload_family!(RirCallArgsRange, CallArgsFamily, "call arguments");
payload_family!(
    RirIntrinsicArgsRange,
    IntrinsicArgsFamily,
    "intrinsic arguments"
);
payload_family!(
    RirInternalIntrinsicArgsRange,
    InternalIntrinsicArgsFamily,
    "internal intrinsic arguments"
);
payload_family!(RirBlockInstsRange, BlockInstsFamily, "block instructions");
payload_family!(RirStructFieldsRange, StructFieldsFamily, "struct fields");
payload_family!(
    RirAnonStructFieldsRange,
    AnonStructFieldsFamily,
    "anonymous struct fields"
);
payload_family!(RirStructMethodsRange, StructMethodsFamily, "struct methods");
payload_family!(
    RirAnonStructMethodsRange,
    AnonStructMethodsFamily,
    "anonymous struct methods"
);
payload_family!(RirFieldInitsRange, FieldInitsFamily, "field initializers");
payload_family!(RirEnumVariantsRange, EnumVariantsFamily, "enum variants");
payload_family!(
    RirAnonEnumVariantsRange,
    AnonEnumVariantsFamily,
    "anonymous enum variants"
);
payload_family!(
    RirEnumPayloadsRange,
    EnumPayloadsFamily,
    "enum variant payloads"
);
payload_family!(
    RirAnonEnumPayloadsRange,
    AnonEnumPayloadsFamily,
    "anonymous enum variant payloads"
);
payload_family!(RirArrayElemsRange, ArrayElemsFamily, "array elements");

/// Stable inventory of every owner-issued variable-payload family.
///
/// Verification and benchmark tooling consumes this list so adding a schema
/// family necessarily changes the cross-phase inventory rather than silently
/// escaping its coverage.
pub const RIR_PAYLOAD_FAMILY_NAMES: [&str; 17] = [
    RirMatchArmsRange::FAMILY,
    RirDirectivesRange::FAMILY,
    RirParamsRange::FAMILY,
    RirCallArgsRange::FAMILY,
    RirIntrinsicArgsRange::FAMILY,
    RirInternalIntrinsicArgsRange::FAMILY,
    RirBlockInstsRange::FAMILY,
    RirStructFieldsRange::FAMILY,
    RirAnonStructFieldsRange::FAMILY,
    RirStructMethodsRange::FAMILY,
    RirAnonStructMethodsRange::FAMILY,
    RirFieldInitsRange::FAMILY,
    RirEnumVariantsRange::FAMILY,
    RirAnonEnumVariantsRange::FAMILY,
    RirEnumPayloadsRange::FAMILY,
    RirAnonEnumPayloadsRange::FAMILY,
    RirArrayElemsRange::FAMILY,
];

/// Read-only accounting for the compact RIR payload store.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RirPayloadStorageStats {
    pub family_logical_bytes: [usize; 17],
    pub word_store_logical_bytes: usize,
    pub word_store_capacity_bytes: usize,
    pub nonempty_variable_envelopes: usize,
    /// Largest complete logical payload staged by one atomic builder.
    pub peak_staging_bytes: usize,
}

/// A reference to an instruction in the RIR.
///
/// This is a lightweight handle (4 bytes) that indexes into the instruction array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstRef(u32);

impl InstRef {
    /// Create an instruction reference from a raw index.
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

impl PayloadFallback for InstRef {
    fn payload_fallback() -> Self {
        Self(u32::MAX)
    }
}

/// A directive in the RIR (e.g., @allow(unused_variable))
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RirDirective {
    /// Directive name (e.g., "allow")
    pub name: Spur,
    /// Arguments (e.g., ["unused_variable"])
    pub args: Vec<Spur>,
    /// Span covering the directive
    pub span: Span,
}

/// Parameter passing mode in RIR.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RirParamMode {
    /// Normal pass-by-value parameter
    #[default]
    Normal,
    /// Inout parameter - mutated in place and returned to caller
    Inout,
    /// Borrow parameter - immutable borrow without ownership transfer
    Borrow,
}

impl RirParamMode {
    /// Convert the serialized parameter mode from the RIR extra array.
    ///
    /// Invalid values indicate corrupted RIR or a producer/consumer mismatch.
    /// They must not silently recover as normal by-value parameters, because
    /// that changes ownership and aliasing semantics.
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => RirParamMode::Normal,
            1 => RirParamMode::Inout,
            2 => RirParamMode::Borrow,
            _ => panic!("invalid RirParamMode value: {}", v),
        }
    }

    /// Serialize this mode into the RIR extra array.
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

/// A parameter in a function declaration.
#[derive(Debug, Clone, Copy)]
pub struct RirParam {
    /// Parameter name
    pub name: Spur,
    /// Parameter type
    pub ty: RirTypeSyntaxRef,
    /// Parameter passing mode
    pub mode: RirParamMode,
    /// Whether this parameter is evaluated at compile time (declared with
    /// the `comptime` modifier; carried separately from `mode`, which stays
    /// `Normal` for comptime parameters)
    pub is_comptime: bool,
    /// Span of the parameter's name, used to point diagnostics (e.g. the
    /// duplicate-parameter error, RUE-349) at the offending occurrence.
    pub span: Span,
}

/// Argument passing mode in RIR.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RirArgMode {
    /// Normal pass-by-value argument
    #[default]
    Normal,
    /// Inout argument - mutated in place
    Inout,
    /// Borrow argument - immutable borrow
    Borrow,
}

impl RirArgMode {
    /// Convert the serialized call-argument mode from the RIR extra array.
    ///
    /// Invalid values indicate corrupted RIR or a producer/consumer mismatch.
    /// They must not silently recover as normal by-value arguments, because
    /// that changes ownership and aliasing semantics.
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => RirArgMode::Normal,
            1 => RirArgMode::Inout,
            2 => RirArgMode::Borrow,
            _ => panic!("invalid RirArgMode value: {}", v),
        }
    }

    /// Serialize this mode into the RIR extra array.
    pub fn as_u32(self) -> u32 {
        self as u32
    }
}

/// An argument in a function call.
#[derive(Debug, Clone, Copy)]
pub struct RirCallArg {
    /// The argument expression
    pub value: InstRef,
    /// The passing mode for this argument
    pub mode: RirArgMode,
}

impl RirCallArg {
    /// Returns true if this argument is passed as inout.
    /// This is a convenience method for backwards compatibility.
    pub fn is_inout(&self) -> bool {
        self.mode == RirArgMode::Inout
    }

    /// Returns true if this argument is passed as borrow.
    pub fn is_borrow(&self) -> bool {
        self.mode == RirArgMode::Borrow
    }
}

/// A pattern in a match expression (RIR level - untyped).
#[derive(Debug, Clone)]
pub enum RirPattern {
    /// Wildcard pattern `_` - matches anything
    Wildcard(Span),
    /// Integer literal pattern. The magnitude and sign are kept separate so
    /// Sema can range-check the literal against the scrutinee type exactly
    /// like `let` bindings (E0800/E0801) instead of silently wrapping
    /// (RUE-74). `negative` means the source had a leading `-`.
    Int {
        /// Magnitude of the literal as written (e.g. 128 for `-128`).
        value: u64,
        /// True if the pattern was written with a leading minus sign.
        negative: bool,
        /// Span of the pattern (including the minus sign, if any).
        span: Span,
    },
    /// Boolean literal pattern
    Bool(bool, Span),
    /// Path pattern for enum variants (e.g., `Color::Red` or `module.Color::Red`)
    Path {
        /// Optional module reference for qualified paths (e.g., the `module` in `module.Color::Red`)
        module: Option<InstRef>,
        /// Optional inline type-constructor call head — the instruction that
        /// reduces to the enum type at comptime for `F(args).Variant(..)`
        /// (RUE-596, spec 4.14:23). When `Some`, the enum is
        /// the reduction of this head and `type_name` is only the constructor
        /// function's name; `None` for an ordinary `Enum.Variant` pattern.
        ctor_head: Option<InstRef>,
        /// The enum type name
        type_name: Spur,
        /// The variant name
        variant: Spur,
        /// Payload binding names for a tuple-variant pattern `Circle(r)`
        /// (RUE-221), in payload order. Empty for a discriminant-only pattern.
        bindings: Vec<Spur>,
        /// Span of the pattern
        span: Span,
    },
}

impl RirPattern {
    /// Get the span of this pattern.
    pub fn span(&self) -> Span {
        match self {
            RirPattern::Wildcard(span) => *span,
            RirPattern::Int { span, .. } => *span,
            RirPattern::Bool(_, span) => *span,
            RirPattern::Path { span, .. } => *span,
        }
    }
}

/// A lazily decoded, borrowing view of a match pattern stored in RIR.
///
/// Unlike [`RirPattern`], a path pattern's bindings remain in the compact RIR
/// word store and are decoded only as they are traversed.
#[derive(Debug, Clone)]
pub enum RirPatternView<'a> {
    Wildcard(Span),
    Int {
        value: u64,
        negative: bool,
        span: Span,
    },
    Bool(bool, Span),
    Path {
        module: Option<InstRef>,
        ctor_head: Option<InstRef>,
        type_name: Spur,
        variant: Spur,
        bindings: RirSymbols<'a>,
        span: Span,
    },
}

impl RirPatternView<'_> {
    pub fn span(&self) -> Span {
        match self {
            Self::Wildcard(span) | Self::Bool(_, span) => *span,
            Self::Int { span, .. } | Self::Path { span, .. } => *span,
        }
    }

    /// Materialize an owned pattern when it must outlive the RIR borrow.
    pub fn to_owned(&self) -> RirPattern {
        match self {
            Self::Wildcard(span) => RirPattern::Wildcard(*span),
            Self::Int {
                value,
                negative,
                span,
            } => RirPattern::Int {
                value: *value,
                negative: *negative,
                span: *span,
            },
            Self::Bool(value, span) => RirPattern::Bool(*value, *span),
            Self::Path {
                module,
                ctor_head,
                type_name,
                variant,
                bindings,
                span,
            } => RirPattern::Path {
                module: *module,
                ctor_head: *ctor_head,
                type_name: *type_name,
                variant: *variant,
                bindings: bindings.to_vec(),
                span: *span,
            },
        }
    }
}

/// A reusable, zero-allocation view of fixed-width records in the RIR word store.
pub struct RirSlice<'a, T> {
    words: &'a [u32],
    width: usize,
    decode: fn(&[u32]) -> T,
}

impl<T> std::fmt::Debug for RirSlice<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RirSlice")
            .field("len", &self.len())
            .finish()
    }
}

impl<T> Clone for RirSlice<'_, T> {
    fn clone(&self) -> Self {
        Self {
            words: self.words,
            width: self.width,
            decode: self.decode,
        }
    }
}

impl<'a, T: 'a> RirSlice<'a, T> {
    fn new(words: &'a [u32], width: usize, decode: fn(&[u32]) -> T) -> Self {
        assert!(width != 0 && words.len().is_multiple_of(width));
        for record in words.chunks_exact(width) {
            decode(record);
        }
        Self {
            words,
            width,
            decode,
        }
    }

    pub fn len(&self) -> usize {
        self.words.len() / self.width
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn iter(&self) -> DecodedIter<impl ExactSizeIterator<Item = T> + Clone + 'a> {
        DecodedIter(self.values())
    }

    pub fn values(&self) -> impl ExactSizeIterator<Item = T> + Clone + 'a {
        RirSliceIter {
            words: self.words.chunks_exact(self.width),
            decode: self.decode,
        }
    }

    pub fn get(&self, index: usize) -> Option<T> {
        self.values().nth(index)
    }

    pub fn to_vec(&self) -> Vec<T> {
        self.values().collect()
    }
}

impl<'a, T> IntoIterator for RirSlice<'a, T> {
    type Item = T;
    type IntoIter = RirSliceIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        RirSliceIter {
            words: self.words.chunks_exact(self.width),
            decode: self.decode,
        }
    }
}

impl<'a, T> IntoIterator for &RirSlice<'a, T> {
    type Item = Decoded<T>;
    type IntoIter = DecodedIter<RirSliceIter<'a, T>>;

    fn into_iter(self) -> Self::IntoIter {
        DecodedIter(RirSliceIter {
            words: self.words.chunks_exact(self.width),
            decode: self.decode,
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Decoded<T>(T);

impl<T> std::ops::Deref for Decoded<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct DecodedIter<I>(I);

impl<I: Iterator> Iterator for DecodedIter<I> {
    type Item = Decoded<I::Item>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(Decoded)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}

impl<I: ExactSizeIterator> ExactSizeIterator for DecodedIter<I> {}

pub struct RirSliceIter<'a, T> {
    words: std::slice::ChunksExact<'a, u32>,
    decode: fn(&[u32]) -> T,
}

impl<T> Clone for RirSliceIter<'_, T> {
    fn clone(&self) -> Self {
        Self {
            words: self.words.clone(),
            decode: self.decode,
        }
    }
}

impl<T> Iterator for RirSliceIter<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.words.next().map(self.decode)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.words.size_hint()
    }
}

impl<T> ExactSizeIterator for RirSliceIter<'_, T> {}

pub type RirSymbols<'a> = RirSlice<'a, Spur>;
pub type RirTypeSyntaxRefs<'a> = RirSlice<'a, RirTypeSyntaxRef>;

/// Stable tag for a span-bearing field inside one RIR instruction.
///
/// Record indices are local to their typed payload. Optional fields have their
/// own tags, so adding or removing one never renumbers a later instruction or
/// a different field family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RirSpanField {
    Instruction,
    MatchPattern { arm: u32 },
    FunctionDirective { directive: u32 },
    FunctionParameter { parameter: u32 },
    ConstDirective { directive: u32 },
    AllocDirective { directive: u32 },
    StructDirective { directive: u32 },
    StructInitShorthand,
}

/// Position-independent identity of one span field in structurally equal RIR.
///
/// The instruction index is a dense structural RIR location. It is never
/// derived from callback order, source coordinates, tokens, or spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RirSpanSlot {
    instruction: InstRef,
    field: RirSpanField,
}

impl RirSpanSlot {
    const fn new(instruction: InstRef, field: RirSpanField) -> Self {
        Self { instruction, field }
    }

    pub const fn instruction(self) -> InstRef {
        self.instruction
    }

    pub const fn field(self) -> RirSpanField {
        self.field
    }
}

/// Stable inventory of the span-bearing storage families in RIR.
///
/// `api_inventory` ties this list to the concrete storage declarations and to
/// the canonical visitor. Adding a span field therefore requires an explicit
/// visitor/schema update.
pub const RIR_SPAN_FIELD_FAMILY_NAMES: [&str; 5] = [
    "instruction",
    "directive",
    "parameter",
    "match pattern",
    "struct-init shorthand",
];

/// Failure from canonical RIR span-slot traversal.
#[derive(Debug)]
pub enum RirSpanTraversalError<E> {
    MalformedPayload(RirPayloadError),
    DuplicateSlot(RirSpanSlot),
    Callback(E),
}

/// Failure while atomically appending and remapping a RIR owner by span slot.
#[derive(Debug)]
pub enum RirSpanRemapError<E> {
    MalformedPayload(RirPayloadError),
    MalformedTypeSyntax(crate::RirTypeSyntaxValidationError),
    DuplicateSlot(RirSpanSlot),
    MissingSlot(RirSpanSlot),
    UnexpectedSlot {
        expected: RirSpanSlot,
        actual: RirSpanSlot,
    },
    UnconsumedSlot(RirSpanSlot),
    InvalidInstructionRange(std::ops::Range<u32>),
    ForeignInstructionEdge {
        instruction: InstRef,
        child: InstRef,
    },
    Checkpoint(E),
    Mapping {
        slot: RirSpanSlot,
        error: E,
    },
    Build(RirPayloadBuildError),
}

impl<E> From<RirPayloadBuildError> for RirSpanRemapError<E> {
    fn from(error: RirPayloadBuildError) -> Self {
        Self::Build(error)
    }
}

fn type_syntax_build_error(error: crate::RirTypeSyntaxBuildError) -> RirPayloadBuildError {
    let family = match error {
        crate::RirTypeSyntaxBuildError::TooManyNodes => "type syntax nodes",
        crate::RirTypeSyntaxBuildError::TooManySymbols => "type syntax symbols",
        crate::RirTypeSyntaxBuildError::TooMuchPayload => "type syntax payload",
    };
    RirPayloadBuildError::ResourceLimitExceeded { family }
}

/// One typed step in a definition-relative structural path. Indices are local
/// to the immediately enclosing syntax node, never absolute source positions
/// or global RIR instruction indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RirStructuralPathSegment {
    Body,
    ParameterType(u32),
    ReturnType,
    Statement(u32),
    Operand(u32),
    Branch(u32),
    MatchArm(u32),
    FieldType(u32),
    VariantPayload { variant: u32, payload: u32 },
    Method(u32),
    AnonymousType(u32),
    StringLiteral(u32),
    ReadOnlyData(u32),
}

/// Trivia- and absolute-position-insensitive identity of a structural source
/// site, relative to the semantic definition that owns it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RirStructuralAnchor(std::sync::Arc<[RirStructuralPathSegment]>);

impl RirStructuralAnchor {
    pub fn new(segments: impl Into<std::sync::Arc<[RirStructuralPathSegment]>>) -> Self {
        Self(segments.into())
    }

    pub fn segments(&self) -> &[RirStructuralPathSegment] {
        &self.0
    }
}

/// A borrowing directive view. Arguments are decoded lazily from RIR.
#[derive(Debug, Clone)]
pub struct RirDirectiveView<'a> {
    pub name: Spur,
    pub args: RirSymbols<'a>,
    pub span: Span,
}

impl RirDirectiveView<'_> {
    /// Materialize an owned directive when it must outlive the RIR borrow.
    pub fn to_owned(&self) -> RirDirective {
        RirDirective {
            name: self.name,
            args: self.args.to_vec(),
            span: self.span,
        }
    }
}

/// Extra data marker types for type-safe storage in the extra array.
/// These types represent data stored in the extra array.

#[derive(Clone, Copy)]
struct FixedPayloadSchema {
    width: usize,
    symbol_offsets: &'static [usize],
}

const REF_SCHEMA: FixedPayloadSchema = FixedPayloadSchema {
    width: 1,
    symbol_offsets: &[],
};
const SYMBOL_SCHEMA: FixedPayloadSchema = FixedPayloadSchema {
    width: 1,
    symbol_offsets: &[0],
};
/// `[value, mode]`.
const CALL_ARG_SCHEMA: FixedPayloadSchema = FixedPayloadSchema {
    width: 2,
    symbol_offsets: &[],
};
const CALL_ARG_VALUE: usize = 0;
const CALL_ARG_MODE: usize = 1;
/// `[name, ty, mode, is_comptime, span.file, span.start, span.end]`.
const PARAM_SCHEMA: FixedPayloadSchema = FixedPayloadSchema {
    width: 7,
    symbol_offsets: &[PARAM_NAME],
};
const PARAM_NAME: usize = 0;
const PARAM_TYPE: usize = 1;
const PARAM_MODE: usize = 2;
const PARAM_COMPTIME: usize = 3;
const PARAM_SPAN_FILE: usize = 4;
const PARAM_SPAN_START: usize = 5;
const PARAM_SPAN_END: usize = 6;

/// Stored representation of match arm in the extra array.
/// Layout: pattern data + [body: u32]
/// Pattern data varies by kind (see PatternKind enum).

/// Pattern kinds encoded in extra array
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternKind {
    /// Wildcard pattern: [kind, span_start, span_len]
    Wildcard = 0,
    /// Int pattern: [kind, span_start, span_len, value_lo, value_hi]
    Int = 1,
    /// Bool pattern: [kind, span_start, span_len, value]
    Bool = 2,
    /// Path pattern: [kind, span_start, span_len, module, type_name, variant]
    /// module is u32::MAX for None, otherwise an InstRef
    Path = 3,
}

/// Size of each pattern kind in the extra array (including body InstRef).
///
/// The span is stored as three words — start, len, AND file id — so pattern
/// diagnostics in multi-file compilations attribute to the right file
/// (dropping the file id here was why pattern-anchored errors reported
/// "span has an unknown file id", RUE-185).
const PATTERN_WILDCARD_SIZE: u32 = 5; // kind, span_start, span_len, span_file, body
const PATTERN_INT_SIZE: u32 = 8; // kind, span_start, span_len, span_file, value_lo, value_hi, negative, body
const PATTERN_BOOL_SIZE: u32 = 6; // kind, span_start, span_len, span_file, value, body
const PATTERN_PATH_BASE_SIZE: usize = 10;
const DIRECTIVE_HEADER_WORDS: usize = 5;
const RECORD_KIND: usize = 0;
const RECORD_SPAN_START: usize = 1;
const RECORD_SPAN_LEN: usize = 2;
const RECORD_SPAN_FILE: usize = 3;
const MATCH_VALUE_LO_OR_BOOL_OR_BODY: usize = 4;
const MATCH_VALUE_HI_OR_BOOL_BODY: usize = 5;
const MATCH_INT_NEGATIVE_OR_PATH_TYPE: usize = 6;
const MATCH_INT_BODY_OR_PATH_VARIANT: usize = 7;
const MATCH_PATH_BINDING_COUNT: usize = 8;
const MATCH_PATH_BINDINGS_START: usize = 9;
const DIRECTIVE_NAME: usize = 0;
const DIRECTIVE_ARG_COUNT: usize = 4;
const DIRECTIVE_ARGS_START: usize = 5;
// Path patterns are variable-length (RUE-221): kind, span×3, module,
// type_name, variant, n_bindings, bindings…, body = 9 + n_bindings words.
// See `add_match_arms`/`get_match_arms` for the layout.

/// Stored representation of struct field initializer.
/// Layout: [field_name: u32, value: u32] = 2 u32s per field
const FIELD_INIT_SCHEMA: FixedPayloadSchema = FixedPayloadSchema {
    width: 2,
    symbol_offsets: &[FIELD_INIT_NAME],
};
const FIELD_INIT_NAME: usize = 0;
const FIELD_INIT_VALUE: usize = 1;

/// Stored representation of struct field declaration.
/// Layout: [field_name: u32, field_type: u32] = 2 u32s per field
const FIELD_DECL_SCHEMA: FixedPayloadSchema = FixedPayloadSchema {
    width: 2,
    symbol_offsets: &[FIELD_DECL_NAME],
};
const FIELD_DECL_NAME: usize = 0;
const FIELD_DECL_TYPE: usize = 1;

fn decode_symbol_word(word: u32) -> Option<Spur> {
    Spur::try_from_usize(word as usize)
}

fn validated_symbol_word(word: u32) -> Spur {
    match decode_symbol_word(word) {
        Some(symbol) => symbol,
        None => unreachable!("RIR symbol word passed schema validation"),
    }
}

fn encoded_match_record_extent(pattern: &RirPattern) -> Option<usize> {
    match pattern {
        RirPattern::Wildcard(_) => Some(PATTERN_WILDCARD_SIZE as usize),
        RirPattern::Int { .. } => Some(PATTERN_INT_SIZE as usize),
        RirPattern::Bool(..) => Some(PATTERN_BOOL_SIZE as usize),
        RirPattern::Path { bindings, .. } => PATTERN_PATH_BASE_SIZE.checked_add(bindings.len()),
    }
}

fn encoded_directive_record_extent(directive: &RirDirective) -> Option<usize> {
    DIRECTIVE_HEADER_WORDS.checked_add(directive.args.len())
}

fn decoded_match_record_extent(words: &[u32], position: usize) -> Option<usize> {
    match words.get(position + RECORD_KIND).copied()? {
        x if x == PatternKind::Wildcard as u32 => Some(PATTERN_WILDCARD_SIZE as usize),
        x if x == PatternKind::Int as u32 => Some(PATTERN_INT_SIZE as usize),
        x if x == PatternKind::Bool as u32 => Some(PATTERN_BOOL_SIZE as usize),
        x if x == PatternKind::Path as u32 => words
            .get(position + MATCH_PATH_BINDING_COUNT)
            .and_then(|count| PATTERN_PATH_BASE_SIZE.checked_add(*count as usize)),
        _ => None,
    }
}

fn decoded_directive_record_extent(words: &[u32], position: usize) -> Option<usize> {
    words
        .get(position + DIRECTIVE_ARG_COUNT)
        .and_then(|count| DIRECTIVE_HEADER_WORDS.checked_add(*count as usize))
}

fn embedded_span(words: &[u32], position: usize) -> Option<Span> {
    let start = *words.get(position + RECORD_SPAN_START)?;
    let len = *words.get(position + RECORD_SPAN_LEN)?;
    let file = FileId::new(*words.get(position + RECORD_SPAN_FILE)?);
    Some(Span::with_file(file, start, start.checked_add(len)?))
}

fn enum_payload_record(words: &[u32], position: usize) -> Option<(usize, usize)> {
    let count = *words.get(position)? as usize;
    let start = position.checked_add(1)?;
    let end = start.checked_add(count)?;
    (end <= words.len()).then_some((start, end))
}

fn encoded_enum_payload_record_extent<T>(payload: &[T]) -> Option<usize> {
    1usize.checked_add(payload.len())
}

fn decode_match_record(
    words: &[u32],
    position: usize,
) -> Option<(RirPatternView<'_>, InstRef, usize)> {
    let kind = *words.get(position + RECORD_KIND)?;
    let span = embedded_span(words, position)?;
    let extent = decoded_match_record_extent(words, position)?;
    if position.checked_add(extent)? > words.len() {
        return None;
    }
    match kind {
        x if x == PatternKind::Wildcard as u32 => Some((
            RirPatternView::Wildcard(span),
            InstRef::from_raw(*words.get(position + MATCH_VALUE_LO_OR_BOOL_OR_BODY)?),
            extent,
        )),
        x if x == PatternKind::Int as u32 => {
            let negative = *words.get(position + MATCH_INT_NEGATIVE_OR_PATH_TYPE)?;
            if negative > 1 {
                return None;
            }
            Some((
                RirPatternView::Int {
                    value: *words.get(position + MATCH_VALUE_LO_OR_BOOL_OR_BODY)? as u64
                        | ((*words.get(position + MATCH_VALUE_HI_OR_BOOL_BODY)? as u64) << 32),
                    negative: negative != 0,
                    span,
                },
                InstRef::from_raw(*words.get(position + MATCH_INT_BODY_OR_PATH_VARIANT)?),
                extent,
            ))
        }
        x if x == PatternKind::Bool as u32 => {
            let value = *words.get(position + MATCH_VALUE_LO_OR_BOOL_OR_BODY)?;
            if value > 1 {
                return None;
            }
            Some((
                RirPatternView::Bool(value != 0, span),
                InstRef::from_raw(*words.get(position + MATCH_VALUE_HI_OR_BOOL_BODY)?),
                extent,
            ))
        }
        x if x == PatternKind::Path as u32 => {
            let count = *words.get(position + MATCH_PATH_BINDING_COUNT)? as usize;
            let end = position.checked_add(extent)?;
            let optional_ref = |word| (word != u32::MAX).then(|| InstRef::from_raw(word));
            let binding_start = position + MATCH_PATH_BINDINGS_START;
            Some((
                RirPatternView::Path {
                    module: optional_ref(words[position + MATCH_VALUE_LO_OR_BOOL_OR_BODY]),
                    ctor_head: optional_ref(words[position + MATCH_VALUE_HI_OR_BOOL_BODY]),
                    type_name: decode_symbol_word(
                        words[position + MATCH_INT_NEGATIVE_OR_PATH_TYPE],
                    )?,
                    variant: decode_symbol_word(words[position + MATCH_INT_BODY_OR_PATH_VARIANT])?,
                    bindings: RirSlice::new(
                        &words[binding_start..binding_start + count],
                        SYMBOL_SCHEMA.width,
                        |record| validated_symbol_word(record[0]),
                    ),
                    span,
                },
                InstRef::from_raw(words[end - 1]),
                extent,
            ))
        }
        _ => None,
    }
}

fn decode_directive_record(
    words: &[u32],
    position: usize,
) -> Option<(RirDirectiveView<'_>, usize)> {
    let extent = decoded_directive_record_extent(words, position)?;
    let end = position.checked_add(extent)?;
    if end > words.len() {
        return None;
    }
    let args_start = position + DIRECTIVE_ARGS_START;
    Some((
        RirDirectiveView {
            name: decode_symbol_word(words[position + DIRECTIVE_NAME])?,
            args: RirSlice::new(&words[args_start..end], SYMBOL_SCHEMA.width, |record| {
                validated_symbol_word(record[0])
            }),
            span: embedded_span(words, position)?,
        },
        extent,
    ))
}

/// Stored representation of directive in the extra array.
/// Layout: [name: u32, span_start: u32, span_len: u32, args_len: u32, args...]
/// Variable size due to args.

/// The complete canonical RIR for one source revision.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Rir {
    /// All instructions across the canonical module sequence.
    instructions: Vec<Inst>,
    /// Extra data for variable-length instruction payloads.
    extra: Vec<u32>,
    /// Declaration-local structured type syntax referenced by type-bearing
    /// instruction and payload slots. Leaf spellings use the same candidate
    /// symbol universe as the instruction graph; compound syntax is never
    /// rendered into that universe.
    type_syntax: RirTypeSyntaxArena<Spur>,
    /// Set once `add_inst` is asked for an instruction beyond the published
    /// `u32` instruction ceiling (spec Appendix C.6:1). `add_inst` is called
    /// from hundreds of infallible lowering sites, so the ceiling is recorded
    /// here and reported once at the construction/publication boundary
    /// (`AstGen::try_finish_editor`, `RirEditor::capacity_error`) instead of
    /// wrapping an `InstRef` onto the reserved null payload. Spec C.1:2
    /// requires a diagnostic, not a wrapped index.
    instruction_limit_exceeded: bool,
}

/// Mutable construction-phase owner. Payload descriptors never leave this
/// owner through the public API; callers add or replace complete nodes.
///
/// Family identities cannot be interchanged:
///
/// ```compile_fail
/// use rue_rir::{Rir, RirParamsRange};
/// fn wrong_family(rir: &Rir, params: &RirParamsRange) {
///     let _ = rir.call_args(params);
/// }
/// ```
///
/// Raw positions cannot be reconstructed:
///
/// ```compile_fail
/// use rue_rir::RirCallArgsRange;
/// let _ = RirCallArgsRange::from_parts(0, 0);
/// ```
///
/// A descriptor cannot be extracted from a published owner for movement to a
/// different editor:
///
/// ```compile_fail
/// use rue_rir::{InstData, InstRef, Rir, RirCallArgsRange};
/// fn extract(rir: &Rir, inst: InstRef) -> RirCallArgsRange {
///     match &rir.get_inst(inst).data {
///         InstData::Call { args, .. } => *args,
///         _ => panic!("not a call"),
///     }
/// }
/// ```
///
/// Consequently a payload-bearing node cannot be detached from one owner and
/// inserted into another:
///
/// ```compile_fail
/// use rue_rir::{Inst, InstData, InstRef, Rir, RirEditor};
/// fn detach(source: &Rir, destination: &mut RirEditor, inst: InstRef) {
///     let borrowed = source.get_inst(inst);
///     destination.add_inst(Inst { data: borrowed.data, span: borrowed.span });
/// }
/// ```
#[derive(Debug, Default)]
pub struct RirEditor {
    rir: Rir,
    type_syntax: RirTypeSyntaxBuilder<Spur>,
}

/// The destination ranges occupied by one RIR owner after a typed append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RirAppendRange {
    pub instructions: std::ops::Range<u32>,
    pub extra: std::ops::Range<u32>,
}

struct StructMethodsOverride<'a> {
    source_root: InstRef,
    destination_methods: &'a [InstRef],
}

fn remap_call_args(
    args: RirSlice<'_, RirCallArg>,
    remap_ref: impl Fn(InstRef) -> InstRef,
) -> Vec<RirCallArg> {
    args.values()
        .map(|argument| RirCallArg {
            value: remap_ref(argument.value),
            mode: argument.mode,
        })
        .collect()
}

impl RirEditor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Project one parser type directly into this RIR owner's dense structured
    /// type arena. The supplied resolver transports parser-local spellings into
    /// the instruction graph's candidate-local symbol universe.
    pub fn add_parser_type(
        &mut self,
        ty: &rue_parser::ast::TypeExpr,
        resolve: impl Copy + Fn(Spur) -> Spur,
    ) -> Result<crate::RirTypeSyntaxRef, crate::RirTypeSyntaxBuildError> {
        self.type_syntax.push_parser_type(ty, resolve)
    }

    pub fn add_unit_type(
        &mut self,
    ) -> Result<crate::RirTypeSyntaxRef, crate::RirTypeSyntaxBuildError> {
        self.type_syntax.push_unit_type()
    }

    pub fn add_named_type(
        &mut self,
        symbol: Spur,
    ) -> Result<RirTypeSyntaxRef, crate::RirTypeSyntaxBuildError> {
        self.type_syntax.push_named_type(symbol)
    }

    pub(crate) fn into_unvalidated(self) -> Rir {
        let Self {
            mut rir,
            type_syntax,
        } = self;
        rir.type_syntax = type_syntax.finish();
        rir
    }

    /// Finish the owner-mediated editor without contextual validation.
    ///
    /// This is the post-construction counterpart to [`AstGen::finish`], used
    /// by controlled synthesis that must make one final editor-only
    /// replacement before exposing the immutable RIR. Production publication
    /// should prefer [`ValidatedRir::finish`].
    #[doc(hidden)]
    pub fn finish(self) -> Rir {
        self.into_unvalidated()
    }

    fn atomic<T>(
        &mut self,
        build: impl FnOnce(&mut Rir) -> Result<T, RirPayloadBuildError>,
    ) -> Result<T, RirPayloadBuildError> {
        let instruction_len = self.rir.instructions.len();
        let extra_len = self.rir.extra.len();
        match build(&mut self.rir) {
            Ok(value) => Ok(value),
            Err(error) => {
                self.rir.instructions.truncate(instruction_len);
                self.rir.extra.truncate(extra_len);
                Err(error)
            }
        }
    }

    /// Add a payload-free node. Payload-bearing nodes use the atomic methods
    /// below, whose descriptors never escape the editor.
    pub fn add_inst(&mut self, inst: Inst) -> InstRef {
        self.rir.add_inst(inst)
    }

    /// The implementation-limit rejection latched by an infallible
    /// [`Self::add_inst`] that ran past the published instruction ceiling
    /// (spec Appendix C.6:1), if any. Publication boundaries consult this so
    /// the ceiling becomes an `E1401` diagnostic rather than a wrapped
    /// `InstRef` (spec C.1:2).
    pub fn capacity_error(&self) -> Option<RirPayloadBuildError> {
        self.rir.latched_capacity_error()
    }

    pub fn add_intrinsic(
        &mut self,
        name: Spur,
        args: &[InstRef],
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let args = rir.add_intrinsic_args(args)?;
            Ok(rir.add_inst(Inst {
                data: InstData::Intrinsic { name, args },
                span,
            }))
        })
    }

    pub fn add_internal_intrinsic(
        &mut self,
        intrinsic: InternalIntrinsic,
        args: &[InstRef],
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let args = rir.add_internal_intrinsic_args(args)?;
            Ok(rir.add_inst(Inst {
                data: InstData::InternalIntrinsic { intrinsic, args },
                span,
            }))
        })
    }

    pub fn add_block(
        &mut self,
        instructions: &[InstRef],
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let instructions = rir.add_block_insts(instructions)?;
            Ok(rir.add_inst(Inst {
                data: InstData::Block { instructions },
                span,
            }))
        })
    }

    pub fn add_call(
        &mut self,
        name: Spur,
        args: &[RirCallArg],
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let args = rir.add_call_args(args)?;
            Ok(rir.add_inst(Inst {
                data: InstData::Call { name, args },
                span,
            }))
        })
    }

    pub fn add_method_call(
        &mut self,
        receiver: InstRef,
        method: Spur,
        args: &[RirCallArg],
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let args = rir.add_method_args(args)?;
            Ok(rir.add_inst(Inst {
                data: InstData::MethodCall {
                    receiver,
                    method,
                    args,
                },
                span,
            }))
        })
    }

    pub fn add_match(
        &mut self,
        scrutinee: InstRef,
        arms: &[(RirPattern, InstRef)],
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let arms = rir.add_match_arms(arms)?;
            Ok(rir.add_inst(Inst {
                data: InstData::Match { scrutinee, arms },
                span,
            }))
        })
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub fn add_fn_decl(
        &mut self,
        directives: &[RirDirective],
        is_pub: bool,
        is_unchecked: bool,
        is_extern: bool,
        is_c_export: bool,
        name: Spur,
        params: &[RirParam],
        return_type: RirTypeSyntaxRef,
        body: InstRef,
        has_self: bool,
        self_mode: RirParamMode,
        self_is_mut: bool,
        returns_borrow: bool,
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.add_fn_decl_with_return_modes(
            directives,
            is_pub,
            is_unchecked,
            is_extern,
            is_c_export,
            name,
            params,
            return_type,
            body,
            has_self,
            self_mode,
            self_is_mut,
            returns_borrow,
            false,
            span,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_fn_decl_with_return_modes(
        &mut self,
        directives: &[RirDirective],
        is_pub: bool,
        is_unchecked: bool,
        is_extern: bool,
        is_c_export: bool,
        name: Spur,
        params: &[RirParam],
        return_type: RirTypeSyntaxRef,
        body: InstRef,
        has_self: bool,
        self_mode: RirParamMode,
        self_is_mut: bool,
        returns_borrow: bool,
        returns_inout: bool,
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let directives = rir.add_directives(directives)?;
            let params = rir.add_params(params)?;
            Ok(rir.add_inst(Inst {
                data: InstData::FnDecl {
                    directives,
                    is_pub,
                    is_unchecked,
                    is_extern,
                    is_c_export,
                    name,
                    params,
                    return_type,
                    body,
                    has_self,
                    self_mode,
                    self_is_mut,
                    returns_borrow,
                    returns_inout,
                },
                span,
            }))
        })
    }

    pub fn add_const_decl(
        &mut self,
        directives: &[RirDirective],
        is_pub: bool,
        name: Spur,
        ty: Option<RirTypeSyntaxRef>,
        init: InstRef,
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let directives = rir.add_directives(directives)?;
            Ok(rir.add_inst(Inst {
                data: InstData::ConstDecl {
                    directives,
                    is_pub,
                    name,
                    ty,
                    init,
                },
                span,
            }))
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_alloc(
        &mut self,
        directives: &[RirDirective],
        name: Option<Spur>,
        is_mut: bool,
        ty: Option<RirTypeSyntaxRef>,
        init: InstRef,
        iter_elem: bool,
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let directives = rir.add_directives(directives)?;
            Ok(rir.add_inst(Inst {
                data: InstData::Alloc {
                    directives,
                    name,
                    is_mut,
                    ty,
                    init,
                    iter_elem,
                },
                span,
            }))
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_struct_decl(
        &mut self,
        directives: &[RirDirective],
        is_pub: bool,
        is_linear: bool,
        name: Spur,
        fields: &[(Spur, RirTypeSyntaxRef)],
        methods: &[InstRef],
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let directives = rir.add_directives(directives)?;
            let fields = rir.add_struct_fields(fields)?;
            let methods = rir.add_struct_methods(methods)?;
            Ok(rir.add_inst(Inst {
                data: InstData::StructDecl {
                    directives,
                    is_pub,
                    is_linear,
                    name,
                    fields,
                    methods,
                },
                span,
            }))
        })
    }

    pub fn add_struct_init(
        &mut self,
        module: Option<InstRef>,
        ctor_head: Option<InstRef>,
        type_name: Spur,
        fields: &[(Spur, InstRef)],
        shorthand_span: Option<Span>,
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let fields = rir.add_field_inits(fields)?;
            Ok(rir.add_inst(Inst {
                data: InstData::StructInit {
                    module,
                    ctor_head,
                    type_name,
                    fields,
                    shorthand_span,
                },
                span,
            }))
        })
    }

    pub fn add_enum_decl(
        &mut self,
        is_pub: bool,
        is_non_exhaustive: bool,
        name: Spur,
        variants: &[Spur],
        payloads: &[Vec<RirTypeSyntaxRef>],
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            if variants.len() != payloads.len() {
                return Err(RirPayloadBuildError::InvalidBuilderInput {
                    family: RirEnumPayloadsRange::FAMILY,
                    reason: "variant and payload counts differ",
                });
            }
            let variants = rir.add_enum_variants(variants)?;
            let payloads = rir.add_enum_payloads(payloads)?;
            Ok(rir.add_inst(Inst {
                data: InstData::EnumDecl {
                    is_pub,
                    is_non_exhaustive,
                    name,
                    variants,
                    payloads,
                },
                span,
            }))
        })
    }

    pub fn add_array_init(
        &mut self,
        elements: &[InstRef],
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let elements = rir.add_array_elements(elements)?;
            Ok(rir.add_inst(Inst {
                data: InstData::ArrayInit { elements },
                span,
            }))
        })
    }

    pub fn add_anon_struct_type(
        &mut self,
        fields: &[(Spur, RirTypeSyntaxRef)],
        methods: &[InstRef],
        anchor: RirStructuralAnchor,
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            let fields = rir.add_anon_struct_fields(fields)?;
            let methods = rir.add_anon_struct_methods(methods)?;
            Ok(rir.add_inst(Inst {
                data: InstData::AnonStructType {
                    fields,
                    methods,
                    anchor,
                },
                span,
            }))
        })
    }

    pub fn add_anon_enum_type(
        &mut self,
        variants: &[Spur],
        payloads: &[Vec<RirTypeSyntaxRef>],
        anchor: RirStructuralAnchor,
        span: Span,
    ) -> Result<InstRef, RirPayloadBuildError> {
        self.atomic(|rir| {
            if variants.len() != payloads.len() {
                return Err(RirPayloadBuildError::InvalidBuilderInput {
                    family: RirAnonEnumPayloadsRange::FAMILY,
                    reason: "variant and payload counts differ",
                });
            }
            let variants = rir.add_anon_enum_variants(variants)?;
            let payloads = rir.add_anon_enum_payloads(payloads)?;
            Ok(rir.add_inst(Inst {
                data: InstData::AnonEnumType {
                    variants,
                    payloads,
                    anchor,
                },
                span,
            }))
        })
    }

    /// Append an immutable RIR owner while remapping its owner-local symbols.
    ///
    /// Payload descriptors never cross the owner boundary. Every variable
    /// payload is decoded through its typed view and rebuilt by the matching
    /// destination builder; instruction references are translated by the
    /// destination instruction offset.
    pub fn append_remapped(
        &mut self,
        source: &ValidatedRir,
        symbol: impl FnMut(Spur) -> Spur,
    ) -> Result<RirAppendRange, RirPayloadBuildError> {
        self.append_remapped_with_spans(source, symbol, std::convert::identity)
    }

    /// Append an immutable RIR owner while remapping owner-local symbols and
    /// rebinding every embedded source span into the destination file table.
    pub fn append_remapped_with_spans(
        &mut self,
        source: &ValidatedRir,
        symbol: impl FnMut(Spur) -> Spur,
        mut remap_span: impl FnMut(Span) -> Span,
    ) -> Result<RirAppendRange, RirPayloadBuildError> {
        match self.try_append_remapped_with_span_slots(
            source,
            symbol,
            || Ok::<_, std::convert::Infallible>(()),
            |_, span| Ok::<_, std::convert::Infallible>(remap_span(span)),
        ) {
            Ok(range) => Ok(range),
            Err(RirSpanRemapError::Build(error)) => Err(error),
            Err(
                RirSpanRemapError::MalformedPayload(_)
                | RirSpanRemapError::MalformedTypeSyntax(_)
                | RirSpanRemapError::DuplicateSlot(_)
                | RirSpanRemapError::MissingSlot(_)
                | RirSpanRemapError::UnexpectedSlot { .. }
                | RirSpanRemapError::UnconsumedSlot(_)
                | RirSpanRemapError::InvalidInstructionRange(_)
                | RirSpanRemapError::ForeignInstructionEdge { .. },
            ) => unreachable!("validated RIR and canonical span schema must agree"),
            Err(RirSpanRemapError::Checkpoint(error))
            | Err(RirSpanRemapError::Mapping { error, .. }) => match error {},
        }
    }

    /// Atomically append an immutable RIR owner while fallibly remapping each
    /// span by its stable structural slot.
    ///
    /// The callback is evaluated by the canonical span visitor before the
    /// append begins. Checkpoints continue during rebuilding; cancellation or
    /// any error rolls the destination back to its original instruction and
    /// payload lengths.
    pub fn try_append_remapped_with_span_slots<E>(
        &mut self,
        source: &ValidatedRir,
        symbol: impl FnMut(Spur) -> Spur,
        checkpoint: impl FnMut() -> Result<(), E>,
        remap_span: impl FnMut(RirSpanSlot, Span) -> Result<Span, E>,
    ) -> Result<RirAppendRange, RirSpanRemapError<E>> {
        self.try_append_remapped_selection_with_span_slots(
            source, None, None, symbol, checkpoint, remap_span,
        )
    }

    /// Atomically append one methodless `StructDecl` shell while wiring it to
    /// method declarations that have already been composed in this editor.
    ///
    /// Candidate-local AstGen emits a struct shell independently from its
    /// methods. This is the sole composition seam: every directive, field,
    /// symbol, and span is rebuilt through the ordinary typed remapper, while
    /// only the empty methods payload is replaced. The source must contain
    /// exactly the supplied `StructDecl` root and no methods, and every
    /// replacement must name an existing destination `FnDecl`.
    pub fn try_append_methodless_struct_shell_with_methods<E>(
        &mut self,
        source: &ValidatedRir,
        source_root: InstRef,
        destination_methods: &[InstRef],
        symbol: impl FnMut(Spur) -> Spur,
        checkpoint: impl FnMut() -> Result<(), E>,
        remap_span: impl FnMut(RirSpanSlot, Span) -> Result<Span, E>,
    ) -> Result<RirAppendRange, RirSpanRemapError<E>> {
        let invalid = |reason| {
            RirSpanRemapError::Build(RirPayloadBuildError::InvalidBuilderInput {
                family: "struct shell composition",
                reason,
            })
        };
        if usize::try_from(source_root.as_u32())
            .ok()
            .is_none_or(|root| root >= source.len())
        {
            return Err(invalid("source root is outside the candidate shell"));
        }
        let InstData::StructDecl { methods, .. } = &source.get(source_root).data else {
            return Err(invalid("source root is not a struct declaration"));
        };
        if source.struct_methods(methods).len() != 0 {
            return Err(invalid("source struct declaration is not methodless"));
        }
        if source.len() != 1 || source_root.as_u32() != 0 {
            return Err(invalid(
                "source candidate shell is not exactly one struct declaration",
            ));
        }
        if destination_methods.iter().any(|method| {
            !matches!(
                self.rir.instructions.get(method.as_u32() as usize),
                Some(Inst {
                    data: InstData::FnDecl { .. },
                    ..
                })
            )
        }) {
            return Err(invalid(
                "replacement method is not an existing destination function declaration",
            ));
        }
        self.try_append_remapped_selection_with_span_slots(
            source,
            None,
            Some(StructMethodsOverride {
                source_root,
                destination_methods,
            }),
            symbol,
            checkpoint,
            remap_span,
        )
    }

    /// Atomically copy one validated declaration-producer interval.
    ///
    /// Canonical AstGen records this interval around the producer call and the
    /// module publisher proves every child edge remains within it. Projection
    /// work is therefore proportional to this declaration, independent of the
    /// number or size of sibling declarations.
    pub fn try_append_instruction_range_remapped_with_span_slots<E>(
        &mut self,
        source: &ValidatedRir,
        instructions: std::ops::Range<u32>,
        symbol: impl FnMut(Spur) -> Spur,
        mut checkpoint: impl FnMut() -> Result<(), E>,
        remap_span: impl FnMut(RirSpanSlot, Span) -> Result<Span, E>,
    ) -> Result<RirAppendRange, RirSpanRemapError<E>> {
        if instructions.start >= instructions.end
            || usize::try_from(instructions.end)
                .ok()
                .is_none_or(|end| end > source.len())
        {
            return Err(RirSpanRemapError::InvalidInstructionRange(instructions));
        }
        let mut children = Vec::new();
        for ordinal in instructions.clone() {
            checkpoint().map_err(RirSpanRemapError::Checkpoint)?;
            let instruction = InstRef::from_raw(ordinal);
            children.clear();
            source.child_instructions(instruction, &mut children);
            if let Some(child) = children.iter().copied().find(|child| {
                child.as_u32() < instructions.start || child.as_u32() >= instructions.end
            }) {
                return Err(RirSpanRemapError::ForeignInstructionEdge { instruction, child });
            }
        }
        self.try_append_remapped_selection_with_span_slots(
            source,
            Some(instructions),
            None,
            symbol,
            checkpoint,
            remap_span,
        )
    }

    fn try_append_remapped_selection_with_span_slots<E>(
        &mut self,
        source: &ValidatedRir,
        selected: Option<std::ops::Range<u32>>,
        struct_methods_override: Option<StructMethodsOverride<'_>>,
        mut symbol: impl FnMut(Spur) -> Spur,
        mut checkpoint: impl FnMut() -> Result<(), E>,
        mut remap_span: impl FnMut(RirSpanSlot, Span) -> Result<Span, E>,
    ) -> Result<RirAppendRange, RirSpanRemapError<E>> {
        enum CollectError<E> {
            Checkpoint(E),
            Mapping { slot: RirSpanSlot, error: E },
        }

        let instruction_start = u32::try_from(self.rir.instructions.len()).map_err(|_| {
            RirPayloadBuildError::ResourceLimitExceeded {
                family: "instructions",
            }
        })?;
        let source_start = selected.as_ref().map_or(0, |range| range.start);
        let source_end = selected.as_ref().map_or_else(
            || u32::try_from(source.len()).unwrap_or(u32::MAX),
            |range| range.end,
        );
        let source_instructions = source_end - source_start;
        instruction_start.checked_add(source_instructions).ok_or(
            RirPayloadBuildError::ResourceLimitExceeded {
                family: "instructions",
            },
        )?;

        let mut mapped_spans = Vec::new();
        let traversal = source.try_visit_instruction_range_span_slots(
            source_start..source_end,
            || checkpoint().map_err(CollectError::Checkpoint),
            |slot, span| {
                let destination = InstRef::from_raw(
                    instruction_start + (slot.instruction().as_u32() - source_start),
                );
                let destination_slot = RirSpanSlot::new(destination, slot.field());
                let mapped =
                    remap_span(destination_slot, span).map_err(|error| CollectError::Mapping {
                        slot: destination_slot,
                        error,
                    })?;
                mapped_spans.push((slot, mapped));
                Ok(())
            },
        );
        if let Err(error) = traversal {
            return Err(match error {
                RirSpanTraversalError::MalformedPayload(error) => {
                    RirSpanRemapError::MalformedPayload(error)
                }
                RirSpanTraversalError::DuplicateSlot(slot) => {
                    RirSpanRemapError::DuplicateSlot(slot)
                }
                RirSpanTraversalError::Callback(CollectError::Checkpoint(error)) => {
                    RirSpanRemapError::Checkpoint(error)
                }
                RirSpanTraversalError::Callback(CollectError::Mapping { slot, error }) => {
                    RirSpanRemapError::Mapping { slot, error }
                }
            });
        }
        let mut mapped_spans = mapped_spans.into_iter();
        let extra_start = u32::try_from(self.rir.extra.len()).map_err(|_| {
            RirPayloadBuildError::ResourceLimitExceeded {
                family: "payload words",
            }
        })?;
        let source_extra = u32::try_from(source.extra_len()).map_err(|_| {
            RirPayloadBuildError::ResourceLimitExceeded {
                family: "payload words",
            }
        })?;
        extra_start.checked_add(source_extra).ok_or(
            RirPayloadBuildError::ResourceLimitExceeded {
                family: "payload words",
            },
        )?;
        let remap_ref =
            |value: InstRef| InstRef::from_raw(instruction_start + (value.as_u32() - source_start));
        let type_snapshot = self.type_syntax.snapshot();
        let type_map = match self.type_syntax.append_remapped(
            source.type_syntax(),
            |source_symbol| symbol(*source_symbol),
            || checkpoint(),
        ) {
            Ok(type_map) => type_map,
            Err(crate::RirTypeSyntaxAppendError::Malformed(error)) => {
                return Err(RirSpanRemapError::MalformedTypeSyntax(error));
            }
            Err(crate::RirTypeSyntaxAppendError::Checkpoint(error)) => {
                return Err(RirSpanRemapError::Checkpoint(error));
            }
            Err(crate::RirTypeSyntaxAppendError::Build(error)) => {
                return Err(RirSpanRemapError::Build(type_syntax_build_error(error)));
            }
        };
        let remap_type = |reference: RirTypeSyntaxRef| {
            type_map
                .get(reference.index())
                .copied()
                .expect("validated type-syntax reference has a destination")
        };
        let result = (|| {
            for ordinal in source_start..source_end {
                let source_instruction = InstRef::from_raw(ordinal);
                let instruction = source.get(source_instruction);
                checkpoint().map_err(RirSpanRemapError::Checkpoint)?;
                let mut take_span = |field| {
                    let expected = RirSpanSlot::new(source_instruction, field);
                    let Some((actual, span)) = mapped_spans.next() else {
                        return Err(RirSpanRemapError::MissingSlot(expected));
                    };
                    if actual != expected {
                        return Err(RirSpanRemapError::UnexpectedSlot { expected, actual });
                    }
                    Ok(span)
                };
                let span = take_span(RirSpanField::Instruction)?;
                let payload_free = |data| Inst { data, span };
                match &instruction.data {
                    InstData::IntConst(value) => {
                        self.add_inst(payload_free(InstData::IntConst(*value)))
                    }
                    InstData::FloatConst { text } => {
                        self.add_inst(payload_free(InstData::FloatConst {
                            text: symbol(*text),
                        }))
                    }
                    InstData::BoolConst(value) => {
                        self.add_inst(payload_free(InstData::BoolConst(*value)))
                    }
                    InstData::StringConst { content, anchor } => {
                        self.add_inst(payload_free(InstData::StringConst {
                            content: symbol(*content),
                            anchor: anchor.clone(),
                        }))
                    }
                    InstData::UnitConst => self.add_inst(payload_free(InstData::UnitConst)),
                    InstData::Add { lhs, rhs } => self.add_inst(payload_free(InstData::Add {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Sub { lhs, rhs } => self.add_inst(payload_free(InstData::Sub {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Mul { lhs, rhs } => self.add_inst(payload_free(InstData::Mul {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Div { lhs, rhs } => self.add_inst(payload_free(InstData::Div {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Mod { lhs, rhs } => self.add_inst(payload_free(InstData::Mod {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Eq { lhs, rhs } => self.add_inst(payload_free(InstData::Eq {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Ne { lhs, rhs } => self.add_inst(payload_free(InstData::Ne {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Lt { lhs, rhs } => self.add_inst(payload_free(InstData::Lt {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Gt { lhs, rhs } => self.add_inst(payload_free(InstData::Gt {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Le { lhs, rhs } => self.add_inst(payload_free(InstData::Le {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Ge { lhs, rhs } => self.add_inst(payload_free(InstData::Ge {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::And { lhs, rhs } => self.add_inst(payload_free(InstData::And {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Or { lhs, rhs } => self.add_inst(payload_free(InstData::Or {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::BitAnd { lhs, rhs } => {
                        self.add_inst(payload_free(InstData::BitAnd {
                            lhs: remap_ref(*lhs),
                            rhs: remap_ref(*rhs),
                        }))
                    }
                    InstData::BitOr { lhs, rhs } => self.add_inst(payload_free(InstData::BitOr {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::BitXor { lhs, rhs } => {
                        self.add_inst(payload_free(InstData::BitXor {
                            lhs: remap_ref(*lhs),
                            rhs: remap_ref(*rhs),
                        }))
                    }
                    InstData::Shl { lhs, rhs } => self.add_inst(payload_free(InstData::Shl {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Shr { lhs, rhs } => self.add_inst(payload_free(InstData::Shr {
                        lhs: remap_ref(*lhs),
                        rhs: remap_ref(*rhs),
                    })),
                    InstData::Neg { operand } => self.add_inst(payload_free(InstData::Neg {
                        operand: remap_ref(*operand),
                    })),
                    InstData::Not { operand } => self.add_inst(payload_free(InstData::Not {
                        operand: remap_ref(*operand),
                    })),
                    InstData::BitNot { operand } => self.add_inst(payload_free(InstData::BitNot {
                        operand: remap_ref(*operand),
                    })),
                    InstData::Try { operand } => self.add_inst(payload_free(InstData::Try {
                        operand: remap_ref(*operand),
                    })),
                    InstData::Branch {
                        cond,
                        then_block,
                        else_block,
                    } => self.add_inst(payload_free(InstData::Branch {
                        cond: remap_ref(*cond),
                        then_block: remap_ref(*then_block),
                        else_block: else_block.map(remap_ref),
                    })),
                    InstData::Loop { cond, body } => self.add_inst(payload_free(InstData::Loop {
                        cond: remap_ref(*cond),
                        body: remap_ref(*body),
                    })),
                    InstData::InfiniteLoop { body, iter_borrow } => {
                        self.add_inst(payload_free(InstData::InfiniteLoop {
                            body: remap_ref(*body),
                            iter_borrow: iter_borrow.map(&mut symbol),
                        }))
                    }
                    InstData::Match { scrutinee, arms } => {
                        let arms = source
                            .match_arms(arms)
                            .iter()
                            .enumerate()
                            .map(|(arm, (pattern, body))| {
                                let pattern_span = take_span(RirSpanField::MatchPattern {
                                    arm: u32::try_from(arm)
                                        .expect("validated match-arm count is encoded as u32"),
                                })?;
                                let pattern = match pattern {
                                    RirPatternView::Wildcard(_) => {
                                        RirPattern::Wildcard(pattern_span)
                                    }
                                    RirPatternView::Int {
                                        value,
                                        negative,
                                        span: _,
                                    } => RirPattern::Int {
                                        value,
                                        negative,
                                        span: pattern_span,
                                    },
                                    RirPatternView::Bool(value, _) => {
                                        RirPattern::Bool(value, pattern_span)
                                    }
                                    RirPatternView::Path {
                                        module,
                                        ctor_head,
                                        type_name,
                                        variant,
                                        bindings,
                                        span: _,
                                    } => RirPattern::Path {
                                        module: module.map(remap_ref),
                                        ctor_head: ctor_head.map(remap_ref),
                                        type_name: symbol(type_name),
                                        variant: symbol(variant),
                                        bindings: bindings.values().map(&mut symbol).collect(),
                                        span: pattern_span,
                                    },
                                };
                                Ok((pattern, remap_ref(body)))
                            })
                            .collect::<Result<Vec<_>, RirSpanRemapError<E>>>()?;
                        self.add_match(remap_ref(*scrutinee), &arms, span)?
                    }
                    InstData::Break { value } => self.add_inst(payload_free(InstData::Break {
                        value: value.map(remap_ref),
                    })),
                    InstData::Continue => self.add_inst(payload_free(InstData::Continue)),
                    InstData::FnDecl {
                        directives,
                        is_pub,
                        is_unchecked,
                        is_extern,
                        is_c_export,
                        name,
                        params,
                        return_type,
                        body,
                        has_self,
                        self_mode,
                        self_is_mut,
                        returns_borrow,
                        returns_inout,
                    } => {
                        let directives = source
                            .directives(directives)
                            .iter()
                            .enumerate()
                            .map(|(directive, value)| {
                                Ok(RirDirective {
                                    name: symbol(value.name),
                                    args: value.args.values().map(&mut symbol).collect(),
                                    span: take_span(RirSpanField::FunctionDirective {
                                        directive: u32::try_from(directive)
                                            .expect("validated directive count is encoded as u32"),
                                    })?,
                                })
                            })
                            .collect::<Result<Vec<_>, RirSpanRemapError<E>>>()?;
                        let params = source
                            .params(params)
                            .values()
                            .enumerate()
                            .map(|(parameter, param)| {
                                Ok(RirParam {
                                    name: symbol(param.name),
                                    ty: remap_type(param.ty),
                                    span: take_span(RirSpanField::FunctionParameter {
                                        parameter: u32::try_from(parameter)
                                            .expect("validated parameter count is encoded as u32"),
                                    })?,
                                    ..param
                                })
                            })
                            .collect::<Result<Vec<_>, RirSpanRemapError<E>>>()?;
                        self.add_fn_decl_with_return_modes(
                            &directives,
                            *is_pub,
                            *is_unchecked,
                            *is_extern,
                            *is_c_export,
                            symbol(*name),
                            &params,
                            remap_type(*return_type),
                            remap_ref(*body),
                            *has_self,
                            *self_mode,
                            *self_is_mut,
                            *returns_borrow,
                            *returns_inout,
                            span,
                        )?
                    }
                    InstData::ConstDecl {
                        directives,
                        is_pub,
                        name,
                        ty,
                        init,
                    } => {
                        let directives = source
                            .directives(directives)
                            .iter()
                            .enumerate()
                            .map(|(directive, value)| {
                                Ok(RirDirective {
                                    name: symbol(value.name),
                                    args: value.args.values().map(&mut symbol).collect(),
                                    span: take_span(RirSpanField::ConstDirective {
                                        directive: u32::try_from(directive)
                                            .expect("validated directive count is encoded as u32"),
                                    })?,
                                })
                            })
                            .collect::<Result<Vec<_>, RirSpanRemapError<E>>>()?;
                        self.add_const_decl(
                            &directives,
                            *is_pub,
                            symbol(*name),
                            ty.map(remap_type),
                            remap_ref(*init),
                            span,
                        )?
                    }
                    InstData::Call { name, args } => {
                        let args = remap_call_args(source.call_args(args), remap_ref);
                        self.add_call(symbol(*name), &args, span)?
                    }
                    InstData::Intrinsic { name, args } => {
                        let args = source
                            .intrinsic_args(args)
                            .values()
                            .map(remap_ref)
                            .collect::<Vec<_>>();
                        self.add_intrinsic(symbol(*name), &args, span)?
                    }
                    InstData::InternalIntrinsic { intrinsic, args } => {
                        let args = source
                            .internal_intrinsic_args(args)
                            .values()
                            .map(remap_ref)
                            .collect::<Vec<_>>();
                        self.add_internal_intrinsic(*intrinsic, &args, span)?
                    }
                    InstData::TypeIntrinsic { name, type_arg } => {
                        self.add_inst(payload_free(InstData::TypeIntrinsic {
                            name: symbol(*name),
                            type_arg: remap_type(*type_arg),
                        }))
                    }
                    InstData::OffsetOf { type_arg, field } => {
                        self.add_inst(payload_free(InstData::OffsetOf {
                            type_arg: remap_type(*type_arg),
                            field: symbol(*field),
                        }))
                    }
                    InstData::Ret(value) => {
                        self.add_inst(payload_free(InstData::Ret(value.map(remap_ref))))
                    }
                    InstData::Yield(value) => {
                        self.add_inst(payload_free(InstData::Yield(remap_ref(*value))))
                    }
                    InstData::Block { instructions } => {
                        let instructions = source
                            .block_insts(instructions)
                            .values()
                            .map(remap_ref)
                            .collect::<Vec<_>>();
                        self.add_block(&instructions, span)?
                    }
                    InstData::Alloc {
                        directives,
                        name,
                        is_mut,
                        ty,
                        init,
                        iter_elem,
                    } => {
                        let directives = source
                            .directives(directives)
                            .iter()
                            .enumerate()
                            .map(|(directive, value)| {
                                Ok(RirDirective {
                                    name: symbol(value.name),
                                    args: value.args.values().map(&mut symbol).collect(),
                                    span: take_span(RirSpanField::AllocDirective {
                                        directive: u32::try_from(directive)
                                            .expect("validated directive count is encoded as u32"),
                                    })?,
                                })
                            })
                            .collect::<Result<Vec<_>, RirSpanRemapError<E>>>()?;
                        self.add_alloc(
                            &directives,
                            name.map(&mut symbol),
                            *is_mut,
                            ty.map(remap_type),
                            remap_ref(*init),
                            *iter_elem,
                            span,
                        )?
                    }
                    InstData::VarRef { name, anchor } => {
                        self.add_inst(payload_free(InstData::VarRef {
                            name: symbol(*name),
                            anchor: anchor.clone(),
                        }))
                    }
                    InstData::Assign { name, value } => {
                        self.add_inst(payload_free(InstData::Assign {
                            name: symbol(*name),
                            value: remap_ref(*value),
                        }))
                    }
                    InstData::PlaceSet { place, value } => {
                        self.add_inst(payload_free(InstData::PlaceSet {
                            place: remap_ref(*place),
                            value: remap_ref(*value),
                        }))
                    }
                    InstData::StructDecl {
                        directives,
                        is_pub,
                        is_linear,
                        name,
                        fields,
                        methods,
                    } => {
                        let directives = source
                            .directives(directives)
                            .iter()
                            .enumerate()
                            .map(|(directive, value)| {
                                Ok(RirDirective {
                                    name: symbol(value.name),
                                    args: value.args.values().map(&mut symbol).collect(),
                                    span: take_span(RirSpanField::StructDirective {
                                        directive: u32::try_from(directive)
                                            .expect("validated directive count is encoded as u32"),
                                    })?,
                                })
                            })
                            .collect::<Result<Vec<_>, RirSpanRemapError<E>>>()?;
                        let fields = source
                            .struct_fields(fields)
                            .values()
                            .map(|(name, ty)| (symbol(name), remap_type(ty)))
                            .collect::<Vec<_>>();
                        let methods = struct_methods_override
                            .as_ref()
                            .filter(|override_| override_.source_root == source_instruction)
                            .map_or_else(
                                || {
                                    source
                                        .struct_methods(methods)
                                        .values()
                                        .map(remap_ref)
                                        .collect::<Vec<_>>()
                                },
                                |override_| override_.destination_methods.to_vec(),
                            );
                        self.add_struct_decl(
                            &directives,
                            *is_pub,
                            *is_linear,
                            symbol(*name),
                            &fields,
                            &methods,
                            span,
                        )?
                    }
                    InstData::StructInit {
                        module,
                        ctor_head,
                        type_name,
                        fields,
                        shorthand_span,
                    } => {
                        let fields = source
                            .field_inits(fields)
                            .values()
                            .map(|(name, value)| (symbol(name), remap_ref(value)))
                            .collect::<Vec<_>>();
                        self.add_struct_init(
                            module.map(remap_ref),
                            ctor_head.map(remap_ref),
                            symbol(*type_name),
                            &fields,
                            shorthand_span
                                .map(|_| take_span(RirSpanField::StructInitShorthand))
                                .transpose()?,
                            span,
                        )?
                    }
                    InstData::FieldGet { base, field } => {
                        self.add_inst(payload_free(InstData::FieldGet {
                            base: remap_ref(*base),
                            field: symbol(*field),
                        }))
                    }
                    InstData::FieldSet { base, field, value } => {
                        self.add_inst(payload_free(InstData::FieldSet {
                            base: remap_ref(*base),
                            field: symbol(*field),
                            value: remap_ref(*value),
                        }))
                    }
                    InstData::EnumDecl {
                        is_pub,
                        is_non_exhaustive,
                        name,
                        variants: variant_range,
                        payloads,
                    } => {
                        let variants = source
                            .enum_variants(variant_range)
                            .values()
                            .map(&mut symbol)
                            .collect::<Vec<_>>();
                        let payloads = source
                            .enum_payloads(payloads, variant_range)
                            .map(|payload| payload.values().map(remap_type).collect())
                            .collect::<Vec<Vec<_>>>();
                        self.add_enum_decl(
                            *is_pub,
                            *is_non_exhaustive,
                            symbol(*name),
                            &variants,
                            &payloads,
                            span,
                        )?
                    }
                    InstData::EnumVariant {
                        module,
                        type_name,
                        variant,
                    } => self.add_inst(payload_free(InstData::EnumVariant {
                        module: module.map(remap_ref),
                        type_name: symbol(*type_name),
                        variant: symbol(*variant),
                    })),
                    InstData::ArrayInit { elements } => {
                        let elements = source
                            .array_elements(elements)
                            .values()
                            .map(remap_ref)
                            .collect::<Vec<_>>();
                        self.add_array_init(&elements, span)?
                    }
                    InstData::ArrayRepeat { value, count } => {
                        self.add_inst(payload_free(InstData::ArrayRepeat {
                            value: remap_ref(*value),
                            count: match count {
                                RepeatCount::Literal(value) => RepeatCount::Literal(*value),
                                RepeatCount::Named(name) => RepeatCount::Named(symbol(*name)),
                            },
                        }))
                    }
                    InstData::IndexGet { base, index } => {
                        self.add_inst(payload_free(InstData::IndexGet {
                            base: remap_ref(*base),
                            index: remap_ref(*index),
                        }))
                    }
                    InstData::IndexSet { base, index, value } => {
                        self.add_inst(payload_free(InstData::IndexSet {
                            base: remap_ref(*base),
                            index: remap_ref(*index),
                            value: remap_ref(*value),
                        }))
                    }
                    InstData::MethodCall {
                        receiver,
                        method,
                        args,
                    } => {
                        let args = remap_call_args(source.call_args(args), remap_ref);
                        self.add_method_call(remap_ref(*receiver), symbol(*method), &args, span)?
                    }
                    InstData::DropFnDecl { type_name, body } => {
                        self.add_inst(payload_free(InstData::DropFnDecl {
                            type_name: symbol(*type_name),
                            body: remap_ref(*body),
                        }))
                    }
                    InstData::Comptime { expr } => {
                        self.add_inst(payload_free(InstData::Comptime {
                            expr: remap_ref(*expr),
                        }))
                    }
                    InstData::Checked { expr } => self.add_inst(payload_free(InstData::Checked {
                        expr: remap_ref(*expr),
                    })),
                    InstData::TypeConst { type_name } => {
                        self.add_inst(payload_free(InstData::TypeConst {
                            type_name: remap_type(*type_name),
                        }))
                    }
                    InstData::AnonStructType {
                        fields,
                        methods,
                        anchor,
                    } => {
                        let fields = source
                            .anon_struct_fields(fields)
                            .values()
                            .map(|(name, ty)| (symbol(name), remap_type(ty)))
                            .collect::<Vec<_>>();
                        let methods = source
                            .anon_struct_methods(methods)
                            .values()
                            .map(remap_ref)
                            .collect::<Vec<_>>();
                        self.add_anon_struct_type(&fields, &methods, anchor.clone(), span)?
                    }
                    InstData::AnonEnumType {
                        variants: variant_range,
                        payloads,
                        anchor,
                    } => {
                        let variants = source
                            .anon_enum_variants(variant_range)
                            .values()
                            .map(&mut symbol)
                            .collect::<Vec<_>>();
                        let payloads = source
                            .anon_enum_payloads(payloads, variant_range)
                            .map(|payload| payload.values().map(remap_type).collect())
                            .collect::<Vec<Vec<_>>>();
                        self.add_anon_enum_type(&variants, &payloads, anchor.clone(), span)?
                    }
                };
            }
            if let Some((slot, _)) = mapped_spans.next() {
                return Err(RirSpanRemapError::UnconsumedSlot(slot));
            }
            if let Some(error) = self.rir.latched_capacity_error() {
                return Err(RirSpanRemapError::Build(error));
            }
            let instruction_end = u32::try_from(self.rir.instructions.len()).map_err(|_| {
                RirPayloadBuildError::ResourceLimitExceeded {
                    family: "instructions",
                }
            })?;
            let extra_end = u32::try_from(self.rir.extra.len()).map_err(|_| {
                RirPayloadBuildError::ResourceLimitExceeded {
                    family: "payload words",
                }
            })?;
            Ok(RirAppendRange {
                instructions: instruction_start..instruction_end,
                extra: extra_start..extra_end,
            })
        })();
        if result.is_err() {
            self.rir.instructions.truncate(instruction_start as usize);
            self.rir.extra.truncate(extra_start as usize);
            self.type_syntax.rollback(type_snapshot);
        }
        result
    }

    /// Atomically replace an instruction with a compiler-internal intrinsic.
    pub fn replace_internal_intrinsic(
        &mut self,
        instruction: InstRef,
        intrinsic: InternalIntrinsic,
        args: &[InstRef],
    ) -> Result<(), RirPayloadBuildError> {
        if self.rir.instructions.get(instruction.0 as usize).is_none() {
            return Err(RirPayloadBuildError::InvalidBuilderInput {
                family: RirInternalIntrinsicArgsRange::FAMILY,
                reason: "replacement instruction is outside the editor",
            });
        }
        let range = self.rir.add_internal_intrinsic_args(args)?;
        let inst = &mut self.rir.instructions[instruction.0 as usize];
        inst.data = InstData::InternalIntrinsic {
            intrinsic,
            args: range,
        };
        Ok(())
    }

    /// Change function visibility without exposing detached instruction data.
    pub fn set_function_public(
        &mut self,
        instruction: InstRef,
        is_pub: bool,
    ) -> Result<(), RirPayloadBuildError> {
        let Some(inst) = self.rir.instructions.get_mut(instruction.0 as usize) else {
            return Err(RirPayloadBuildError::InvalidBuilderInput {
                family: "function declaration",
                reason: "replacement instruction is outside the editor",
            });
        };
        let InstData::FnDecl { is_pub: slot, .. } = &mut inst.data else {
            return Err(RirPayloadBuildError::InvalidBuilderInput {
                family: "function declaration",
                reason: "visibility replacement requires a function declaration",
            });
        };
        *slot = is_pub;
        Ok(())
    }
}

impl std::ops::Deref for RirEditor {
    type Target = Rir;

    fn deref(&self) -> &Self::Target {
        &self.rir
    }
}

/// Immutable RIR whose complete payload graph passed structural validation.
#[derive(Debug)]
pub struct ValidatedRir(Rir);

impl ValidatedRir {
    /// Consume and validate an editor at the construction/publication boundary.
    pub fn finish(
        editor: RirEditor,
        context: &RirValidationContext<'_>,
    ) -> Result<Self, RirPayloadError> {
        // Structured type syntax is constructed in the editor-owned builder
        // and installed into the immutable RIR only at publication. Validate
        // the published owner, not the editor's still-empty frozen field.
        let rir = editor.into_unvalidated();
        rir.validate_payloads()?;
        rir.validate_context(context)?;
        Ok(Self(rir))
    }

    /// Visit every span-bearing RIR slot through the canonical schema.
    ///
    /// `checkpoint` is called at instruction and payload-record granularity,
    /// allowing cancellation before a large owner is fully traversed.
    pub fn try_visit_span_slots<E>(
        &self,
        checkpoint: impl FnMut() -> Result<(), E>,
        visit: impl FnMut(RirSpanSlot, Span) -> Result<(), E>,
    ) -> Result<(), RirSpanTraversalError<E>> {
        self.0.try_visit_validated_span_slots(checkpoint, visit)
    }

    /// Consume this validated owner and rewrite every canonical span slot in
    /// place, preserving the instruction and payload-word allocations.
    ///
    /// Mapping completes before the first write, so a callback failure cannot
    /// leave a partially rewritten owner observable. The rewritten owner is
    /// validated against `context` before publication. Instruction spans,
    /// match-pattern spans, directives, parameters, and struct-initializer
    /// shorthand spans all pass through the same canonical slot schema used by
    /// [`Self::try_visit_span_slots`].
    pub fn try_rewrite_span_slots<E>(
        mut self,
        context: &RirValidationContext<'_>,
        mut checkpoint: impl FnMut() -> Result<(), E>,
        mut remap_span: impl FnMut(RirSpanSlot, Span) -> Result<Span, E>,
    ) -> Result<Self, RirSpanRemapError<E>> {
        enum CollectError<E> {
            Checkpoint(E),
            Mapping { slot: RirSpanSlot, error: E },
        }

        let mut mapped_spans = Vec::new();
        let traversal = self.try_visit_span_slots(
            || checkpoint().map_err(CollectError::Checkpoint),
            |slot, span| {
                let mapped = remap_span(slot, span)
                    .map_err(|error| CollectError::Mapping { slot, error })?;
                mapped_spans.push((slot, mapped));
                Ok(())
            },
        );
        if let Err(error) = traversal {
            return Err(match error {
                RirSpanTraversalError::MalformedPayload(error) => {
                    RirSpanRemapError::MalformedPayload(error)
                }
                RirSpanTraversalError::DuplicateSlot(slot) => {
                    RirSpanRemapError::DuplicateSlot(slot)
                }
                RirSpanTraversalError::Callback(CollectError::Checkpoint(error)) => {
                    RirSpanRemapError::Checkpoint(error)
                }
                RirSpanTraversalError::Callback(CollectError::Mapping { slot, error }) => {
                    RirSpanRemapError::Mapping { slot, error }
                }
            });
        }

        for (slot, span) in &mapped_spans {
            checkpoint().map_err(RirSpanRemapError::Checkpoint)?;
            let reason = match context
                .source_lengths
                .iter()
                .find(|(file, _)| *file == span.file_id)
            {
                None => Some("span file is outside the canonical source revision"),
                Some((_, source_len)) if span.start > span.end || span.end > *source_len => {
                    Some("span range is outside its canonical source")
                }
                Some(_) => None,
            };
            if let Some(reason) = reason {
                return Err(RirSpanRemapError::MalformedPayload(RirPayloadError::new(
                    "instruction context",
                    slot.instruction().as_u32(),
                    1,
                    None,
                    1,
                    1,
                    reason,
                )));
            }
        }

        self.0
            .try_rewrite_validated_span_slots(&mapped_spans, &mut checkpoint)?;
        self.0
            .validate_payloads()
            .map_err(RirSpanRemapError::MalformedPayload)?;
        self.0
            .validate_context(context)
            .map_err(RirSpanRemapError::MalformedPayload)?;
        Ok(self)
    }

    /// Visit the canonical span schema for one prevalidated contiguous
    /// declaration-producer interval.
    #[doc(hidden)]
    fn try_visit_instruction_range_span_slots<E>(
        &self,
        instructions: std::ops::Range<u32>,
        checkpoint: impl FnMut() -> Result<(), E>,
        visit: impl FnMut(RirSpanSlot, Span) -> Result<(), E>,
    ) -> Result<(), RirSpanTraversalError<E>> {
        self.0
            .try_visit_validated_instruction_range_span_slots(instructions, checkpoint, visit)
    }

    /// Exact equality of the validated dense representation. Candidate body
    /// plans zero every positional span under the reserved structural FileId;
    /// their ordered declaration-relative diagnostic basis is compared by the
    /// owning artifact terminal.
    pub fn exact_eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }

    /// Logical heap bytes retained by this RIR owner, excluding the inline
    /// [`ValidatedRir`] value itself.
    ///
    /// Dense instruction and payload storage is charged by logical length.
    /// Every structural-anchor `Arc` pointee is charged in full along each
    /// reaching instruction path, including when multiple instructions share
    /// one allocation. This matches Rue's allocator-independent retained-value
    /// policy and leaves the enclosing owner responsible for the inline value.
    pub fn retained_allocation_charge(&self) -> u64 {
        let instructions = self.len().saturating_mul(std::mem::size_of::<Inst>()) as u64;
        let payload = self.extra_len().saturating_mul(std::mem::size_of::<u32>()) as u64;
        let type_syntax = self.type_syntax().retained_allocation_charge();
        self.iter().fold(
            instructions
                .saturating_add(payload)
                .saturating_add(type_syntax),
            |charge, (_, instruction)| {
                let anchors = match &instruction.data {
                    InstData::StringConst { anchor, .. }
                    | InstData::AnonStructType { anchor, .. }
                    | InstData::AnonEnumType { anchor, .. } => {
                        std::mem::size_of_val(anchor.segments()) as u64
                    }
                    InstData::VarRef {
                        anchor: Some(anchor),
                        ..
                    } => std::mem::size_of_val(anchor.segments()) as u64,
                    _ => 0,
                };
                charge.saturating_add(anchors)
            },
        )
    }
}

impl std::ops::Deref for ValidatedRir {
    type Target = Rir;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Rir {
    /// Structured type syntax owned by this exact RIR graph.
    pub fn type_syntax(&self) -> &RirTypeSyntaxArena<Spur> {
        &self.type_syntax
    }

    fn symbol_word(family: &'static str, symbol: Spur) -> Result<u32, RirPayloadBuildError> {
        u32::try_from(symbol.into_usize()).map_err(|_| RirPayloadBuildError::InvalidBuilderInput {
            family,
            reason: "symbol index exceeds u32",
        })
    }

    /// Create a new empty RIR.
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate payload structure, then visit every canonical span slot.
    /// Published owners should use [`ValidatedRir::try_visit_span_slots`] to
    /// avoid repeating validation.
    pub fn try_visit_span_slots<E>(
        &self,
        mut checkpoint: impl FnMut() -> Result<(), E>,
        visit: impl FnMut(RirSpanSlot, Span) -> Result<(), E>,
    ) -> Result<(), RirSpanTraversalError<E>> {
        checkpoint().map_err(RirSpanTraversalError::Callback)?;
        self.validate_payloads()
            .map_err(RirSpanTraversalError::MalformedPayload)?;
        self.try_visit_validated_span_slots(checkpoint, visit)
    }

    fn try_visit_validated_span_slots<E>(
        &self,
        checkpoint: impl FnMut() -> Result<(), E>,
        visit: impl FnMut(RirSpanSlot, Span) -> Result<(), E>,
    ) -> Result<(), RirSpanTraversalError<E>> {
        self.try_visit_validated_instruction_range_span_slots(
            0..u32::try_from(self.len()).unwrap_or(u32::MAX),
            checkpoint,
            visit,
        )
    }

    fn try_visit_validated_instruction_range_span_slots<E>(
        &self,
        instructions: std::ops::Range<u32>,
        mut checkpoint: impl FnMut() -> Result<(), E>,
        mut visit: impl FnMut(RirSpanSlot, Span) -> Result<(), E>,
    ) -> Result<(), RirSpanTraversalError<E>> {
        let mut previous_slot = None;

        for ordinal in instructions {
            let instruction = InstRef::from_raw(ordinal);
            let inst = self.get(instruction);
            checkpoint().map_err(RirSpanTraversalError::Callback)?;

            macro_rules! emit {
                ($field:expr, $span:expr) => {{
                    let slot = RirSpanSlot::new(instruction, $field);
                    if previous_slot.is_some_and(|previous| previous >= slot) {
                        return Err(RirSpanTraversalError::DuplicateSlot(slot));
                    }
                    previous_slot = Some(slot);
                    visit(slot, $span).map_err(RirSpanTraversalError::Callback)?;
                }};
            }

            emit!(RirSpanField::Instruction, inst.span);
            match &inst.data {
                InstData::Match { arms, .. } => {
                    for (arm, (pattern, _)) in self.match_arms(arms).iter().enumerate() {
                        checkpoint().map_err(RirSpanTraversalError::Callback)?;
                        emit!(
                            RirSpanField::MatchPattern {
                                arm: u32::try_from(arm)
                                    .expect("validated match-arm count is encoded as u32"),
                            },
                            pattern.span()
                        );
                    }
                }
                InstData::FnDecl {
                    directives, params, ..
                } => {
                    for (directive, value) in self.directives(directives).iter().enumerate() {
                        checkpoint().map_err(RirSpanTraversalError::Callback)?;
                        emit!(
                            RirSpanField::FunctionDirective {
                                directive: u32::try_from(directive)
                                    .expect("validated directive count is encoded as u32"),
                            },
                            value.span
                        );
                    }
                    for (parameter, value) in self.params(params).values().enumerate() {
                        checkpoint().map_err(RirSpanTraversalError::Callback)?;
                        emit!(
                            RirSpanField::FunctionParameter {
                                parameter: u32::try_from(parameter)
                                    .expect("validated parameter count is encoded as u32"),
                            },
                            value.span
                        );
                    }
                }
                InstData::ConstDecl { directives, .. } => {
                    for (directive, value) in self.directives(directives).iter().enumerate() {
                        checkpoint().map_err(RirSpanTraversalError::Callback)?;
                        emit!(
                            RirSpanField::ConstDirective {
                                directive: u32::try_from(directive)
                                    .expect("validated directive count is encoded as u32"),
                            },
                            value.span
                        );
                    }
                }
                InstData::Alloc { directives, .. } => {
                    for (directive, value) in self.directives(directives).iter().enumerate() {
                        checkpoint().map_err(RirSpanTraversalError::Callback)?;
                        emit!(
                            RirSpanField::AllocDirective {
                                directive: u32::try_from(directive)
                                    .expect("validated directive count is encoded as u32"),
                            },
                            value.span
                        );
                    }
                }
                InstData::StructDecl { directives, .. } => {
                    for (directive, value) in self.directives(directives).iter().enumerate() {
                        checkpoint().map_err(RirSpanTraversalError::Callback)?;
                        emit!(
                            RirSpanField::StructDirective {
                                directive: u32::try_from(directive)
                                    .expect("validated directive count is encoded as u32"),
                            },
                            value.span
                        );
                    }
                }
                InstData::StructInit {
                    shorthand_span: Some(span),
                    ..
                } => emit!(RirSpanField::StructInitShorthand, *span),
                _ => {}
            }
        }
        Ok(())
    }

    fn try_rewrite_validated_span_slots<E>(
        &mut self,
        mapped_spans: &[(RirSpanSlot, Span)],
        mut checkpoint: impl FnMut() -> Result<(), E>,
    ) -> Result<(), RirSpanRemapError<E>> {
        let mut mapped_spans = mapped_spans.iter().copied();
        let mut take_span = |expected| {
            let Some((actual, span)) = mapped_spans.next() else {
                return Err(RirSpanRemapError::MissingSlot(expected));
            };
            if actual != expected {
                return Err(RirSpanRemapError::UnexpectedSlot { expected, actual });
            }
            Ok(span)
        };
        let (instructions, extra) = (&mut self.instructions, &mut self.extra);

        for (ordinal, instruction) in instructions.iter_mut().enumerate() {
            checkpoint().map_err(RirSpanRemapError::Checkpoint)?;
            let instruction_ref = InstRef::from_raw(
                u32::try_from(ordinal).expect("validated RIR instruction index fits u32"),
            );
            instruction.span =
                take_span(RirSpanSlot::new(instruction_ref, RirSpanField::Instruction))?;

            let mut rewrite_directives = |range: &RirDirectivesRange,
                                          field: &mut dyn FnMut(u32) -> RirSpanField|
             -> Result<(), RirSpanRemapError<E>> {
                let start = range.start() as usize;
                let end = start + range.extent() as usize;
                let words = &mut extra[start..end];
                if words.is_empty() {
                    return Ok(());
                }
                let count = words[0] as usize;
                let mut position = 1usize;
                for directive in 0..count {
                    checkpoint().map_err(RirSpanRemapError::Checkpoint)?;
                    let extent = decoded_directive_record_extent(words, position)
                        .expect("validated directive record has an exact extent");
                    let span = take_span(RirSpanSlot::new(
                        instruction_ref,
                        field(
                            u32::try_from(directive)
                                .expect("validated directive count is encoded as u32"),
                        ),
                    ))?;
                    words[position + RECORD_SPAN_START] = span.start;
                    words[position + RECORD_SPAN_LEN] = span.end - span.start;
                    words[position + RECORD_SPAN_FILE] = span.file_id.index();
                    position += extent;
                }
                Ok(())
            };

            match &mut instruction.data {
                InstData::Match { arms, .. } => {
                    let start = arms.start() as usize;
                    let end = start + arms.extent() as usize;
                    let words = &mut extra[start..end];
                    if !words.is_empty() {
                        let count = words[0] as usize;
                        let mut position = 1usize;
                        for arm in 0..count {
                            checkpoint().map_err(RirSpanRemapError::Checkpoint)?;
                            let extent = decoded_match_record_extent(words, position)
                                .expect("validated match record has an exact extent");
                            let span = take_span(RirSpanSlot::new(
                                instruction_ref,
                                RirSpanField::MatchPattern {
                                    arm: u32::try_from(arm)
                                        .expect("validated match-arm count is encoded as u32"),
                                },
                            ))?;
                            words[position + RECORD_SPAN_START] = span.start;
                            words[position + RECORD_SPAN_LEN] = span.end - span.start;
                            words[position + RECORD_SPAN_FILE] = span.file_id.index();
                            position += extent;
                        }
                    }
                }
                InstData::FnDecl {
                    directives, params, ..
                } => {
                    rewrite_directives(directives, &mut |directive| {
                        RirSpanField::FunctionDirective { directive }
                    })?;
                    let start = params.start() as usize;
                    let end = start + params.extent() as usize;
                    for (parameter, words) in extra[start..end]
                        .chunks_exact_mut(PARAM_SCHEMA.width)
                        .enumerate()
                    {
                        checkpoint().map_err(RirSpanRemapError::Checkpoint)?;
                        let span = take_span(RirSpanSlot::new(
                            instruction_ref,
                            RirSpanField::FunctionParameter {
                                parameter: u32::try_from(parameter)
                                    .expect("validated parameter count is encoded as u32"),
                            },
                        ))?;
                        words[PARAM_SPAN_FILE] = span.file_id.index();
                        words[PARAM_SPAN_START] = span.start;
                        words[PARAM_SPAN_END] = span.end;
                    }
                }
                InstData::ConstDecl { directives, .. } => {
                    rewrite_directives(directives, &mut |directive| {
                        RirSpanField::ConstDirective { directive }
                    })?;
                }
                InstData::Alloc { directives, .. } => {
                    rewrite_directives(directives, &mut |directive| {
                        RirSpanField::AllocDirective { directive }
                    })?;
                }
                InstData::StructDecl { directives, .. } => {
                    rewrite_directives(directives, &mut |directive| {
                        RirSpanField::StructDirective { directive }
                    })?;
                }
                InstData::StructInit {
                    shorthand_span: Some(span),
                    ..
                } => {
                    *span = take_span(RirSpanSlot::new(
                        instruction_ref,
                        RirSpanField::StructInitShorthand,
                    ))?;
                }
                _ => {}
            }
        }

        if let Some((slot, _)) = mapped_spans.next() {
            return Err(RirSpanRemapError::UnconsumedSlot(slot));
        }
        Ok(())
    }

    /// Add an instruction and return its reference.
    ///
    /// `InstRef` is a `u32` whose maximum value is reserved as the null
    /// payload, so indices `0..=u32::MAX - 1` are addressable and a program
    /// holds at most [`MAX_RIR_ENTRIES_PER_PROGRAM`] instructions. Beyond that
    /// the reference is not representable. This method has hundreds of
    /// infallible callers across AST lowering, so instead of wrapping onto the
    /// null payload (spec C.1:2 forbids that) it latches
    /// `instruction_limit_exceeded` and hands back an already-valid reference;
    /// the construction boundary turns the latch into an `E1401` diagnostic
    /// before the RIR is published.
    pub(crate) fn add_inst(&mut self, inst: Inst) -> InstRef {
        let Ok(index) = u32::try_from(self.instructions.len()) else {
            self.instruction_limit_exceeded = true;
            return InstRef::from_raw(0);
        };
        if index == MAX_RIR_ENTRIES_PER_PROGRAM {
            self.instruction_limit_exceeded = true;
            return InstRef::from_raw(0);
        }
        self.instructions.push(inst);
        InstRef::from_raw(index)
    }

    /// The implementation-limit rejection latched during construction, if the
    /// instruction ceiling was reached. Checked at the publication boundary.
    pub(crate) fn latched_capacity_error(&self) -> Option<RirPayloadBuildError> {
        self.instruction_limit_exceeded
            .then_some(RirPayloadBuildError::ResourceLimitExceeded {
                family: "instructions",
            })
    }

    /// Get an instruction by reference.
    #[inline]
    pub fn get(&self, inst_ref: InstRef) -> &Inst {
        &self.instructions[inst_ref.0 as usize]
    }

    /// The number of instructions.
    #[inline]
    pub fn len(&self) -> usize {
        self.instructions.len()
    }

    /// The number of words in the variable-length payload store.
    #[inline]
    pub fn extra_len(&self) -> usize {
        self.extra.len()
    }

    /// Whether there are no instructions.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.instructions.is_empty()
    }

    /// Iterate over all instructions with their references.
    pub fn iter(&self) -> impl Iterator<Item = (InstRef, &Inst)> {
        self.instructions.iter().enumerate().map(|(i, inst)| {
            (
                InstRef::from_raw(
                    u32::try_from(i).expect("RIR instruction count is bounded by u32"),
                ),
                inst,
            )
        })
    }

    fn append_payload(
        &mut self,
        family: &'static str,
        staged: Vec<u32>,
    ) -> Result<(u32, u32), RirPayloadBuildError> {
        if staged.is_empty() {
            return Ok((0, 0));
        }
        let start = u32::try_from(self.extra.len())
            .map_err(|_| RirPayloadBuildError::ResourceLimitExceeded { family })?;
        let extent = u32::try_from(staged.len())
            .map_err(|_| RirPayloadBuildError::ResourceLimitExceeded { family })?;
        start
            .checked_add(extent)
            .ok_or(RirPayloadBuildError::ResourceLimitExceeded { family })?;
        self.extra
            .try_reserve(staged.len())
            .map_err(|_| RirPayloadBuildError::CapacityFailure { family })?;
        self.extra.extend(staged);
        Ok((start, extent))
    }

    fn append_payload_direct(
        &mut self,
        family: &'static str,
        extent: usize,
        encode: impl FnOnce(&mut Vec<u32>),
    ) -> Result<(u32, u32), RirPayloadBuildError> {
        if extent == 0 {
            return Ok((0, 0));
        }
        let start = u32::try_from(self.extra.len())
            .map_err(|_| RirPayloadBuildError::ResourceLimitExceeded { family })?;
        let extent_u32 = u32::try_from(extent)
            .map_err(|_| RirPayloadBuildError::ResourceLimitExceeded { family })?;
        start
            .checked_add(extent_u32)
            .ok_or(RirPayloadBuildError::ResourceLimitExceeded { family })?;
        self.extra
            .try_reserve(extent)
            .map_err(|_| RirPayloadBuildError::CapacityFailure { family })?;
        let old_len = self.extra.len();
        encode(&mut self.extra);
        debug_assert_eq!(self.extra.len(), old_len + extent);
        Ok((start, extent_u32))
    }

    fn payload_words<'a, R>(
        &'a self,
        range: &R,
        parts: impl FnOnce(&R) -> (u32, u32, &'static str),
    ) -> Result<&'a [u32], RirPayloadError> {
        let (start, extent, family) = parts(range);
        if extent == 0 {
            if start == 0 {
                return Ok(&[]);
            }
            return Err(rir_payload_error! {
                family,
                start,
                extent,
                record: None,
                expected: 0,
                actual: 0,
                reason: "noncanonical empty range",
            });
        }
        let end = start.checked_add(extent).ok_or_else(|| {
            rir_payload_error! {
                family,
                start,
                extent,
                record: None,
                expected: extent as usize,
                actual: 0,
                reason: "range end overflows u32",
            }
        })?;
        self.extra.get(start as usize..end as usize).ok_or_else(|| {
            rir_payload_error! {
                family,
                start,
                extent,
                record: None,
                expected: extent as usize,
                actual: self.extra.len().saturating_sub(start as usize).min(extent as usize),
                reason: "range is outside the word store",
            }
        })
    }

    fn validate_fixed<R>(
        &self,
        range: &R,
        width: usize,
        parts: impl FnOnce(&R) -> (u32, u32, &'static str),
    ) -> Result<(), RirPayloadError> {
        let (start, extent, family) = parts(range);
        let words = self.payload_words(range, |_| (start, extent, family))?;
        if words.len() % width != 0 {
            return Err(rir_payload_error! {
                family,
                start,
                extent,
                record: Some((words.len() / width) as u32),
                expected: width,
                actual: words.len() % width,
                reason: "payload ends in a partial record",
            });
        }
        Ok(())
    }

    fn validate_fixed_symbols<R>(
        &self,
        range: &R,
        schema: FixedPayloadSchema,
        parts: impl FnOnce(&R) -> (u32, u32, &'static str),
    ) -> Result<(), RirPayloadError> {
        let (start, extent, family) = parts(range);
        self.validate_fixed(range, schema.width, |_| (start, extent, family))?;
        for (record, words) in self
            .payload_words(range, |_| (start, extent, family))?
            .chunks_exact(schema.width)
            .enumerate()
        {
            if schema
                .symbol_offsets
                .iter()
                .any(|offset| decode_symbol_word(words[*offset]).is_none())
            {
                return Err(rir_payload_error! {
                    family,
                    start,
                    extent,
                    record: Some(u32::try_from(record).unwrap_or(u32::MAX)),
                    expected: schema.width,
                    actual: schema.width,
                    reason: "symbol word is not representable",
                });
            }
        }
        Ok(())
    }

    fn validate_variable_records<R>(
        &self,
        range: &R,
        parts: impl FnOnce(&R) -> (u32, u32, &'static str),
        record_extent: impl Fn(&[u32], usize) -> Option<usize>,
    ) -> Result<(), RirPayloadError> {
        let (start, extent, family) = parts(range);
        let words = self.payload_words(range, |_| (start, extent, family))?;
        if words.is_empty() {
            return Ok(());
        }
        let count = words[0] as usize;
        let mut pos = 1usize;
        for record in 0..count {
            let Some(width) = record_extent(words, pos) else {
                let remaining = words.len().saturating_sub(pos);
                let (expected, reason) = if family == RirMatchArmsRange::FAMILY {
                    match words.get(pos + RECORD_KIND).copied() {
                        None => (RECORD_KIND + 1, "record header is truncated"),
                        Some(kind) if kind == PatternKind::Path as u32 => (
                            MATCH_PATH_BINDING_COUNT + 1,
                            "path record header is truncated",
                        ),
                        Some(kind)
                            if kind != PatternKind::Wildcard as u32
                                && kind != PatternKind::Int as u32
                                && kind != PatternKind::Bool as u32 =>
                        {
                            (1, "invalid pattern kind")
                        }
                        Some(_) => (1, "record extent is not representable"),
                    }
                } else {
                    (
                        DIRECTIVE_ARG_COUNT + 1,
                        "directive record header is truncated",
                    )
                };
                return Err(rir_payload_error! {
                    family,
                    start,
                    extent,
                    record: Some(record as u32),
                    expected: expected,
                    actual: remaining.min(expected),
                    reason: reason,
                });
            };
            pos = pos.checked_add(width).ok_or_else(|| {
                rir_payload_error! {
                    family,
                    start,
                    extent,
                    record: Some(record as u32),
                    expected: width,
                    actual: words.len().saturating_sub(pos),
                    reason: "record end overflows usize",
                }
            })?;
            if pos > words.len() {
                return Err(rir_payload_error! {
                    family,
                    start,
                    extent,
                    record: Some(record as u32),
                    expected: width,
                    actual: words.len().saturating_sub(pos - width),
                    reason: "record body is truncated",
                });
            }
        }
        if pos != words.len() {
            return Err(rir_payload_error! {
                family,
                start,
                extent,
                record: Some(count as u32),
                expected: 0,
                actual: words.len().saturating_sub(pos),
                reason: "trailing words after final record",
            });
        }
        Ok(())
    }

    /// Validate every variable-length payload before publishing this RIR.
    pub fn validate_payloads(&self) -> Result<(), RirPayloadError> {
        for (_, inst) in self.iter() {
            match &inst.data {
                InstData::Match { arms, .. } => self.validate_match_range(arms)?,
                InstData::FnDecl {
                    directives, params, ..
                } => {
                    self.validate_directive_range(directives)?;
                    self.validate_fixed_symbols(params, PARAM_SCHEMA, |r| {
                        (r.start(), r.extent(), RirParamsRange::FAMILY)
                    })?;
                    for (record, words) in self
                        .payload_words(params, |r| (r.start(), r.extent(), RirParamsRange::FAMILY))?
                        .chunks_exact(PARAM_SCHEMA.width)
                        .enumerate()
                    {
                        if words[PARAM_MODE] > RirParamMode::Borrow as u32 {
                            return Err(rir_payload_error! {
                                family: RirParamsRange::FAMILY,
                                start: params.start(),
                                extent: params.extent(),
                                record: Some(record as u32),
                                expected: PARAM_SCHEMA.width,
                                actual: PARAM_SCHEMA.width,
                                reason: "invalid parameter mode",
                            });
                        }
                        if words[PARAM_COMPTIME] > 1 {
                            return Err(rir_payload_error! {
                                family: RirParamsRange::FAMILY,
                                start: params.start(),
                                extent: params.extent(),
                                record: Some(record as u32),
                                expected: PARAM_SCHEMA.width,
                                actual: PARAM_SCHEMA.width,
                                reason: "invalid comptime flag",
                            });
                        }
                    }
                }
                InstData::ConstDecl { directives, .. } | InstData::Alloc { directives, .. } => {
                    self.validate_directive_range(directives)?
                }
                InstData::Call { args, .. } | InstData::MethodCall { args, .. } => {
                    self.validate_fixed(args, CALL_ARG_SCHEMA.width, |r| {
                        (r.start(), r.extent(), RirCallArgsRange::FAMILY)
                    })?;
                    for (record, words) in self
                        .payload_words(args, |r| (r.start(), r.extent(), RirCallArgsRange::FAMILY))?
                        .chunks_exact(CALL_ARG_SCHEMA.width)
                        .enumerate()
                    {
                        if words[CALL_ARG_MODE] > RirArgMode::Borrow as u32 {
                            return Err(rir_payload_error! {
                                family: RirCallArgsRange::FAMILY,
                                start: args.start(),
                                extent: args.extent(),
                                record: Some(record as u32),
                                expected: CALL_ARG_SCHEMA.width,
                                actual: CALL_ARG_SCHEMA.width,
                                reason: "invalid argument mode",
                            });
                        }
                    }
                }
                InstData::Intrinsic { args, .. } => {
                    self.validate_fixed(args, REF_SCHEMA.width, |r| {
                        (r.start(), r.extent(), RirIntrinsicArgsRange::FAMILY)
                    })?
                }
                InstData::InternalIntrinsic { args, .. } => {
                    self.validate_fixed(args, REF_SCHEMA.width, |r| {
                        (r.start(), r.extent(), RirInternalIntrinsicArgsRange::FAMILY)
                    })?
                }
                InstData::Block { instructions } => {
                    self.validate_fixed(instructions, REF_SCHEMA.width, |r| {
                        (r.start(), r.extent(), RirBlockInstsRange::FAMILY)
                    })?
                }
                InstData::StructDecl {
                    directives,
                    fields,
                    methods,
                    ..
                } => {
                    self.validate_directive_range(directives)?;
                    self.validate_fixed_symbols(fields, FIELD_DECL_SCHEMA, |r| {
                        (r.start(), r.extent(), RirStructFieldsRange::FAMILY)
                    })?;
                    self.validate_fixed(methods, REF_SCHEMA.width, |r| {
                        (r.start(), r.extent(), RirStructMethodsRange::FAMILY)
                    })?;
                }
                InstData::StructInit { fields, .. } => {
                    self.validate_fixed_symbols(fields, FIELD_INIT_SCHEMA, |r| {
                        (r.start(), r.extent(), RirFieldInitsRange::FAMILY)
                    })?
                }
                InstData::EnumDecl {
                    variants, payloads, ..
                } => {
                    self.validate_fixed_symbols(variants, SYMBOL_SCHEMA, |r| {
                        (r.start(), r.extent(), RirEnumVariantsRange::FAMILY)
                    })?;
                    self.validate_enum_payload_range(payloads, variants.extent() as usize)?;
                }
                InstData::ArrayInit { elements } => {
                    self.validate_fixed(elements, REF_SCHEMA.width, |r| {
                        (r.start(), r.extent(), RirArrayElemsRange::FAMILY)
                    })?
                }
                InstData::AnonStructType {
                    fields, methods, ..
                } => {
                    self.validate_fixed_symbols(fields, FIELD_DECL_SCHEMA, |r| {
                        (r.start(), r.extent(), RirAnonStructFieldsRange::FAMILY)
                    })?;
                    self.validate_fixed(methods, REF_SCHEMA.width, |r| {
                        (r.start(), r.extent(), RirAnonStructMethodsRange::FAMILY)
                    })?;
                }
                InstData::AnonEnumType {
                    variants, payloads, ..
                } => {
                    self.validate_fixed_symbols(variants, SYMBOL_SCHEMA, |r| {
                        (r.start(), r.extent(), RirAnonEnumVariantsRange::FAMILY)
                    })?;
                    self.validate_anon_enum_payload_range(payloads, variants.extent() as usize)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn validate_match_range(&self, range: &RirMatchArmsRange) -> Result<(), RirPayloadError> {
        self.validate_variable_records(
            range,
            |r| (r.start(), r.extent(), RirMatchArmsRange::FAMILY),
            decoded_match_record_extent,
        )?;
        let words = self.payload_words(range, |r| {
            (r.start(), r.extent(), RirMatchArmsRange::FAMILY)
        })?;
        if words.is_empty() {
            return Ok(());
        }
        let mut position = 1usize;
        for record in 0..words[0] as usize {
            let record_width = decoded_match_record_extent(words, position)
                .expect("variable-record validation established match extent");
            let kind = words[position + RECORD_KIND];
            if embedded_span(words, position).is_none() {
                return Err(rir_payload_error! {
                    family: RirMatchArmsRange::FAMILY,
                    start: range.start(),
                    extent: range.extent(),
                    record: Some(u32::try_from(record).unwrap_or(u32::MAX)),
                    expected: record_width,
                    actual: record_width,
                    reason: "pattern span overflows u32",
                });
            }
            if kind == PatternKind::Int as u32 {
                if words[position + MATCH_INT_NEGATIVE_OR_PATH_TYPE] > 1 {
                    return Err(rir_payload_error! {
                        family: RirMatchArmsRange::FAMILY,
                        start: range.start(),
                        extent: range.extent(),
                        record: Some(u32::try_from(record).unwrap_or(u32::MAX)),
                        expected: record_width,
                        actual: record_width,
                        reason: "invalid integer-sign flag",
                    });
                }
            } else if kind == PatternKind::Bool as u32 {
                if words[position + MATCH_VALUE_LO_OR_BOOL_OR_BODY] > 1 {
                    return Err(rir_payload_error! {
                        family: RirMatchArmsRange::FAMILY,
                        start: range.start(),
                        extent: range.extent(),
                        record: Some(u32::try_from(record).unwrap_or(u32::MAX)),
                        expected: record_width,
                        actual: record_width,
                        reason: "invalid boolean scalar",
                    });
                }
            } else if kind == PatternKind::Path as u32 {
                let binding_count = words[position + MATCH_PATH_BINDING_COUNT] as usize;
                let binding_start = position + MATCH_PATH_BINDINGS_START;
                let binding_end = binding_start + binding_count;
                if words[binding_start..binding_end]
                    .iter()
                    .any(|word| decode_symbol_word(*word).is_none())
                {
                    return Err(rir_payload_error! {
                        family: RirMatchArmsRange::FAMILY,
                        start: range.start(),
                        extent: range.extent(),
                        record: Some(u32::try_from(record).unwrap_or(u32::MAX)),
                        expected: record_width,
                        actual: record_width,
                        reason: "symbol word is not representable",
                    });
                }
            }
            let (_, _, width) = decode_match_record(words, position).ok_or_else(|| {
                rir_payload_error! {
                    family: RirMatchArmsRange::FAMILY,
                    start: range.start(),
                    extent: range.extent(),
                    record: Some(u32::try_from(record).unwrap_or(u32::MAX)),
                    expected: record_width,
                    actual: record_width,
                    reason: "match record failed schema decoding",
                }
            })?;
            position += width;
        }
        Ok(())
    }

    /// Validate every context-dependent handle after structural payload
    /// validation and before any infallible borrowing view is published.
    pub fn validate_context(
        &self,
        context: &RirValidationContext<'_>,
    ) -> Result<(), RirPayloadError> {
        fn error(index: u32, reason: &'static str) -> RirPayloadError {
            rir_payload_error! {
                family: "instruction context",
                start: index,
                extent: 1,
                record: None,
                expected: 1,
                actual: 1,
                reason,
            }
        }
        let check_ref = |index: u32, reference: InstRef| {
            if (reference.as_u32() as usize) < self.instructions.len() {
                Ok(())
            } else {
                Err(error(index, "instruction reference is outside the owner"))
            }
        };
        let check_symbol = |index: u32, symbol: Spur| {
            if symbol.into_usize() < context.symbol_count {
                Ok(())
            } else {
                Err(error(index, "symbol is outside the canonical interner"))
            }
        };
        let check_span = |index: u32, span: Span| {
            let Some((_, source_len)) = context
                .source_lengths
                .iter()
                .find(|(file, _)| *file == span.file_id)
            else {
                return Err(error(
                    index,
                    "span file is outside the canonical source revision",
                ));
            };
            if span.start <= span.end && span.end <= *source_len {
                Ok(())
            } else {
                Err(error(index, "span range is outside its canonical source"))
            }
        };

        self.type_syntax
            .validate_with_symbol(|symbol| symbol.into_usize() < context.symbol_count)
            .map_err(|failure| {
                error(
                    failure.node.map_or(u32::MAX, RirTypeSyntaxRef::as_u32),
                    failure.reason,
                )
            })?;

        for (instruction, inst) in self.iter() {
            let index = instruction.as_u32();
            check_span(index, inst.span)?;
            macro_rules! refs {
                ($($reference:expr),* $(,)?) => {{ $(check_ref(index, $reference)?;)* }};
            }
            macro_rules! symbols {
                ($($symbol:expr),* $(,)?) => {{ $(check_symbol(index, $symbol)?;)* }};
            }
            macro_rules! types {
                ($($reference:expr),* $(,)?) => {{
                    $(if $reference.index() >= self.type_syntax.nodes().len() {
                        return Err(error(index, "type-syntax reference is outside the owner"));
                    })*
                }};
            }
            match &inst.data {
                InstData::IntConst(_)
                | InstData::BoolConst(_)
                | InstData::UnitConst
                | InstData::Continue => {}
                InstData::StringConst {
                    content: symbol, ..
                }
                | InstData::FloatConst { text: symbol }
                | InstData::VarRef { name: symbol, .. } => symbols!(*symbol),
                InstData::TypeConst { type_name } => types!(*type_name),
                InstData::Add { lhs, rhs }
                | InstData::Sub { lhs, rhs }
                | InstData::Mul { lhs, rhs }
                | InstData::Div { lhs, rhs }
                | InstData::Mod { lhs, rhs }
                | InstData::Eq { lhs, rhs }
                | InstData::Ne { lhs, rhs }
                | InstData::Lt { lhs, rhs }
                | InstData::Gt { lhs, rhs }
                | InstData::Le { lhs, rhs }
                | InstData::Ge { lhs, rhs }
                | InstData::And { lhs, rhs }
                | InstData::Or { lhs, rhs }
                | InstData::BitAnd { lhs, rhs }
                | InstData::BitOr { lhs, rhs }
                | InstData::BitXor { lhs, rhs }
                | InstData::Shl { lhs, rhs }
                | InstData::Shr { lhs, rhs } => refs!(*lhs, *rhs),
                InstData::Neg { operand }
                | InstData::Not { operand }
                | InstData::BitNot { operand }
                | InstData::Try { operand }
                | InstData::Comptime { expr: operand }
                | InstData::Checked { expr: operand } => refs!(*operand),
                InstData::Branch {
                    cond,
                    then_block,
                    else_block,
                } => {
                    refs!(*cond, *then_block);
                    if let Some(reference) = else_block {
                        refs!(*reference);
                    }
                }
                InstData::Loop { cond, body } => refs!(*cond, *body),
                InstData::InfiniteLoop { body, iter_borrow } => {
                    refs!(*body);
                    if let Some(symbol) = iter_borrow {
                        symbols!(*symbol);
                    }
                }
                InstData::Match { scrutinee, arms } => {
                    refs!(*scrutinee);
                    for (pattern, body) in self.match_arms(arms).iter() {
                        refs!(body);
                        check_span(index, pattern.span())?;
                        if let RirPatternView::Path {
                            module,
                            ctor_head,
                            type_name,
                            variant,
                            bindings,
                            ..
                        } = pattern
                        {
                            if let Some(reference) = module {
                                refs!(reference);
                            }
                            if let Some(reference) = ctor_head {
                                refs!(reference);
                            }
                            symbols!(type_name, variant);
                            for binding in bindings {
                                symbols!(binding);
                            }
                        }
                    }
                }
                InstData::Break { value } | InstData::Ret(value) => {
                    if let Some(reference) = value {
                        refs!(*reference);
                    }
                }
                InstData::Yield(value) => refs!(*value),
                InstData::FnDecl {
                    directives,
                    name,
                    params,
                    return_type,
                    body,
                    ..
                } => {
                    symbols!(*name);
                    types!(*return_type);
                    refs!(*body);
                    for directive in self.directives(directives).iter() {
                        symbols!(directive.name);
                        check_span(index, directive.span)?;
                        for arg in directive.args {
                            symbols!(arg);
                        }
                    }
                    for param in self.params(params) {
                        symbols!(param.name);
                        types!(param.ty);
                        check_span(index, param.span)?;
                    }
                }
                InstData::ConstDecl {
                    directives,
                    name,
                    ty,
                    init,
                    ..
                } => {
                    symbols!(*name);
                    if let Some(symbol) = ty {
                        types!(*symbol);
                    }
                    refs!(*init);
                    for directive in self.directives(directives).iter() {
                        symbols!(directive.name);
                        check_span(index, directive.span)?;
                        for arg in directive.args {
                            symbols!(arg);
                        }
                    }
                }
                InstData::Call { name, args } => {
                    symbols!(*name);
                    for arg in self.call_args(args) {
                        refs!(arg.value);
                    }
                }
                InstData::Intrinsic { name, args } => {
                    symbols!(*name);
                    for reference in self.intrinsic_args(args) {
                        refs!(reference);
                    }
                }
                InstData::InternalIntrinsic { args, .. } => {
                    for reference in self.internal_intrinsic_args(args) {
                        refs!(reference);
                    }
                }
                InstData::TypeIntrinsic { name, type_arg } => {
                    symbols!(*name);
                    types!(*type_arg);
                }
                InstData::OffsetOf { type_arg, field } => {
                    types!(*type_arg);
                    symbols!(*field);
                }
                InstData::Block { instructions } => {
                    for reference in self.block_insts(instructions) {
                        refs!(reference);
                    }
                }
                InstData::Alloc {
                    directives,
                    name,
                    ty,
                    init,
                    ..
                } => {
                    if let Some(symbol) = name {
                        symbols!(*symbol);
                    }
                    if let Some(symbol) = ty {
                        types!(*symbol);
                    }
                    refs!(*init);
                    for directive in self.directives(directives).iter() {
                        symbols!(directive.name);
                        check_span(index, directive.span)?;
                        for arg in directive.args {
                            symbols!(arg);
                        }
                    }
                }
                InstData::Assign { name, value } => {
                    symbols!(*name);
                    refs!(*value);
                }
                InstData::PlaceSet { place, value } => {
                    refs!(*place);
                    refs!(*value);
                }
                InstData::StructDecl {
                    directives,
                    name,
                    fields,
                    methods,
                    ..
                } => {
                    symbols!(*name);
                    for (field, ty) in self.struct_fields(fields) {
                        symbols!(field);
                        types!(ty);
                    }
                    for reference in self.struct_methods(methods) {
                        refs!(reference);
                    }
                    for directive in self.directives(directives).iter() {
                        symbols!(directive.name);
                        check_span(index, directive.span)?;
                        for arg in directive.args {
                            symbols!(arg);
                        }
                    }
                }
                InstData::StructInit {
                    module,
                    ctor_head,
                    type_name,
                    fields,
                    shorthand_span,
                } => {
                    if let Some(reference) = module {
                        refs!(*reference);
                    }
                    if let Some(reference) = ctor_head {
                        refs!(*reference);
                    }
                    symbols!(*type_name);
                    for (field, value) in self.field_inits(fields) {
                        symbols!(field);
                        refs!(value);
                    }
                    if let Some(span) = shorthand_span {
                        check_span(index, *span)?;
                    }
                }
                InstData::FieldGet { base, field } => {
                    refs!(*base);
                    symbols!(*field);
                }
                InstData::FieldSet { base, field, value } => {
                    refs!(*base, *value);
                    symbols!(*field);
                }
                InstData::EnumDecl {
                    name,
                    variants,
                    payloads,
                    ..
                } => {
                    symbols!(*name);
                    for variant in self.enum_variants(variants) {
                        symbols!(variant);
                    }
                    for payload in self.enum_payloads(payloads, variants) {
                        for ty in payload {
                            types!(ty);
                        }
                    }
                }
                InstData::EnumVariant {
                    module,
                    type_name,
                    variant,
                } => {
                    if let Some(reference) = module {
                        refs!(*reference);
                    }
                    symbols!(*type_name, *variant);
                }
                InstData::ArrayInit { elements } => {
                    for reference in self.array_elements(elements) {
                        refs!(reference);
                    }
                }
                InstData::ArrayRepeat { value, count } => {
                    refs!(*value);
                    if let RepeatCount::Named(symbol) = count {
                        symbols!(*symbol);
                    }
                }
                InstData::IndexGet {
                    base,
                    index: subscript,
                } => refs!(*base, *subscript),
                InstData::IndexSet {
                    base,
                    index: subscript,
                    value,
                } => refs!(*base, *subscript, *value),
                InstData::MethodCall {
                    receiver,
                    method,
                    args,
                } => {
                    refs!(*receiver);
                    symbols!(*method);
                    for arg in self.call_args(args) {
                        refs!(arg.value);
                    }
                }
                InstData::DropFnDecl { type_name, body } => {
                    symbols!(*type_name);
                    refs!(*body);
                }
                InstData::AnonStructType {
                    fields, methods, ..
                } => {
                    for (field, ty) in self.anon_struct_fields(fields) {
                        symbols!(field);
                        types!(ty);
                    }
                    for reference in self.anon_struct_methods(methods) {
                        refs!(reference);
                    }
                }
                InstData::AnonEnumType {
                    variants, payloads, ..
                } => {
                    for variant in self.anon_enum_variants(variants) {
                        symbols!(variant);
                    }
                    for payload in self.anon_enum_payloads(payloads, variants) {
                        for ty in payload {
                            types!(ty);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Append every instruction `instruction` references — its operands, block
    /// members, match-arm bodies, call arguments, and nested declaration
    /// bodies — to `out`.
    ///
    /// The match is exhaustive with no catch-all arm, so a new [`InstData`]
    /// variant does not compile until its operands are listed here. That is
    /// what lets a consumer walk an instruction's whole subtree without
    /// silently missing a syntactic form; the accessor-body legality rules
    /// (spec 6.6:6, 6.6:7) decide containment questions this way, before any
    /// type resolves.
    ///
    /// Declaration-forming variants report their nested bodies as children. A
    /// consumer that must not cross a declaration boundary — a nested `fn` owns
    /// its own body — stops on the declaration instruction itself rather than
    /// filtering the children out here.
    pub fn child_instructions(&self, instruction: InstRef, out: &mut Vec<InstRef>) {
        match &self.get(instruction).data {
            InstData::IntConst(_)
            | InstData::FloatConst { .. }
            | InstData::BoolConst(_)
            | InstData::UnitConst
            | InstData::Continue
            | InstData::StringConst { .. }
            | InstData::VarRef { .. }
            | InstData::TypeConst { .. }
            | InstData::TypeIntrinsic { .. }
            | InstData::OffsetOf { .. }
            | InstData::EnumDecl { .. }
            | InstData::AnonEnumType { .. } => {}
            InstData::Add { lhs, rhs }
            | InstData::Sub { lhs, rhs }
            | InstData::Mul { lhs, rhs }
            | InstData::Div { lhs, rhs }
            | InstData::Mod { lhs, rhs }
            | InstData::Eq { lhs, rhs }
            | InstData::Ne { lhs, rhs }
            | InstData::Lt { lhs, rhs }
            | InstData::Gt { lhs, rhs }
            | InstData::Le { lhs, rhs }
            | InstData::Ge { lhs, rhs }
            | InstData::And { lhs, rhs }
            | InstData::Or { lhs, rhs }
            | InstData::BitAnd { lhs, rhs }
            | InstData::BitOr { lhs, rhs }
            | InstData::BitXor { lhs, rhs }
            | InstData::Shl { lhs, rhs }
            | InstData::Shr { lhs, rhs } => out.extend([*lhs, *rhs]),
            InstData::Neg { operand }
            | InstData::Not { operand }
            | InstData::BitNot { operand }
            | InstData::Try { operand }
            | InstData::Comptime { expr: operand }
            | InstData::Checked { expr: operand }
            | InstData::Yield(operand) => out.push(*operand),
            InstData::Branch {
                cond,
                then_block,
                else_block,
            } => {
                out.extend([*cond, *then_block]);
                out.extend(else_block.iter().copied());
            }
            InstData::Loop { cond, body } => out.extend([*cond, *body]),
            InstData::InfiniteLoop { body, .. } => out.push(*body),
            InstData::Match { scrutinee, arms } => {
                out.push(*scrutinee);
                for (pattern, body) in self.match_arms(arms).iter() {
                    out.push(body);
                    if let RirPatternView::Path {
                        module, ctor_head, ..
                    } = pattern
                    {
                        out.extend(module);
                        out.extend(ctor_head);
                    }
                }
            }
            InstData::Break { value } | InstData::Ret(value) => out.extend(value.iter().copied()),
            InstData::FnDecl { body, .. } | InstData::DropFnDecl { body, .. } => out.push(*body),
            InstData::ConstDecl { init, .. } | InstData::Alloc { init, .. } => out.push(*init),
            InstData::Call { args, .. } => {
                out.extend(self.call_args(args).values().map(|arg| arg.value))
            }
            InstData::MethodCall { receiver, args, .. } => {
                out.push(*receiver);
                out.extend(self.call_args(args).values().map(|arg| arg.value));
            }
            InstData::Intrinsic { args, .. } => out.extend(self.intrinsic_args(args).values()),
            InstData::InternalIntrinsic { args, .. } => {
                out.extend(self.internal_intrinsic_args(args).values())
            }
            InstData::Block { instructions } => out.extend(self.block_insts(instructions).values()),
            InstData::Assign { value, .. } => out.push(*value),
            InstData::PlaceSet { place, value } => out.extend([*place, *value]),
            InstData::StructDecl { methods, .. } => {
                out.extend(self.struct_methods(methods).values())
            }
            InstData::AnonStructType { methods, .. } => {
                out.extend(self.anon_struct_methods(methods).values())
            }
            InstData::StructInit {
                module,
                ctor_head,
                fields,
                ..
            } => {
                out.extend(module.iter().copied());
                out.extend(ctor_head.iter().copied());
                out.extend(self.field_inits(fields).values().map(|(_, value)| value));
            }
            InstData::FieldGet { base, .. } => out.push(*base),
            InstData::FieldSet { base, value, .. } => out.extend([*base, *value]),
            InstData::EnumVariant { module, .. } => out.extend(module.iter().copied()),
            InstData::ArrayInit { elements } => out.extend(self.array_elements(elements).values()),
            InstData::ArrayRepeat { value, .. } => out.push(*value),
            InstData::IndexGet {
                base,
                index: subscript,
            } => out.extend([*base, *subscript]),
            InstData::IndexSet {
                base,
                index: subscript,
                value,
            } => out.extend([*base, *subscript, *value]),
        }
    }

    fn validate_directive_range(&self, range: &RirDirectivesRange) -> Result<(), RirPayloadError> {
        self.validate_variable_records(
            range,
            |r| (r.start(), r.extent(), RirDirectivesRange::FAMILY),
            decoded_directive_record_extent,
        )?;
        let words = self.payload_words(range, |r| {
            (r.start(), r.extent(), RirDirectivesRange::FAMILY)
        })?;
        if words.is_empty() {
            return Ok(());
        }
        let mut position = 1usize;
        for record in 0..words[0] as usize {
            let record_width = decoded_directive_record_extent(words, position)
                .expect("variable-record validation established directive extent");
            if embedded_span(words, position).is_none() {
                return Err(rir_payload_error! {
                    family: RirDirectivesRange::FAMILY,
                    start: range.start(),
                    extent: range.extent(),
                    record: Some(u32::try_from(record).unwrap_or(u32::MAX)),
                    expected: record_width,
                    actual: record_width,
                    reason: "directive span overflows u32",
                });
            }
            let arg_count = words[position + DIRECTIVE_ARG_COUNT] as usize;
            let args_start = position + DIRECTIVE_ARGS_START;
            let args_end = args_start + arg_count;
            if words[args_start..args_end]
                .iter()
                .any(|word| decode_symbol_word(*word).is_none())
            {
                return Err(rir_payload_error! {
                    family: RirDirectivesRange::FAMILY,
                    start: range.start(),
                    extent: range.extent(),
                    record: Some(u32::try_from(record).unwrap_or(u32::MAX)),
                    expected: record_width,
                    actual: record_width,
                    reason: "symbol word is not representable",
                });
            }
            let (_, record_extent) = decode_directive_record(words, position).ok_or_else(|| {
                rir_payload_error! {
                    family: RirDirectivesRange::FAMILY,
                    start: range.start(),
                    extent: range.extent(),
                    record: Some(u32::try_from(record).unwrap_or(u32::MAX)),
                    expected: record_width,
                    actual: record_width,
                    reason: "directive record failed schema decoding",
                }
            })?;
            let end = position + record_extent;
            position = end;
        }
        Ok(())
    }

    fn validate_enum_payload_words<R>(
        &self,
        range: &R,
        variants: usize,
        parts: impl FnOnce(&R) -> (u32, u32, &'static str),
    ) -> Result<(), RirPayloadError> {
        let (start, extent, family) = parts(range);
        let words = self.payload_words(range, |_| (start, extent, family))?;
        if words.is_empty() {
            return Ok(());
        }
        let mut pos = 0usize;
        for record in 0..variants {
            if words.get(pos).is_none() {
                return Err(rir_payload_error! {
                    family,
                    start,
                    extent,
                    record: Some(record as u32),
                    expected: 1,
                    actual: 0,
                    reason: "missing variant payload record",
                });
            }
            let record_width = 1usize.saturating_add(words[pos] as usize);
            let Some((payload_start, end)) = enum_payload_record(words, pos) else {
                return Err(rir_payload_error! {
                    family,
                    start,
                    extent,
                    record: Some(record as u32),
                    expected: record_width,
                    actual: words.len().saturating_sub(pos).min(record_width),
                    reason: "variant payload record is truncated",
                });
            };
            // Payload words are declaration-local structured type references.
            // Their owner bounds are checked by `validate_context` after the
            // variable-width envelope has been proven complete here.
            let _ = payload_start;
            pos = end;
        }
        if pos != words.len() {
            return Err(rir_payload_error! {
                family,
                start,
                extent,
                record: Some(variants as u32),
                expected: 0,
                actual: words.len().saturating_sub(pos),
                reason: "trailing words after variant payloads",
            });
        }
        Ok(())
    }

    fn validate_enum_payload_range(
        &self,
        range: &RirEnumPayloadsRange,
        variants: usize,
    ) -> Result<(), RirPayloadError> {
        self.validate_enum_payload_words(range, variants, |r| {
            (r.start(), r.extent(), RirEnumPayloadsRange::FAMILY)
        })
    }

    fn validate_anon_enum_payload_range(
        &self,
        range: &RirAnonEnumPayloadsRange,
        variants: usize,
    ) -> Result<(), RirPayloadError> {
        self.validate_enum_payload_words(range, variants, |r| {
            (r.start(), r.extent(), RirAnonEnumPayloadsRange::FAMILY)
        })
    }

    fn add_ref_words<R>(
        &mut self,
        family: &'static str,
        refs: &[InstRef],
        make: impl FnOnce(u32, u32) -> R,
    ) -> Result<R, RirPayloadBuildError> {
        let (start, extent) = self.append_payload_direct(family, refs.len(), |words| {
            words.extend(refs.iter().map(|reference| reference.as_u32()));
        })?;
        Ok(make(start, extent))
    }

    pub(crate) fn add_intrinsic_args(
        &mut self,
        refs: &[InstRef],
    ) -> Result<RirIntrinsicArgsRange, RirPayloadBuildError> {
        self.add_ref_words(
            RirIntrinsicArgsRange::FAMILY,
            refs,
            RirIntrinsicArgsRange::from_parts,
        )
    }
    pub(crate) fn add_internal_intrinsic_args(
        &mut self,
        refs: &[InstRef],
    ) -> Result<RirInternalIntrinsicArgsRange, RirPayloadBuildError> {
        self.add_ref_words(
            RirInternalIntrinsicArgsRange::FAMILY,
            refs,
            RirInternalIntrinsicArgsRange::from_parts,
        )
    }
    pub(crate) fn add_block_insts(
        &mut self,
        refs: &[InstRef],
    ) -> Result<RirBlockInstsRange, RirPayloadBuildError> {
        self.add_ref_words(
            RirBlockInstsRange::FAMILY,
            refs,
            RirBlockInstsRange::from_parts,
        )
    }
    pub(crate) fn add_struct_methods(
        &mut self,
        refs: &[InstRef],
    ) -> Result<RirStructMethodsRange, RirPayloadBuildError> {
        self.add_ref_words(
            RirStructMethodsRange::FAMILY,
            refs,
            RirStructMethodsRange::from_parts,
        )
    }
    pub(crate) fn add_anon_struct_methods(
        &mut self,
        refs: &[InstRef],
    ) -> Result<RirAnonStructMethodsRange, RirPayloadBuildError> {
        self.add_ref_words(
            RirAnonStructMethodsRange::FAMILY,
            refs,
            RirAnonStructMethodsRange::from_parts,
        )
    }
    pub(crate) fn add_array_elements(
        &mut self,
        refs: &[InstRef],
    ) -> Result<RirArrayElemsRange, RirPayloadBuildError> {
        self.add_ref_words(
            RirArrayElemsRange::FAMILY,
            refs,
            RirArrayElemsRange::from_parts,
        )
    }

    fn ref_view<R>(
        &self,
        range: &R,
        parts: impl FnOnce(&R) -> (u32, u32, &'static str),
    ) -> RirSlice<'_, InstRef> {
        RirSlice::new(
            self.payload_words(range, parts)
                .expect("validated RIR range"),
            REF_SCHEMA.width,
            |record| InstRef::from_raw(record[0]),
        )
    }
    pub fn intrinsic_args(&self, range: &RirIntrinsicArgsRange) -> RirSlice<'_, InstRef> {
        self.ref_view(range, |r| {
            (r.start(), r.extent(), RirIntrinsicArgsRange::FAMILY)
        })
    }
    pub fn internal_intrinsic_args(
        &self,
        range: &RirInternalIntrinsicArgsRange,
    ) -> RirSlice<'_, InstRef> {
        self.ref_view(range, |r| {
            (r.start(), r.extent(), RirInternalIntrinsicArgsRange::FAMILY)
        })
    }
    pub fn block_insts(&self, range: &RirBlockInstsRange) -> RirSlice<'_, InstRef> {
        self.ref_view(range, |r| {
            (r.start(), r.extent(), RirBlockInstsRange::FAMILY)
        })
    }

    /// Number of instructions in a block payload without constructing a
    /// borrowing view over the complete payload.
    pub fn block_inst_count(&self, range: &RirBlockInstsRange) -> usize {
        self.payload_words(range, |r| {
            (r.start(), r.extent(), RirBlockInstsRange::FAMILY)
        })
        .expect("validated RIR range")
        .len()
    }

    /// Retrieves one instruction from a block payload in constant time.
    ///
    /// Recursive consumers use this accessor when retaining a borrowing
    /// [`RirSlice`] across the recursive call would unnecessarily require an
    /// owned copy of the block's instruction list.
    pub fn block_inst(&self, range: &RirBlockInstsRange, index: usize) -> Option<InstRef> {
        self.payload_words(range, |r| {
            (r.start(), r.extent(), RirBlockInstsRange::FAMILY)
        })
        .expect("validated RIR range")
        .get(index)
        .copied()
        .map(InstRef::from_raw)
    }
    pub fn struct_methods(&self, range: &RirStructMethodsRange) -> RirSlice<'_, InstRef> {
        self.ref_view(range, |r| {
            (r.start(), r.extent(), RirStructMethodsRange::FAMILY)
        })
    }
    pub fn anon_struct_methods(&self, range: &RirAnonStructMethodsRange) -> RirSlice<'_, InstRef> {
        self.ref_view(range, |r| {
            (r.start(), r.extent(), RirAnonStructMethodsRange::FAMILY)
        })
    }
    pub fn array_elements(&self, range: &RirArrayElemsRange) -> RirSlice<'_, InstRef> {
        self.ref_view(range, |r| {
            (r.start(), r.extent(), RirArrayElemsRange::FAMILY)
        })
    }

    /// Store RirCallArgs and return (start, len).
    /// Layout: [value: u32, mode: u32] per arg
    fn add_call_arg_words<R>(
        &mut self,
        family: &'static str,
        args: &[RirCallArg],
        make: impl FnOnce(u32, u32) -> R,
    ) -> Result<R, RirPayloadBuildError> {
        let words = args
            .len()
            .checked_mul(CALL_ARG_SCHEMA.width)
            .ok_or(RirPayloadBuildError::ResourceLimitExceeded { family })?;
        let (start, extent) = self.append_payload_direct(family, words, |data| {
            for arg in args {
                data.extend([arg.value.as_u32(), arg.mode.as_u32()]);
            }
        })?;
        Ok(make(start, extent))
    }

    pub(crate) fn add_call_args(
        &mut self,
        args: &[RirCallArg],
    ) -> Result<RirCallArgsRange, RirPayloadBuildError> {
        self.add_call_arg_words(RirCallArgsRange::FAMILY, args, RirCallArgsRange::from_parts)
    }
    pub(crate) fn add_method_args(
        &mut self,
        args: &[RirCallArg],
    ) -> Result<RirCallArgsRange, RirPayloadBuildError> {
        self.add_call_args(args)
    }

    /// Retrieve RirCallArgs from the extra array.
    fn call_arg_view<R>(
        &self,
        range: &R,
        parts: impl FnOnce(&R) -> (u32, u32, &'static str),
    ) -> RirSlice<'_, RirCallArg> {
        let data = self
            .payload_words(range, parts)
            .expect("validated RIR range");
        RirSlice::new(data, CALL_ARG_SCHEMA.width, |chunk| RirCallArg {
            value: InstRef::from_raw(chunk[CALL_ARG_VALUE]),
            mode: RirArgMode::from_u32(chunk[CALL_ARG_MODE]),
        })
    }
    pub fn call_args(&self, range: &RirCallArgsRange) -> RirSlice<'_, RirCallArg> {
        self.call_arg_view(range, |r| (r.start(), r.extent(), RirCallArgsRange::FAMILY))
    }

    /// Store RirParams and return (start, len).
    /// Layout: [name: u32, ty: u32, mode: u32, is_comptime: u32,
    ///          span.file_id: u32, span.start: u32, span.end: u32] per param
    pub(crate) fn add_params(
        &mut self,
        params: &[RirParam],
    ) -> Result<RirParamsRange, RirPayloadBuildError> {
        let words = params.len().checked_mul(PARAM_SCHEMA.width).ok_or(
            RirPayloadBuildError::ResourceLimitExceeded {
                family: RirParamsRange::FAMILY,
            },
        )?;
        for param in params {
            Self::symbol_word(RirParamsRange::FAMILY, param.name)?;
        }
        let (start, extent) =
            self.append_payload_direct(RirParamsRange::FAMILY, words, |data| {
                for param in params {
                    data.extend([
                        u32::try_from(param.name.into_usize()).expect("prevalidated symbol"),
                        param.ty.as_u32(),
                        param.mode.as_u32(),
                        param.is_comptime as u32,
                        param.span.file_id.index(),
                        param.span.start,
                        param.span.end,
                    ]);
                }
            })?;
        Ok(RirParamsRange::from_parts(start, extent))
    }

    /// Retrieve RirParams from the extra array.
    pub fn params(&self, range: &RirParamsRange) -> RirSlice<'_, RirParam> {
        let data = self
            .payload_words(range, |r| (r.start(), r.extent(), RirParamsRange::FAMILY))
            .expect("validated RIR range");
        RirSlice::new(data, PARAM_SCHEMA.width, |chunk| RirParam {
            name: validated_symbol_word(chunk[PARAM_NAME]),
            ty: RirTypeSyntaxRef::from_u32(chunk[PARAM_TYPE]),
            mode: RirParamMode::from_u32(chunk[PARAM_MODE]),
            is_comptime: chunk[PARAM_COMPTIME] != 0,
            span: Span::with_file(
                FileId::new(chunk[PARAM_SPAN_FILE]),
                chunk[PARAM_SPAN_START],
                chunk[PARAM_SPAN_END],
            ),
        })
    }

    /// Store match arms (pattern + body pairs) and return (start, arm_count).
    /// Each arm is stored with variable size depending on pattern kind.
    pub(crate) fn add_match_arms(
        &mut self,
        arms: &[(RirPattern, InstRef)],
    ) -> Result<RirMatchArmsRange, RirPayloadBuildError> {
        if arms.is_empty() {
            return Ok(RirMatchArmsRange::from_parts(0, 0));
        }
        let count =
            u32::try_from(arms.len()).map_err(|_| RirPayloadBuildError::ResourceLimitExceeded {
                family: RirMatchArmsRange::FAMILY,
            })?;
        let exact_words = arms.iter().try_fold(1usize, |total, (pattern, _)| {
            let width = encoded_match_record_extent(pattern).ok_or(
                RirPayloadBuildError::ResourceLimitExceeded {
                    family: RirMatchArmsRange::FAMILY,
                },
            )?;
            total
                .checked_add(width)
                .ok_or(RirPayloadBuildError::ResourceLimitExceeded {
                    family: RirMatchArmsRange::FAMILY,
                })
        })?;
        for (pattern, _) in arms {
            if let RirPattern::Path {
                type_name,
                variant,
                bindings,
                ..
            } = pattern
            {
                u32::try_from(bindings.len()).map_err(|_| {
                    RirPayloadBuildError::ResourceLimitExceeded {
                        family: RirMatchArmsRange::FAMILY,
                    }
                })?;
                Self::symbol_word(RirMatchArmsRange::FAMILY, *type_name)?;
                Self::symbol_word(RirMatchArmsRange::FAMILY, *variant)?;
                for binding in bindings {
                    Self::symbol_word(RirMatchArmsRange::FAMILY, *binding)?;
                }
            }
        }
        let (start, extent) =
            self.append_payload_direct(RirMatchArmsRange::FAMILY, exact_words, |words| {
                words.push(count);
                for (pattern, body) in arms {
                    match pattern {
                        RirPattern::Wildcard(span) => {
                            words.extend([
                                PatternKind::Wildcard as u32,
                                span.start(),
                                span.len(),
                                span.file_id.index(),
                                body.as_u32(),
                            ]);
                        }
                        RirPattern::Int {
                            value,
                            negative,
                            span,
                        } => {
                            words.extend([
                                PatternKind::Int as u32,
                                span.start(),
                                span.len(),
                                span.file_id.index(),
                            ]);
                            // Store u64 magnitude as two u32s (little-endian) plus sign flag
                            words.extend([
                                *value as u32,
                                (*value >> 32) as u32,
                                u32::from(*negative),
                                body.as_u32(),
                            ]);
                        }
                        RirPattern::Bool(value, span) => {
                            words.extend([
                                PatternKind::Bool as u32,
                                span.start(),
                                span.len(),
                                span.file_id.index(),
                                u32::from(*value),
                                body.as_u32(),
                            ]);
                        }
                        RirPattern::Path {
                            module,
                            ctor_head,
                            type_name,
                            variant,
                            bindings,
                            span,
                        } => {
                            words.extend([
                                PatternKind::Path as u32,
                                span.start(),
                                span.len(),
                                span.file_id.index(),
                            ]);
                            // Store module as u32::MAX for None, otherwise the InstRef
                            words.push(module.map_or(u32::MAX, |r| r.as_u32()));
                            // Store ctor_head (inline type-constructor pattern head,
                            // RUE-596) the same way — u32::MAX for None.
                            words.push(ctor_head.map_or(u32::MAX, |r| r.as_u32()));
                            words.push(
                                u32::try_from(type_name.into_usize()).expect("prevalidated symbol"),
                            );
                            words.push(
                                u32::try_from(variant.into_usize()).expect("prevalidated symbol"),
                            );
                            // Variable-length payload bindings (RUE-221): a count
                            // followed by the binding symbols, then the body last.
                            words.push(u32::try_from(bindings.len()).expect("prevalidated length"));
                            for b in bindings {
                                words.push(
                                    u32::try_from(b.into_usize()).expect("prevalidated symbol"),
                                );
                            }
                            words.push(body.as_u32());
                        }
                    }
                }
            })?;
        Ok(RirMatchArmsRange::from_parts(start, extent))
    }

    /// Retrieve match arms from the extra array.
    pub fn match_arms(&self, range: &RirMatchArmsRange) -> RirMatchArms<'_> {
        let words = self
            .payload_words(range, |r| {
                (r.start(), r.extent(), RirMatchArmsRange::FAMILY)
            })
            .expect("validated RIR range");
        if words.is_empty() {
            return RirMatchArms {
                extra: words,
                start: 0,
                len: 0,
            };
        }
        let view = RirMatchArms {
            extra: words,
            start: 1,
            len: words[0] as usize,
        };
        view.iter().for_each(drop);
        view
    }

    /// Store field initializers (name, value) and return (start, len).
    /// Layout: [name: u32, value: u32] per field
    pub(crate) fn add_field_inits(
        &mut self,
        fields: &[(Spur, InstRef)],
    ) -> Result<RirFieldInitsRange, RirPayloadBuildError> {
        let words = fields.len().checked_mul(FIELD_INIT_SCHEMA.width).ok_or(
            RirPayloadBuildError::ResourceLimitExceeded {
                family: RirFieldInitsRange::FAMILY,
            },
        )?;
        for (name, _) in fields {
            Self::symbol_word(RirFieldInitsRange::FAMILY, *name)?;
        }
        let (start, extent) =
            self.append_payload_direct(RirFieldInitsRange::FAMILY, words, |data| {
                for (name, value) in fields {
                    data.extend([
                        u32::try_from(name.into_usize()).expect("prevalidated symbol"),
                        value.as_u32(),
                    ]);
                }
            })?;
        Ok(RirFieldInitsRange::from_parts(start, extent))
    }

    /// Retrieve field initializers from the extra array.
    pub fn field_inits(&self, range: &RirFieldInitsRange) -> RirSlice<'_, (Spur, InstRef)> {
        let data = self
            .payload_words(range, |r| {
                (r.start(), r.extent(), RirFieldInitsRange::FAMILY)
            })
            .expect("validated RIR range");
        RirSlice::new(data, FIELD_INIT_SCHEMA.width, |chunk| {
            (
                validated_symbol_word(chunk[FIELD_INIT_NAME]),
                InstRef::from_raw(chunk[FIELD_INIT_VALUE]),
            )
        })
    }

    /// Store field declarations (name, type) and return (start, len).
    /// Layout: [name: u32, type: u32] per field
    fn add_field_decl_words<R>(
        &mut self,
        family: &'static str,
        fields: &[(Spur, RirTypeSyntaxRef)],
        make: impl FnOnce(u32, u32) -> R,
    ) -> Result<R, RirPayloadBuildError> {
        let words = fields
            .len()
            .checked_mul(FIELD_DECL_SCHEMA.width)
            .ok_or(RirPayloadBuildError::ResourceLimitExceeded { family })?;
        for (name, _) in fields {
            Self::symbol_word(family, *name)?;
        }
        let (start, extent) = self.append_payload_direct(family, words, |data| {
            for (name, ty) in fields {
                data.extend([
                    u32::try_from(name.into_usize()).expect("prevalidated symbol"),
                    ty.as_u32(),
                ]);
            }
        })?;
        Ok(make(start, extent))
    }
    pub(crate) fn add_struct_fields(
        &mut self,
        fields: &[(Spur, RirTypeSyntaxRef)],
    ) -> Result<RirStructFieldsRange, RirPayloadBuildError> {
        self.add_field_decl_words(
            RirStructFieldsRange::FAMILY,
            fields,
            RirStructFieldsRange::from_parts,
        )
    }
    pub(crate) fn add_anon_struct_fields(
        &mut self,
        fields: &[(Spur, RirTypeSyntaxRef)],
    ) -> Result<RirAnonStructFieldsRange, RirPayloadBuildError> {
        self.add_field_decl_words(
            RirAnonStructFieldsRange::FAMILY,
            fields,
            RirAnonStructFieldsRange::from_parts,
        )
    }

    /// Retrieve field declarations from the extra array.
    fn field_decl_view<R>(
        &self,
        range: &R,
        parts: impl FnOnce(&R) -> (u32, u32, &'static str),
    ) -> RirSlice<'_, (Spur, RirTypeSyntaxRef)> {
        let data = self
            .payload_words(range, parts)
            .expect("validated RIR range");
        RirSlice::new(data, FIELD_DECL_SCHEMA.width, |chunk| {
            (
                validated_symbol_word(chunk[FIELD_DECL_NAME]),
                RirTypeSyntaxRef::from_u32(chunk[FIELD_DECL_TYPE]),
            )
        })
    }
    pub fn struct_fields(
        &self,
        range: &RirStructFieldsRange,
    ) -> RirSlice<'_, (Spur, RirTypeSyntaxRef)> {
        self.field_decl_view(range, |r| {
            (r.start(), r.extent(), RirStructFieldsRange::FAMILY)
        })
    }
    pub fn anon_struct_fields(
        &self,
        range: &RirAnonStructFieldsRange,
    ) -> RirSlice<'_, (Spur, RirTypeSyntaxRef)> {
        self.field_decl_view(range, |r| {
            (r.start(), r.extent(), RirAnonStructFieldsRange::FAMILY)
        })
    }

    /// Store directives and return (start, directive_count).
    /// Layout: [name: u32, span_start: u32, span_len: u32, span_file: u32, args_len: u32, args...] per directive
    ///
    /// The span is stored as three words — start, len, AND file id — so
    /// directive-anchored diagnostics in multi-file compilations attribute
    /// to the right file (dropping the file id here was the same loss shape
    /// as the RUE-185 pattern-span bug; RUE-189).
    pub(crate) fn add_directives(
        &mut self,
        directives: &[RirDirective],
    ) -> Result<RirDirectivesRange, RirPayloadBuildError> {
        if directives.is_empty() {
            return Ok(RirDirectivesRange::from_parts(0, 0));
        }
        let count = u32::try_from(directives.len()).map_err(|_| {
            RirPayloadBuildError::ResourceLimitExceeded {
                family: RirDirectivesRange::FAMILY,
            }
        })?;
        let exact_words = directives.iter().try_fold(1usize, |total, directive| {
            total
                .checked_add(encoded_directive_record_extent(directive).ok_or(
                    RirPayloadBuildError::ResourceLimitExceeded {
                        family: RirDirectivesRange::FAMILY,
                    },
                )?)
                .ok_or(RirPayloadBuildError::ResourceLimitExceeded {
                    family: RirDirectivesRange::FAMILY,
                })
        })?;
        let mut staged = Vec::new();
        staged.try_reserve_exact(exact_words).map_err(|_| {
            RirPayloadBuildError::CapacityFailure {
                family: RirDirectivesRange::FAMILY,
            }
        })?;
        staged.push(count);
        for directive in directives {
            staged.push(Self::symbol_word(
                RirDirectivesRange::FAMILY,
                directive.name,
            )?);
            staged.push(directive.span.start());
            staged.push(directive.span.len());
            staged.push(directive.span.file_id.index());
            staged.push(u32::try_from(directive.args.len()).map_err(|_| {
                RirPayloadBuildError::ResourceLimitExceeded {
                    family: RirDirectivesRange::FAMILY,
                }
            })?);
            for arg in &directive.args {
                staged.push(Self::symbol_word(RirDirectivesRange::FAMILY, *arg)?);
            }
        }
        let (start, extent) = self.append_payload(RirDirectivesRange::FAMILY, staged)?;
        Ok(RirDirectivesRange::from_parts(start, extent))
    }

    /// Retrieve directives from the extra array.
    pub fn directives(&self, range: &RirDirectivesRange) -> RirDirectives<'_> {
        let words = self
            .payload_words(range, |r| {
                (r.start(), r.extent(), RirDirectivesRange::FAMILY)
            })
            .expect("validated RIR range");
        if words.is_empty() {
            return RirDirectives {
                extra: words,
                start: 0,
                len: 0,
            };
        }
        let view = RirDirectives {
            extra: words,
            start: 1,
            len: words[0] as usize,
        };
        view.iter().for_each(drop);
        view
    }

    fn add_symbol_words<R>(
        &mut self,
        family: &'static str,
        symbols: &[Spur],
        make: impl FnOnce(u32, u32) -> R,
    ) -> Result<R, RirPayloadBuildError> {
        let mut staged = Vec::new();
        staged
            .try_reserve(symbols.len())
            .map_err(|_| RirPayloadBuildError::CapacityFailure { family })?;
        for symbol in symbols {
            staged.push(u32::try_from(symbol.into_usize()).map_err(|_| {
                RirPayloadBuildError::InvalidBuilderInput {
                    family,
                    reason: "symbol index exceeds u32",
                }
            })?);
        }
        let (start, extent) = self.append_payload(family, staged)?;
        Ok(make(start, extent))
    }

    pub(crate) fn add_enum_variants(
        &mut self,
        symbols: &[Spur],
    ) -> Result<RirEnumVariantsRange, RirPayloadBuildError> {
        self.add_symbol_words(
            RirEnumVariantsRange::FAMILY,
            symbols,
            RirEnumVariantsRange::from_parts,
        )
    }
    pub(crate) fn add_anon_enum_variants(
        &mut self,
        symbols: &[Spur],
    ) -> Result<RirAnonEnumVariantsRange, RirPayloadBuildError> {
        self.add_symbol_words(
            RirAnonEnumVariantsRange::FAMILY,
            symbols,
            RirAnonEnumVariantsRange::from_parts,
        )
    }
    fn symbol_view<R>(
        &self,
        range: &R,
        parts: impl FnOnce(&R) -> (u32, u32, &'static str),
    ) -> RirSymbols<'_> {
        let words = self
            .payload_words(range, parts)
            .expect("validated RIR range");
        RirSlice::new(words, SYMBOL_SCHEMA.width, |record| {
            validated_symbol_word(record[0])
        })
    }
    pub fn enum_variants(&self, range: &RirEnumVariantsRange) -> RirSymbols<'_> {
        self.symbol_view(range, |r| {
            (r.start(), r.extent(), RirEnumVariantsRange::FAMILY)
        })
    }
    pub fn anon_enum_variants(&self, range: &RirAnonEnumVariantsRange) -> RirSymbols<'_> {
        self.symbol_view(range, |r| {
            (r.start(), r.extent(), RirAnonEnumVariantsRange::FAMILY)
        })
    }

    fn add_enum_payload_words<R>(
        &mut self,
        family: &'static str,
        payloads: &[Vec<RirTypeSyntaxRef>],
        make: impl FnOnce(u32, u32) -> R,
    ) -> Result<R, RirPayloadBuildError> {
        if payloads.iter().all(Vec::is_empty) {
            return Ok(make(0, 0));
        }
        let exact_words = payloads.iter().try_fold(0usize, |total, payload| {
            total
                .checked_add(
                    encoded_enum_payload_record_extent(payload)
                        .ok_or(RirPayloadBuildError::ResourceLimitExceeded { family })?,
                )
                .ok_or(RirPayloadBuildError::ResourceLimitExceeded { family })
        })?;
        let mut staged = Vec::new();
        staged
            .try_reserve_exact(exact_words)
            .map_err(|_| RirPayloadBuildError::CapacityFailure { family })?;
        for payload in payloads {
            staged.push(
                u32::try_from(payload.len())
                    .map_err(|_| RirPayloadBuildError::ResourceLimitExceeded { family })?,
            );
            for ty in payload {
                staged.push(ty.as_u32());
            }
        }
        let (start, extent) = self.append_payload(family, staged)?;
        Ok(make(start, extent))
    }
    pub(crate) fn add_enum_payloads(
        &mut self,
        payloads: &[Vec<RirTypeSyntaxRef>],
    ) -> Result<RirEnumPayloadsRange, RirPayloadBuildError> {
        self.add_enum_payload_words(
            RirEnumPayloadsRange::FAMILY,
            payloads,
            RirEnumPayloadsRange::from_parts,
        )
    }
    pub(crate) fn add_anon_enum_payloads(
        &mut self,
        payloads: &[Vec<RirTypeSyntaxRef>],
    ) -> Result<RirAnonEnumPayloadsRange, RirPayloadBuildError> {
        self.add_enum_payload_words(
            RirAnonEnumPayloadsRange::FAMILY,
            payloads,
            RirAnonEnumPayloadsRange::from_parts,
        )
    }
    fn enum_payload_view<'a, R>(
        &'a self,
        range: &R,
        variant_count: usize,
        parts: impl FnOnce(&R) -> (u32, u32, &'static str),
    ) -> RirEnumPayloads<'a> {
        RirEnumPayloads {
            words: self
                .payload_words(range, parts)
                .expect("validated RIR range"),
            position: 0,
            remaining: variant_count,
        }
    }

    pub fn enum_payloads(
        &self,
        payloads: &RirEnumPayloadsRange,
        variants: &RirEnumVariantsRange,
    ) -> RirEnumPayloads<'_> {
        self.enum_payload_view(payloads, self.enum_variants(variants).len(), |r| {
            (r.start(), r.extent(), RirEnumPayloadsRange::FAMILY)
        })
    }

    pub fn anon_enum_payloads(
        &self,
        payloads: &RirAnonEnumPayloadsRange,
        variants: &RirAnonEnumVariantsRange,
    ) -> RirEnumPayloads<'_> {
        self.enum_payload_view(payloads, self.anon_enum_variants(variants).len(), |r| {
            (r.start(), r.extent(), RirAnonEnumPayloadsRange::FAMILY)
        })
    }
}

/// Exact, borrowing semantic view of one payload-type list per enum variant.
#[derive(Debug, Clone)]
pub struct RirEnumPayloads<'a> {
    words: &'a [u32],
    position: usize,
    remaining: usize,
}

impl<'a> Iterator for RirEnumPayloads<'a> {
    type Item = RirTypeSyntaxRefs<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        if self.words.is_empty() {
            return Some(RirSlice::new(&[], SYMBOL_SCHEMA.width, |_| unreachable!()));
        }
        let (start, end) = enum_payload_record(self.words, self.position)
            .expect("validated enum payload descriptor");
        self.position = end;
        Some(RirSlice::new(
            &self.words[start..end],
            SYMBOL_SCHEMA.width,
            |record| RirTypeSyntaxRef::from_u32(record[0]),
        ))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for RirEnumPayloads<'_> {}

/// Reusable zero-allocation view over variable-width RIR match arms.
#[derive(Debug, Clone)]
pub struct RirMatchArms<'a> {
    extra: &'a [u32],
    start: usize,
    len: usize,
}

impl<'a> RirMatchArms<'a> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn iter(
        &self,
    ) -> impl ExactSizeIterator<Item = (RirPatternView<'a>, InstRef)> + Clone + 'a {
        RirMatchArmsIter {
            extra: self.extra,
            pos: self.start,
            remaining: self.len,
        }
    }

    pub fn get(&self, index: usize) -> Option<(RirPatternView<'a>, InstRef)> {
        self.iter().nth(index)
    }

    pub fn to_vec(&self) -> Vec<(RirPatternView<'a>, InstRef)> {
        self.iter().collect()
    }
}

#[derive(Debug, Clone)]
struct RirMatchArmsIter<'a> {
    extra: &'a [u32],
    pos: usize,
    remaining: usize,
}

impl<'a> Iterator for RirMatchArmsIter<'a> {
    type Item = (RirPatternView<'a>, InstRef);

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let (pattern, body, extent) = match decode_match_record(self.extra, self.pos) {
            Some(record) => record,
            None => unreachable!("match record passed schema validation"),
        };
        self.pos += extent;
        self.remaining -= 1;
        Some((pattern, body))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for RirMatchArmsIter<'_> {}

/// Reusable zero-allocation view over variable-width RIR directives.
#[derive(Debug, Clone)]
pub struct RirDirectives<'a> {
    extra: &'a [u32],
    start: usize,
    len: usize,
}

impl<'a> RirDirectives<'a> {
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = RirDirectiveView<'a>> + Clone + 'a {
        RirDirectivesIter {
            extra: self.extra,
            pos: self.start,
            remaining: self.len,
        }
    }

    pub fn get(&self, index: usize) -> Option<RirDirectiveView<'a>> {
        self.iter().nth(index)
    }

    pub fn to_vec(&self) -> Vec<RirDirectiveView<'a>> {
        self.iter().collect()
    }
}

#[derive(Debug, Clone)]
struct RirDirectivesIter<'a> {
    extra: &'a [u32],
    pos: usize,
    remaining: usize,
}

impl<'a> Iterator for RirDirectivesIter<'a> {
    type Item = RirDirectiveView<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let (directive, extent) = match decode_directive_record(self.extra, self.pos) {
            Some(record) => record,
            None => unreachable!("directive record passed schema validation"),
        };
        self.pos += extent;
        self.remaining -= 1;
        Some(directive)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for RirDirectivesIter<'_> {}

/// A single RIR instruction.
#[derive(Debug, PartialEq, Eq)]
pub struct Inst {
    pub data: InstData,
    pub span: Span,
}

/// The repeat count of an array-repeat literal `[value; count]` (RUE-235).
///
/// The count is a compile-time constant: either an integer literal (`[0; 128]`)
/// or a name referring to a `const` / `comptime` value parameter (`[0; N]`).
/// Named counts are resolved to a concrete value during semantic analysis using
/// the same const-eval machinery as array-type lengths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepeatCount {
    /// A literal count, e.g. the `128` in `[0; 128]`.
    Literal(u64),
    /// A named count, e.g. the `N` in `[0; N]`.
    Named(Spur),
}

/// A compiler-internal intrinsic synthesized directly into RIR.
///
/// These operations are not source intrinsics. Keeping their identity separate
/// from [`InstData::Intrinsic`] prevents a source spelling that resembles an
/// implementation detail from selecting compiler-only behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum InternalIntrinsic {
    IterLen,
    CharScalar,
    CharNext,
    CharScalarLossy,
    CharNextLossy,
}

impl InternalIntrinsic {
    /// The diagnostic and RIR-printer spelling for this operation.
    ///
    /// This is presentation-only; compiler dispatch must match the enum.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IterLen => "__rue_iter_len",
            Self::CharScalar => "__rue_char_scalar",
            Self::CharNext => "__rue_char_next",
            Self::CharScalarLossy => "__rue_char_scalar_lossy",
            Self::CharNextLossy => "__rue_char_next_lossy",
        }
    }

    /// Number of RIR operands required by this operation.
    pub const fn arity(self) -> u32 {
        match self {
            Self::IterLen => 1,
            Self::CharScalar | Self::CharNext | Self::CharScalarLossy | Self::CharNextLossy => 2,
        }
    }
}

/// Instruction data - the actual operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstData {
    /// Integer constant
    IntConst(u64),

    /// Floating-point constant, held as the interned literal *text* with `_`
    /// separators removed (`1.5`, `1e9`, `6.022e23`).
    ///
    /// This node is deliberately untyped and undecoded (ADR-0065 §3,
    /// RUE-1069). A float literal is a `comptime_float`: an arbitrary-precision
    /// abstract constant that only becomes `f32` or `f64` when context demands
    /// one, with round-to-nearest applied at that point and an out-of-range
    /// magnitude reported as a compile error. Storing an `f64` here would
    /// perform that rounding before the target width — and therefore the
    /// range check — was known, so the digits travel intact and the phase that
    /// resolves the type parses them.
    FloatConst { text: Spur },

    /// Boolean constant
    BoolConst(bool),

    /// String constant with its definition-relative structural source site.
    StringConst {
        content: Spur,
        anchor: RirStructuralAnchor,
    },

    /// Unit constant (for blocks that produce unit type)
    UnitConst,

    // Binary arithmetic operations
    /// Addition: lhs + rhs
    Add { lhs: InstRef, rhs: InstRef },
    /// Subtraction: lhs - rhs
    Sub { lhs: InstRef, rhs: InstRef },
    /// Multiplication: lhs * rhs
    Mul { lhs: InstRef, rhs: InstRef },
    /// Division: lhs / rhs
    Div { lhs: InstRef, rhs: InstRef },
    /// Modulo: lhs % rhs
    Mod { lhs: InstRef, rhs: InstRef },

    // Comparison operations
    /// Equality: lhs == rhs
    Eq { lhs: InstRef, rhs: InstRef },
    /// Inequality: lhs != rhs
    Ne { lhs: InstRef, rhs: InstRef },
    /// Less than: lhs < rhs
    Lt { lhs: InstRef, rhs: InstRef },
    /// Greater than: lhs > rhs
    Gt { lhs: InstRef, rhs: InstRef },
    /// Less than or equal: lhs <= rhs
    Le { lhs: InstRef, rhs: InstRef },
    /// Greater than or equal: lhs >= rhs
    Ge { lhs: InstRef, rhs: InstRef },

    // Logical operations
    /// Logical AND: lhs && rhs
    And { lhs: InstRef, rhs: InstRef },
    /// Logical OR: lhs || rhs
    Or { lhs: InstRef, rhs: InstRef },

    // Bitwise operations
    /// Bitwise AND: lhs & rhs
    BitAnd { lhs: InstRef, rhs: InstRef },
    /// Bitwise OR: lhs | rhs
    BitOr { lhs: InstRef, rhs: InstRef },
    /// Bitwise XOR: lhs ^ rhs
    BitXor { lhs: InstRef, rhs: InstRef },
    /// Left shift: lhs << rhs
    Shl { lhs: InstRef, rhs: InstRef },
    /// Right shift: lhs >> rhs (arithmetic for signed, logical for unsigned)
    Shr { lhs: InstRef, rhs: InstRef },

    // Unary operations
    /// Negation: -operand
    Neg { operand: InstRef },
    /// Logical NOT: !operand
    Not { operand: InstRef },
    /// Bitwise NOT: ~operand
    BitNot { operand: InstRef },

    /// Try/`?` propagation: `operand?` unwraps an `Option`, evaluating to the
    /// `Some` payload, and early-returns `None` from the enclosing function
    /// when the operand is `None` (RUE-6, ADR-0038). Sema lowers it to a
    /// discriminant match with an early `return` on the `None` arm; the
    /// enclosing function must itself return an `Option`.
    Try { operand: InstRef },

    // Control flow
    /// Branch: if cond then then_block else else_block
    Branch {
        cond: InstRef,
        then_block: InstRef,
        else_block: Option<InstRef>,
    },

    /// While loop: while cond { body }
    Loop { cond: InstRef, body: InstRef },

    /// Infinite loop: loop { body }
    ///
    /// `iter_borrow` names the collection variable held as a scoped shared
    /// borrow for the loop's duration when this loop is the desugaring of a
    /// `for` over a named variable (spec 4.8:26, RUE-233). Sema rejects
    /// mutation of that variable inside the body (E0428). `None` for a plain
    /// `loop {}` or a `for` over a temporary (which is unnameable, so
    /// unmutatable, in the body).
    InfiniteLoop {
        body: InstRef,
        iter_borrow: Option<Spur>,
    },

    /// Match expression: match scrutinee { pattern => expr, ... }
    /// Arms are stored in the extra array using add_match_arms/get_match_arms.
    Match {
        /// The value being matched
        scrutinee: InstRef,
        /// Index into extra data where arms start
        arms: RirMatchArmsRange,
    },

    /// Break: exits the innermost loop.
    /// `value` is a value operand (e.g. `break 42`); it is carried through
    /// for diagnostics but always rejected by sema - break does not carry a
    /// value (see spec 4.8).
    Break { value: Option<InstRef> },

    /// Continue: jumps to the next iteration of the innermost loop
    Continue,

    /// Function definition
    /// Contains: name symbol, parameters, return type symbol, body instruction ref
    /// Directives and params are stored in the extra array.
    FnDecl {
        /// Index into extra data where directives start
        directives: RirDirectivesRange,
        /// Whether this function is public (requires --preview modules)
        is_pub: bool,
        /// Whether this function is marked `unchecked` (can only be called from checked blocks)
        is_unchecked: bool,
        /// Whether this is a foreign `extern "C"` declaration (ADR-0064 C FFI):
        /// a body-less import lowered to an undefined linker symbol. When true
        /// `body` is a synthesized placeholder that is never analyzed or
        /// code-generated.
        is_extern: bool,
        /// Whether this is a Rue-to-C export (`pub extern "C" fn`, ADR-0064 P4):
        /// an ordinary Rue function body that is *also* exposed to C callers
        /// under its unmangled source name via a C-ABI callee thunk. Unlike
        /// `is_extern`, an export keeps a real body, is analyzed, gets a CFG,
        /// and is code-generated; the flag only marks that a C entry thunk must
        /// be emitted and that the signature is validated for the C boundary.
        is_c_export: bool,
        name: Spur,
        /// Index into extra data where params start
        params: RirParamsRange,
        return_type: RirTypeSyntaxRef,
        body: InstRef,
        /// Whether this function/method takes `self` as a receiver.
        /// Only true for methods in impl blocks that have a self parameter.
        /// Used by sema to know to add the implicit self parameter.
        has_self: bool,
        /// The receiver's passing mode when `has_self` is true (`Normal`
        /// by-value, `Borrow`, or `Inout`; RUE-15). Always `Normal` for
        /// associated functions and free functions.
        self_mode: RirParamMode,
        /// Whether the receiver is declared `mut self`: a by-value receiver
        /// that binds mutably in the method body. Body-local only — it does
        /// not affect the method's signature, call sites, or structural
        /// identity, and is always false unless `has_self` is true with
        /// `self_mode == Normal`.
        self_is_mut: bool,
        /// Whether the result position is `-> borrow T` (ADR-0062): the
        /// declaration is a place-returning accessor whose body yields a
        /// second-class borrow of a receiver projection. `return_type` holds
        /// the borrowed element type `T`.
        returns_borrow: bool,
        /// Whether the result position is `-> inout T` (ADR-0062 phase 2).
        returns_inout: bool,
    },

    /// Constant declaration
    /// Contains: name symbol, optional type, initializer expression ref
    /// Directives are stored in the extra array.
    /// Used for module re-exports: `pub const strings = @import("utils/strings.rue");`
    ConstDecl {
        /// Index into extra data where directives start
        directives: RirDirectivesRange,
        /// Whether this constant is public (requires --preview modules)
        is_pub: bool,
        /// Constant name
        name: Spur,
        /// Optional structured type annotation (`None` when inferred).
        ty: Option<RirTypeSyntaxRef>,
        /// Initializer expression
        init: InstRef,
    },

    /// Function call
    /// Args are stored in the extra array using add_call_args/get_call_args.
    Call {
        /// Function name
        name: Spur,
        /// Index into extra data where args start
        args: RirCallArgsRange,
    },

    /// Intrinsic call with expression arguments (e.g., @dbg)
    /// Args are stored in the typed intrinsic-argument payload family.
    Intrinsic {
        /// Intrinsic name (without @)
        name: Spur,
        /// Index into extra data where args start
        args: RirIntrinsicArgsRange,
    },

    /// Compiler-internal intrinsic with expression arguments.
    ///
    /// Args are stored in the typed internal-intrinsic payload family.
    InternalIntrinsic {
        intrinsic: InternalIntrinsic,
        args: RirInternalIntrinsicArgsRange,
    },

    /// Intrinsic call with a type argument (e.g., @size_of, @align_of)
    TypeIntrinsic {
        /// Intrinsic name (without @)
        name: Spur,
        /// Structured type argument.
        type_arg: RirTypeSyntaxRef,
    },

    /// `@offset_of(T, field)` — the compile-time byte offset of `field` within
    /// struct type `T` (RUE-301). Carries a type argument (as an interned
    /// string, exactly like [`InstData::TypeIntrinsic`]) and the field name;
    /// neither is an `InstRef`, so this variant has no operands to renumber.
    OffsetOf {
        /// Structured type argument.
        type_arg: RirTypeSyntaxRef,
        /// Field name whose offset is requested.
        field: Spur,
    },

    /// Return value from function (None for `return;` in unit-returning functions)
    Ret(Option<InstRef>),

    /// Yield a place from a `-> borrow T` accessor body (ADR-0062). The
    /// operand is the place expression the accessor hands out; valid only as
    /// the trailing exit of an accessor body (enforced in sema).
    Yield(InstRef),

    /// Block of instructions (for function bodies)
    /// The result is the last instruction in the block
    Block {
        /// Index into extra data where instruction refs start
        instructions: RirBlockInstsRange,
    },

    // Variable operations
    /// Local variable declaration: allocates storage and initializes
    /// If name is None, this is a wildcard pattern that discards the value
    /// Directives are stored in the extra array using add_directives/get_directives.
    Alloc {
        /// Index into extra data where directives start
        directives: RirDirectivesRange,
        /// Variable name (None for wildcard `_` pattern that discards the value)
        name: Option<Spur>,
        /// Whether the variable is mutable
        is_mut: bool,
        /// Optional type annotation
        ty: Option<RirTypeSyntaxRef>,
        /// Initial value instruction
        init: InstRef,
        /// True when this binding is a `for`-loop element binding, which reads
        /// the element by shared borrow (spec 4.8:26): the initializer is
        /// analyzed as a by-ref read (never a move-out of the collection, so a
        /// non-Copy element is fine, RUE-259) and a non-Copy binder is a
        /// non-owning borrow slot that drop elaboration must not drop (the
        /// collection retains ownership and drops the element).
        iter_elem: bool,
    },

    /// Variable reference: reads the value of a variable
    VarRef {
        /// Variable name
        name: Spur,
        /// Stable source occurrence for named read-only data materialized at
        /// this reference. Synthesized references do not carry one.
        anchor: Option<RirStructuralAnchor>,
    },

    /// Assignment: stores a value into a mutable variable
    Assign {
        /// Variable name
        name: Spur,
        /// Value to store
        value: InstRef,
    },

    /// Assignment to an expression that produces a place (currently a
    /// mutable accessor result). Kept distinct from field/index assignment so
    /// source field names cannot collide with compiler-generated syntax.
    PlaceSet { place: InstRef, value: InstRef },

    // Struct operations
    /// Struct type declaration
    /// Directives, fields, and methods are stored in the extra array.
    StructDecl {
        /// Index into extra data where directives start
        directives: RirDirectivesRange,
        /// Whether this struct is public (requires --preview modules)
        is_pub: bool,
        /// Whether this struct is a linear type (must be consumed)
        is_linear: bool,
        /// Struct name
        name: Spur,
        /// Index into extra data where fields start
        fields: RirStructFieldsRange,
        /// Index into extra data where method refs start
        methods: RirStructMethodsRange,
    },

    /// Struct literal: creates a new struct instance
    /// Fields are stored in the extra array using add_field_inits/get_field_inits.
    StructInit {
        /// Optional module reference (for qualified struct literals like `module.Point { ... }`)
        /// If Some, the struct is looked up in the module's exports.
        module: Option<InstRef>,
        /// Optional inline type-constructor call head — the instruction that
        /// reduces to the struct type at comptime for `F(args) { ... }` (RUE-596,
        /// spec 4.14:23). When `Some`, the struct type is
        /// the reduction of this head and `type_name` is only the constructor
        /// function's name (kept for diagnostics); `None` for `Name { ... }`.
        ctor_head: Option<InstRef>,
        /// Struct type name
        type_name: Spur,
        /// Index into extra data where fields start
        fields: RirFieldInitsRange,
        /// Span of the first field-init-shorthand field, if any (`P { x }`
        /// desugaring to `P { x: x }`, RUE-613, stabilized in RUE-628). `Some`
        /// iff at least one field used the shorthand; retained as diagnostic
        /// provenance for the shorthand form. `None` when every field was
        /// written explicitly (`P { x: x }`).
        shorthand_span: Option<Span>,
    },

    /// Field access: reads a field from a struct
    FieldGet {
        /// Base struct value
        base: InstRef,
        /// Field name
        field: Spur,
    },

    /// Field assignment: writes a value to a struct field
    FieldSet {
        /// Base struct value
        base: InstRef,
        /// Field name
        field: Spur,
        /// Value to store
        value: InstRef,
    },

    // Enum operations
    /// Enum type declaration
    /// Variants are stored in the typed enum-variant payload family.
    EnumDecl {
        /// Whether this enum is public (requires --preview modules)
        is_pub: bool,
        /// Whether importing modules must include a wildcard when matching.
        is_non_exhaustive: bool,
        /// Enum name
        name: Spur,
        /// Index into extra data where variants start
        variants: RirEnumVariantsRange,
        /// Index into extra data where the tuple-variant payloads start
        /// (RUE-221). The region is a self-describing flat sequence: for each
        /// variant in declaration order, a count `k` followed by `k`
        /// type-name symbols (as `Spur`s). A count of 0 means a
        /// discriminant-only variant. `payloads_len` is the total number of
        /// u32 words in the region (0 when no variant carries a payload).
        payloads: RirEnumPayloadsRange,
    },

    /// Enum variant: creates a value of an enum type
    EnumVariant {
        /// Optional module reference (for qualified paths like `module.Color::Red`)
        /// If Some, the enum is looked up in the module's exports.
        module: Option<InstRef>,
        /// Enum type name
        type_name: Spur,
        /// Variant name
        variant: Spur,
    },

    // Array operations
    /// Array literal: creates a new array from element values
    /// Elements are stored in the typed array-element payload family.
    ArrayInit {
        /// Index into extra data where elements start
        elements: RirArrayElemsRange,
    },

    /// Array-repeat literal `[value; count]` (RUE-235): creates an array of
    /// `count` copies of a single `value`. `value` is evaluated once and its
    /// (Copy) result fills every slot. The count is a compile-time constant
    /// resolved during semantic analysis.
    ArrayRepeat {
        /// The value being repeated (evaluated once).
        value: InstRef,
        /// The repeat count (literal or named compile-time constant).
        count: RepeatCount,
    },

    /// Array index read: reads an element from an array
    IndexGet {
        /// Base array value
        base: InstRef,
        /// Index expression
        index: InstRef,
    },

    /// Array index write: writes a value to an array element
    IndexSet {
        /// Base array value (must be a VarRef to a mutable variable)
        base: InstRef,
        /// Index expression
        index: InstRef,
        /// Value to store
        value: InstRef,
    },

    // Method operations
    /// Method call: receiver.method(args)
    /// Args are stored in the extra array using add_call_args/get_call_args.
    MethodCall {
        /// Receiver expression (the struct value)
        receiver: InstRef,
        /// Method name
        method: Spur,
        /// Index into extra data where args start
        args: RirCallArgsRange,
    },

    /// User-defined destructor declaration: drop fn TypeName(self) { ... }
    DropFnDecl {
        /// The struct type this destructor is for
        type_name: Spur,
        /// Destructor body instruction ref
        body: InstRef,
    },

    /// Comptime block expression: comptime { expr }
    /// The inner expression must be evaluable at compile time.
    Comptime {
        /// The expression to evaluate at compile time
        expr: InstRef,
    },

    /// Checked block expression: checked { expr }
    /// Unchecked operations (raw pointer manipulation, calling unchecked functions)
    /// are only allowed inside checked blocks.
    Checked {
        /// The expression inside the checked block
        expr: InstRef,
    },

    /// Type constant: a type used as a value expression (e.g., `i32` in `identity(i32, 42)`)
    /// Carries the exact structured parser-owned syntax.
    TypeConst { type_name: RirTypeSyntaxRef },

    /// Anonymous struct type: a struct type used as a value expression
    /// (e.g., `struct { first: T, second: T, fn method(self) -> T { ... } }` in comptime type construction)
    /// Fields are stored in the extra array using add_field_decls/get_field_decls.
    /// Methods are stored as InstRefs to FnDecl instructions in the extra array.
    AnonStructType {
        /// Index into extra data where fields start
        fields: RirAnonStructFieldsRange,
        /// Index into extra data where method InstRefs start
        methods: RirAnonStructMethodsRange,
        /// Structural occurrence relative to the producing definition body.
        anchor: RirStructuralAnchor,
    },

    /// Anonymous enum type: an enum (sum) type used as a value expression
    /// (e.g., `enum { Some(T), None }` in comptime type construction). The
    /// enum analog of [`InstData::AnonStructType`]; enables generic sum types
    /// like `Option`/`Result` as comptime type functions (ADR-0038, RUE-6).
    /// Variant names and tuple-variant payloads are encoded exactly
    /// as in [`InstData::EnumDecl`].
    AnonEnumType {
        /// Index into extra data where variant name symbols start
        variants: RirAnonEnumVariantsRange,
        /// Index into extra data where the tuple-variant payloads start,
        /// encoded as in [`InstData::EnumDecl`]: a self-describing flat
        /// sequence of `count` + `count` type-name symbols per variant.
        payloads: RirAnonEnumPayloadsRange,
        /// Structural occurrence relative to the producing definition body.
        anchor: RirStructuralAnchor,
    },
}

impl fmt::Display for InstRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

struct DisplayedInstRef(u32);

impl fmt::Display for DisplayedInstRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

/// Printer for RIR that resolves symbols to their string values.
pub struct RirPrinter<'a, 'b> {
    rir: &'a Rir,
    interner: &'b lasso::ThreadedRodeo,
    instruction_order: Option<Vec<InstRef>>,
    displayed_refs: Option<Vec<u32>>,
    displayed_extra: Option<Vec<u32>>,
}

impl<'a, 'b> RirPrinter<'a, 'b> {
    fn format_type(&self, reference: RirTypeSyntaxRef) -> String {
        self.rir
            .type_syntax()
            .render_type_with(reference, |symbol| self.interner.resolve(symbol))
            .unwrap_or_else(|| "<invalid-type>".to_owned())
    }
    /// Create a new RIR printer.
    pub fn new(rir: &'a Rir, interner: &'b lasso::ThreadedRodeo) -> Self {
        Self {
            rir,
            interner,
            instruction_order: None,
            displayed_refs: None,
            displayed_extra: None,
        }
    }

    /// Create a read-only presentation of `rir` in a different instruction order.
    ///
    /// The supplied order must be a permutation of every instruction in the RIR.
    /// References are displayed in that order without cloning or rewriting the RIR.
    pub fn with_instruction_order(
        rir: &'a Rir,
        interner: &'b lasso::ThreadedRodeo,
        instruction_order: Vec<InstRef>,
    ) -> Self {
        assert_eq!(instruction_order.len(), rir.len());
        let mut displayed_refs = vec![u32::MAX; rir.len()];
        for (displayed, canonical) in instruction_order.iter().enumerate() {
            let slot = &mut displayed_refs[canonical.as_u32() as usize];
            assert_eq!(
                *slot,
                u32::MAX,
                "RIR presentation order contains a duplicate"
            );
            *slot = displayed as u32;
        }
        assert!(
            displayed_refs
                .iter()
                .all(|displayed| *displayed != u32::MAX)
        );
        Self {
            rir,
            interner,
            instruction_order: Some(instruction_order),
            displayed_refs: Some(displayed_refs),
            displayed_extra: None,
        }
    }

    /// Create a presentation that remaps both instruction and payload ordering.
    pub fn with_presentation_order(
        rir: &'a Rir,
        interner: &'b lasso::ThreadedRodeo,
        instruction_order: Vec<InstRef>,
        extra_order: Vec<u32>,
    ) -> Self {
        let mut printer = Self::with_instruction_order(rir, interner, instruction_order);
        assert_eq!(extra_order.len(), rir.extra_len());
        let mut displayed_extra = vec![u32::MAX; rir.extra_len()];
        for (displayed, canonical) in extra_order.into_iter().enumerate() {
            let slot = &mut displayed_extra[canonical as usize];
            assert_eq!(
                *slot,
                u32::MAX,
                "RIR payload presentation contains a duplicate"
            );
            *slot = displayed as u32;
        }
        assert!(
            displayed_extra
                .iter()
                .all(|displayed| *displayed != u32::MAX)
        );
        printer.displayed_extra = Some(displayed_extra);
        printer
    }

    fn display_ref(&self, inst: InstRef) -> DisplayedInstRef {
        DisplayedInstRef(
            self.displayed_refs
                .as_ref()
                .map_or(inst.as_u32(), |refs| refs[inst.as_u32() as usize]),
        )
    }

    /// Format a call argument with its mode prefix.
    fn format_call_arg(&self, arg: &RirCallArg) -> String {
        match arg.mode {
            RirArgMode::Inout => format!("inout {}", self.display_ref(arg.value)),
            RirArgMode::Borrow => format!("borrow {}", self.display_ref(arg.value)),
            RirArgMode::Normal => format!("{}", self.display_ref(arg.value)),
        }
    }

    /// Format a list of call arguments.
    fn format_call_args(&self, args: impl IntoIterator<Item = RirCallArg>) -> String {
        args.into_iter()
            .map(|arg| self.format_call_arg(&arg))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Format an item's directives as a `"@copy @allow(..) "` prefix
    /// (empty string when there are none).
    fn format_directives(&self, range: &RirDirectivesRange) -> String {
        let directives = self.rir.directives(range);
        if directives.len() == 0 {
            return String::new();
        }
        let dir_names: Vec<String> = directives
            .iter()
            .map(|d| format!("@{}", self.interner.resolve(&d.name)))
            .collect();
        format!("{} ", dir_names.join(" "))
    }

    /// Format a pattern for printing.
    fn format_pattern(&self, pat: &RirPatternView<'_>) -> String {
        match pat {
            RirPatternView::Wildcard(_) => "_".to_string(),
            RirPatternView::Int {
                value, negative, ..
            } => {
                if *negative {
                    format!("-{}", value)
                } else {
                    value.to_string()
                }
            }
            RirPatternView::Bool(b, _) => b.to_string(),
            RirPatternView::Path {
                module,
                type_name,
                variant,
                bindings,
                ..
            } => {
                let prefix = if let Some(module_ref) = module {
                    format!("{}..", self.display_ref(*module_ref))
                } else {
                    String::new()
                };
                let base = format!(
                    "{}{}::{}",
                    prefix,
                    self.interner.resolve(&*type_name),
                    self.interner.resolve(&*variant)
                );
                if bindings.is_empty() {
                    base
                } else {
                    let names: Vec<&str> =
                        bindings.iter().map(|b| self.interner.resolve(&b)).collect();
                    format!("{}({})", base, names.join(", "))
                }
            }
        }
    }

    /// Format the RIR as a string.
    pub fn to_string(&self) -> String {
        use std::fmt::Write;

        let mut out = String::new();
        let instruction_order: Box<dyn Iterator<Item = InstRef> + '_> =
            match &self.instruction_order {
                Some(order) => Box::new(order.iter().copied()),
                None => Box::new(self.rir.iter().map(|(inst_ref, _)| inst_ref)),
            };
        for inst_ref in instruction_order {
            let inst = self.rir.get(inst_ref);
            write!(out, "{} = ", self.display_ref(inst_ref)).unwrap();
            match &inst.data {
                // Constants
                InstData::IntConst(v) => writeln!(out, "const {}", v).unwrap(),
                // Printed with the `float` tag so a float literal is visibly
                // distinct from an integer one in a RIR dump: `1e9` and
                // `1000000000` are different nodes with the same value.
                InstData::FloatConst { text } => {
                    writeln!(out, "const float {}", self.interner.resolve(&*text)).unwrap()
                }
                InstData::BoolConst(v) => writeln!(out, "const {}", v).unwrap(),
                InstData::StringConst { content, .. } => {
                    writeln!(out, "const {:?}", self.interner.resolve(&*content)).unwrap()
                }
                InstData::UnitConst => writeln!(out, "const ()").unwrap(),

                // Binary operations
                InstData::Add { lhs, rhs } => writeln!(
                    out,
                    "add {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Sub { lhs, rhs } => writeln!(
                    out,
                    "sub {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Mul { lhs, rhs } => writeln!(
                    out,
                    "mul {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Div { lhs, rhs } => writeln!(
                    out,
                    "div {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Mod { lhs, rhs } => writeln!(
                    out,
                    "mod {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Eq { lhs, rhs } => writeln!(
                    out,
                    "eq {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Ne { lhs, rhs } => writeln!(
                    out,
                    "ne {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Lt { lhs, rhs } => writeln!(
                    out,
                    "lt {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Gt { lhs, rhs } => writeln!(
                    out,
                    "gt {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Le { lhs, rhs } => writeln!(
                    out,
                    "le {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Ge { lhs, rhs } => writeln!(
                    out,
                    "ge {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::And { lhs, rhs } => writeln!(
                    out,
                    "and {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Or { lhs, rhs } => writeln!(
                    out,
                    "or {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::BitAnd { lhs, rhs } => writeln!(
                    out,
                    "bit_and {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::BitOr { lhs, rhs } => writeln!(
                    out,
                    "bit_or {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::BitXor { lhs, rhs } => writeln!(
                    out,
                    "bit_xor {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Shl { lhs, rhs } => writeln!(
                    out,
                    "shl {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),
                InstData::Shr { lhs, rhs } => writeln!(
                    out,
                    "shr {}, {}",
                    self.display_ref(*lhs),
                    self.display_ref(*rhs)
                )
                .unwrap(),

                // Unary operations
                InstData::Neg { operand } => {
                    writeln!(out, "neg {}", self.display_ref(*operand)).unwrap()
                }
                InstData::Not { operand } => {
                    writeln!(out, "not {}", self.display_ref(*operand)).unwrap()
                }
                InstData::BitNot { operand } => {
                    writeln!(out, "bit_not {}", self.display_ref(*operand)).unwrap()
                }
                InstData::Try { operand } => {
                    writeln!(out, "try {}", self.display_ref(*operand)).unwrap()
                }

                // Control flow
                InstData::Branch {
                    cond,
                    then_block,
                    else_block,
                } => {
                    if let Some(else_b) = else_block {
                        writeln!(
                            out,
                            "branch {}, {}, {}",
                            self.display_ref(*cond),
                            self.display_ref(*then_block),
                            self.display_ref(*else_b)
                        )
                        .unwrap();
                    } else {
                        writeln!(
                            out,
                            "branch {}, {}",
                            self.display_ref(*cond),
                            self.display_ref(*then_block)
                        )
                        .unwrap();
                    }
                }
                InstData::Loop { cond, body } => writeln!(
                    out,
                    "loop {}, {}",
                    self.display_ref(*cond),
                    self.display_ref(*body)
                )
                .unwrap(),
                InstData::InfiniteLoop { body, iter_borrow } => {
                    let borrow_str = iter_borrow
                        .map(|c| format!(" borrows {}", self.interner.resolve(&c)))
                        .unwrap_or_default();
                    writeln!(
                        out,
                        "infinite_loop {}{}",
                        self.display_ref(*body),
                        borrow_str
                    )
                    .unwrap()
                }
                InstData::Match { scrutinee, arms } => {
                    let arms = self.rir.match_arms(arms);
                    let arms_str: Vec<String> = arms
                        .iter()
                        .map(|(pat, body)| {
                            format!(
                                "{} => {}",
                                self.format_pattern(&pat),
                                self.display_ref(body)
                            )
                        })
                        .collect();
                    writeln!(
                        out,
                        "match {} {{ {} }}",
                        self.display_ref(*scrutinee),
                        arms_str.join(", ")
                    )
                    .unwrap();
                }
                InstData::Break { value } => match value {
                    Some(v) => writeln!(out, "break {}", self.display_ref(*v)).unwrap(),
                    None => writeln!(out, "break").unwrap(),
                },
                InstData::Continue => writeln!(out, "continue").unwrap(),

                // Functions
                InstData::FnDecl {
                    directives,
                    is_pub,
                    is_unchecked,
                    is_extern,
                    is_c_export,
                    name,
                    params,
                    return_type,
                    body,
                    has_self,
                    self_mode,
                    self_is_mut,
                    returns_borrow,
                    returns_inout,
                } => {
                    let pub_str = if *is_c_export {
                        "pub extern \"C\" "
                    } else if *is_pub {
                        "pub "
                    } else {
                        ""
                    };
                    let unchecked_str = if *is_unchecked {
                        "unchecked "
                    } else if *is_extern {
                        "extern "
                    } else {
                        ""
                    };
                    let name_str = self.interner.resolve(&*name);
                    let ret_str = self.format_type(*return_type);
                    let self_str = if *has_self {
                        match self_mode {
                            RirParamMode::Inout => "inout self, ",
                            RirParamMode::Borrow => "borrow self, ",
                            RirParamMode::Normal if *self_is_mut => "mut self, ",
                            RirParamMode::Normal => "self, ",
                        }
                    } else {
                        ""
                    };
                    let params = self.rir.params(params);
                    let params_str: Vec<String> = params
                        .values()
                        .map(|p| {
                            let comptime_prefix = if p.is_comptime { "comptime " } else { "" };
                            let mode_prefix = match p.mode {
                                RirParamMode::Inout => "inout ",
                                RirParamMode::Borrow => "borrow ",
                                RirParamMode::Normal => "",
                            };
                            format!(
                                "{}{}{}: {}",
                                comptime_prefix,
                                mode_prefix,
                                self.interner.resolve(&p.name),
                                self.format_type(p.ty)
                            )
                        })
                        .collect();
                    let directives_str = self.format_directives(directives);
                    let borrow_str = if *returns_borrow {
                        "borrow "
                    } else if *returns_inout {
                        "inout "
                    } else {
                        ""
                    };
                    writeln!(
                        out,
                        "{}{}{}fn {}({}{}) -> {}{} {{",
                        directives_str,
                        pub_str,
                        unchecked_str,
                        name_str,
                        self_str,
                        params_str.join(", "),
                        borrow_str,
                        ret_str
                    )
                    .unwrap();
                    writeln!(out, "    {}", self.display_ref(*body)).unwrap();
                    writeln!(out, "}}").unwrap();
                }
                InstData::ConstDecl {
                    directives,
                    is_pub,
                    name,
                    ty,
                    init,
                } => {
                    let directives_str = self.format_directives(directives);
                    let pub_str = if *is_pub { "pub " } else { "" };
                    let name_str = self.interner.resolve(&*name);
                    let ty_str = ty
                        .map(|t| format!(": {}", self.format_type(t)))
                        .unwrap_or_default();
                    writeln!(
                        out,
                        "{}{}const {}{} = {}",
                        directives_str,
                        pub_str,
                        name_str,
                        ty_str,
                        self.display_ref(*init)
                    )
                    .unwrap();
                }
                InstData::Ret(inner) => {
                    if let Some(inner) = inner {
                        writeln!(out, "ret {}", self.display_ref(*inner)).unwrap();
                    } else {
                        writeln!(out, "ret").unwrap();
                    }
                }
                InstData::Yield(inner) => {
                    writeln!(out, "yield {}", self.display_ref(*inner)).unwrap();
                }
                InstData::Call { name, args } => {
                    let name_str = self.interner.resolve(&*name);
                    let args = self.rir.call_args(args);
                    writeln!(out, "call {}({})", name_str, self.format_call_args(args)).unwrap();
                }
                InstData::Intrinsic { name, args } => {
                    let name_str = self.interner.resolve(&*name);
                    let args = self.rir.intrinsic_args(args);
                    let args_str: Vec<String> = args
                        .values()
                        .map(|a| self.display_ref(a).to_string())
                        .collect();
                    writeln!(out, "intrinsic @{}({})", name_str, args_str.join(", ")).unwrap();
                }
                InstData::InternalIntrinsic { intrinsic, args } => {
                    let args = self.rir.internal_intrinsic_args(args);
                    let args_str: Vec<String> = args
                        .values()
                        .map(|a| self.display_ref(a).to_string())
                        .collect();
                    writeln!(
                        out,
                        "internal_intrinsic @{}({})",
                        intrinsic.as_str(),
                        args_str.join(", ")
                    )
                    .unwrap();
                }
                InstData::TypeIntrinsic { name, type_arg } => {
                    let name_str = self.interner.resolve(&*name);
                    let type_str = self.format_type(*type_arg);
                    writeln!(out, "type_intrinsic @{}({})", name_str, type_str).unwrap();
                }
                InstData::OffsetOf { type_arg, field } => {
                    let type_str = self.format_type(*type_arg);
                    let field_str = self.interner.resolve(&*field);
                    writeln!(out, "offset_of @offset_of({}, {})", type_str, field_str).unwrap();
                }
                InstData::Block { instructions } => {
                    writeln!(out, "block({instructions:?})").unwrap();
                }

                // Variables
                InstData::Alloc {
                    directives,
                    name,
                    is_mut,
                    ty,
                    init,
                    iter_elem,
                } => {
                    let directives_str = self.format_directives(directives);
                    let name_str = name
                        .map(|n| self.interner.resolve(&n).to_string())
                        .unwrap_or_else(|| "_".to_string());
                    let mut_str = if *is_mut { "mut " } else { "" };
                    let ty_str = ty
                        .map(|t| format!(": {}", self.format_type(t)))
                        .unwrap_or_default();
                    let iter_str = if *iter_elem { " [iter_elem]" } else { "" };
                    writeln!(
                        out,
                        "{}alloc {}{}{}= {}{}",
                        directives_str,
                        mut_str,
                        name_str,
                        ty_str,
                        self.display_ref(*init),
                        iter_str
                    )
                    .unwrap();
                }
                InstData::VarRef { name, .. } => {
                    writeln!(out, "var_ref {}", self.interner.resolve(&*name)).unwrap();
                }
                InstData::Assign { name, value } => {
                    writeln!(
                        out,
                        "assign {} = {}",
                        self.interner.resolve(&*name),
                        self.display_ref(*value)
                    )
                    .unwrap();
                }
                InstData::PlaceSet { place, value } => {
                    writeln!(
                        out,
                        "place_set {} = {}",
                        self.display_ref(*place),
                        self.display_ref(*value)
                    )
                    .unwrap();
                }

                // Structs
                InstData::StructDecl {
                    directives,
                    is_pub,
                    is_linear,
                    name,
                    fields,
                    methods,
                } => {
                    let pub_str = if *is_pub { "pub " } else { "" };
                    let name_str = self.interner.resolve(&*name);
                    let fields = self.rir.struct_fields(fields);
                    let fields_str: Vec<String> = fields
                        .values()
                        .map(|(fname, ftype)| {
                            format!(
                                "{}: {}",
                                self.interner.resolve(&fname),
                                self.format_type(ftype)
                            )
                        })
                        .collect();
                    let linear_str = if *is_linear { "linear " } else { "" };
                    let directives_str = self.format_directives(directives);
                    let methods = self.rir.struct_methods(methods);
                    let methods_str = if methods.len() == 0 {
                        String::new()
                    } else {
                        let method_refs: Vec<String> = methods
                            .values()
                            .map(|m| self.display_ref(m).to_string())
                            .collect();
                        format!(" methods: [{}]", method_refs.join(", "))
                    };
                    writeln!(
                        out,
                        "{}{}{}struct {} {{ {} }}{}",
                        directives_str,
                        pub_str,
                        linear_str,
                        name_str,
                        fields_str.join(", "),
                        methods_str
                    )
                    .unwrap();
                }
                InstData::StructInit {
                    module,
                    ctor_head,
                    type_name,
                    fields,
                    shorthand_span: _,
                } => {
                    let module_str = match ctor_head {
                        Some(head) => format!("<{}>.", self.display_ref(*head)),
                        None => module
                            .map(|m| format!("{}.", self.display_ref(m)))
                            .unwrap_or_default(),
                    };
                    let type_str = self.interner.resolve(&*type_name);
                    let fields = self.rir.field_inits(fields);
                    let fields_str: Vec<String> = fields
                        .values()
                        .map(|(fname, value)| {
                            format!(
                                "{}: {}",
                                self.interner.resolve(&fname),
                                self.display_ref(value)
                            )
                        })
                        .collect();
                    writeln!(
                        out,
                        "struct_init {}{} {{ {} }}",
                        module_str,
                        type_str,
                        fields_str.join(", ")
                    )
                    .unwrap();
                }
                InstData::FieldGet { base, field } => {
                    writeln!(
                        out,
                        "field_get {}.{}",
                        self.display_ref(*base),
                        self.interner.resolve(&*field)
                    )
                    .unwrap();
                }
                InstData::FieldSet { base, field, value } => {
                    writeln!(
                        out,
                        "field_set {}.{} = {}",
                        self.display_ref(*base),
                        self.interner.resolve(&*field),
                        self.display_ref(*value)
                    )
                    .unwrap();
                }

                // Enums
                InstData::EnumDecl {
                    is_pub,
                    is_non_exhaustive,
                    name,
                    variants,
                    payloads,
                } => {
                    let pub_str = if *is_pub { "pub " } else { "" };
                    let marker = if *is_non_exhaustive {
                        "@non_exhaustive "
                    } else {
                        ""
                    };
                    let name_str = self.interner.resolve(&*name);
                    let payload_arities: Vec<usize> = self
                        .rir
                        .enum_payloads(payloads, variants)
                        .map(|payload| payload.len())
                        .collect();
                    let variants = self.rir.enum_variants(variants);
                    let variants_str: Vec<String> = variants
                        .iter()
                        .enumerate()
                        .map(|(i, v)| {
                            let base = self.interner.resolve(&v).to_string();
                            match payload_arities.get(i) {
                                Some(k) if *k > 0 => format!("{}/{}", base, k),
                                _ => base,
                            }
                        })
                        .collect();
                    writeln!(
                        out,
                        "{}{}enum {} {{ {} }}",
                        marker,
                        pub_str,
                        name_str,
                        variants_str.join(", ")
                    )
                    .unwrap();
                }
                InstData::EnumVariant {
                    module,
                    type_name,
                    variant,
                } => {
                    let module_str = module
                        .map(|m| format!("{}.", self.display_ref(m)))
                        .unwrap_or_default();
                    writeln!(
                        out,
                        "enum_variant {}{}::{}",
                        module_str,
                        self.interner.resolve(&*type_name),
                        self.interner.resolve(&*variant)
                    )
                    .unwrap();
                }

                // Arrays
                InstData::ArrayInit { elements } => {
                    let elements = self.rir.array_elements(elements);
                    let elems_str: Vec<String> = elements
                        .values()
                        .map(|e| self.display_ref(e).to_string())
                        .collect();
                    writeln!(out, "array_init [{}]", elems_str.join(", ")).unwrap();
                }
                InstData::ArrayRepeat { value, count } => {
                    let count_str = match count {
                        RepeatCount::Literal(n) => n.to_string(),
                        RepeatCount::Named(sym) => {
                            format!("sym:{}", sym.into_usize())
                        }
                    };
                    writeln!(
                        out,
                        "array_repeat [{}; {}]",
                        self.display_ref(*value),
                        count_str
                    )
                    .unwrap();
                }
                InstData::IndexGet { base, index } => {
                    writeln!(
                        out,
                        "index_get {}[{}]",
                        self.display_ref(*base),
                        self.display_ref(*index)
                    )
                    .unwrap();
                }
                InstData::IndexSet { base, index, value } => {
                    writeln!(
                        out,
                        "index_set {}[{}] = {}",
                        self.display_ref(*base),
                        self.display_ref(*index),
                        self.display_ref(*value)
                    )
                    .unwrap();
                }

                // Methods
                InstData::MethodCall {
                    receiver,
                    method,
                    args,
                } => {
                    let args = self.rir.call_args(args);
                    writeln!(
                        out,
                        "method_call {}.{}({})",
                        self.display_ref(*receiver),
                        self.interner.resolve(&*method),
                        self.format_call_args(args)
                    )
                    .unwrap();
                }

                // Drop
                InstData::DropFnDecl { type_name, body } => {
                    writeln!(
                        out,
                        "drop fn {}(self) {{",
                        self.interner.resolve(&*type_name)
                    )
                    .unwrap();
                    writeln!(out, "    {}", self.display_ref(*body)).unwrap();
                    writeln!(out, "}}").unwrap();
                }

                // Comptime block
                InstData::Comptime { expr } => {
                    writeln!(out, "comptime {{ {} }}", self.display_ref(*expr)).unwrap();
                }

                // Checked block
                InstData::Checked { expr } => {
                    writeln!(out, "checked {{ {} }}", self.display_ref(*expr)).unwrap();
                }

                // Type constant
                InstData::TypeConst { type_name } => {
                    let name = self.format_type(*type_name);
                    writeln!(out, "type {}", name).unwrap();
                }

                // Anonymous struct type
                InstData::AnonStructType {
                    fields, methods, ..
                } => {
                    write!(out, "struct {{ ").unwrap();
                    let fields = self.rir.anon_struct_fields(fields);
                    for (i, (name, ty)) in fields.values().enumerate() {
                        if i > 0 {
                            write!(out, ", ").unwrap();
                        }
                        let name_str = self.interner.resolve(&name);
                        let ty_str = self.format_type(ty);
                        write!(out, "{}: {}", name_str, ty_str).unwrap();
                    }
                    // Print methods if any
                    if methods.extent() > 0 {
                        let methods = self.rir.anon_struct_methods(methods);
                        let methods_str: Vec<String> = methods
                            .values()
                            .map(|m| self.display_ref(m).to_string())
                            .collect();
                        if fields.len() != 0 {
                            write!(out, ", ").unwrap();
                        }
                        write!(out, "methods: [{}]", methods_str.join(", ")).unwrap();
                    }
                    writeln!(out, " }}").unwrap();
                }

                // Anonymous enum type
                InstData::AnonEnumType {
                    variants, payloads, ..
                } => {
                    let payload_arities: Vec<usize> = self
                        .rir
                        .anon_enum_payloads(payloads, variants)
                        .map(|payload| payload.len())
                        .collect();
                    let variants = self.rir.anon_enum_variants(variants);
                    let variants_str: Vec<String> = variants
                        .iter()
                        .enumerate()
                        .map(|(i, v)| {
                            let base = self.interner.resolve(&v).to_string();
                            match payload_arities.get(i) {
                                Some(k) if *k > 0 => format!("{}/{}", base, k),
                                _ => base,
                            }
                        })
                        .collect();
                    writeln!(out, "enum {{ {} }}", variants_str.join(", ")).unwrap();
                }
            }
        }
        out
    }
}

impl fmt::Display for RirPrinter<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_string())
    }
}

#[cfg(test)]
mod resource_limit_tests {
    use super::*;

    #[test]
    fn resource_limit_message_names_the_published_ceiling() {
        // RUE-1221 / spec C.1:2: the diagnostic must name the exceeded limit.
        let payload = RirPayloadBuildError::ResourceLimitExceeded {
            family: "payload words",
        };
        let instructions = RirPayloadBuildError::ResourceLimitExceeded {
            family: "instructions",
        };
        assert_eq!(
            payload.to_string(),
            "RIR payload words exceeded the implementation limit of 4294967295 per program \
             (spec Appendix C.6:1)"
        );
        assert!(instructions.to_string().contains("RIR instructions"));
        assert!(instructions.to_string().contains("4294967295"));
    }

    #[test]
    fn build_failures_are_classified_for_the_user() {
        assert!(RirPayloadBuildError::ResourceLimitExceeded { family: "f" }.is_resource_limit());
        assert!(
            !RirPayloadBuildError::ResourceLimitExceeded { family: "f" }.is_resource_exhaustion()
        );
        assert!(RirPayloadBuildError::CapacityFailure { family: "f" }.is_resource_exhaustion());
        assert!(!RirPayloadBuildError::CapacityFailure { family: "f" }.is_resource_limit());
        let invalid = RirPayloadBuildError::InvalidBuilderInput {
            family: "f",
            reason: "r",
        };
        assert!(!invalid.is_resource_limit());
        assert!(!invalid.is_resource_exhaustion());
    }

    #[test]
    fn instruction_capacity_latch_is_clear_for_an_ordinary_owner() {
        let mut editor = RirEditor::new();
        editor.add_inst(Inst {
            data: InstData::IntConst(7),
            span: Span::default(),
        });
        assert!(editor.capacity_error().is_none());
    }

    #[test]
    fn published_instruction_ceiling_matches_the_addressable_index_space() {
        // `InstRef` reserves `u32::MAX` as the null payload, so the last
        // addressable index is `u32::MAX - 1` and the capacity is exactly the
        // published ceiling (spec Appendix C.6:1).
        assert_eq!(MAX_RIR_ENTRIES_PER_PROGRAM, u32::MAX);
        assert_eq!(u64::from(MAX_RIR_ENTRIES_PER_PROGRAM), 4_294_967_295);
    }
}

#[cfg(test)]
mod typed_payload_tests {
    use super::*;
    use lasso::ThreadedRodeo;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    struct CountingAllocator;

    thread_local! {
        static COUNT_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
        static ALLOCATION_COUNT: Cell<usize> = const { Cell::new(0) };
        static ALLOCATION_BYTES: Cell<usize> = const { Cell::new(0) };
    }

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            COUNT_ALLOCATIONS.with(|enabled| {
                if enabled.get() {
                    ALLOCATION_COUNT.with(|count| count.set(count.get() + 1));
                    ALLOCATION_BYTES.with(|bytes| bytes.set(bytes.get() + layout.size()));
                }
            });
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    fn allocations_during(f: impl FnOnce()) -> usize {
        COUNT_ALLOCATIONS.with(|enabled| enabled.set(false));
        ALLOCATION_COUNT.with(|count| count.set(0));
        ALLOCATION_BYTES.with(|bytes| bytes.set(0));
        COUNT_ALLOCATIONS.with(|enabled| enabled.set(true));
        f();
        COUNT_ALLOCATIONS.with(|enabled| enabled.set(false));
        ALLOCATION_COUNT.with(Cell::get)
    }

    fn allocation_evidence(f: impl FnOnce()) -> (usize, usize) {
        COUNT_ALLOCATIONS.with(|enabled| enabled.set(false));
        ALLOCATION_COUNT.with(|count| count.set(0));
        ALLOCATION_BYTES.with(|bytes| bytes.set(0));
        COUNT_ALLOCATIONS.with(|enabled| enabled.set(true));
        f();
        COUNT_ALLOCATIONS.with(|enabled| enabled.set(false));
        (
            ALLOCATION_COUNT.with(Cell::get),
            ALLOCATION_BYTES.with(Cell::get),
        )
    }

    fn span() -> Span {
        Span::with_file(FileId::new(7), 3, 9)
    }

    fn install_named_types(rir: &mut Rir, symbols: &[Spur]) -> Vec<RirTypeSyntaxRef> {
        let mut builder = RirTypeSyntaxBuilder::default();
        let references = symbols
            .iter()
            .map(|symbol| builder.push_named_type(*symbol).unwrap())
            .collect();
        rir.type_syntax = builder.finish();
        references
    }

    #[test]
    fn every_payload_family_round_trips() {
        let interner = ThreadedRodeo::new();
        let a = interner.get_or_intern("a");
        let b = interner.get_or_intern("b");
        let r0 = InstRef::from_raw(0);
        let r1 = InstRef::from_raw(1);
        let mut rir = Rir::new();
        let types = install_named_types(&mut rir, &[a, b]);
        let (type_a, type_b) = (types[0], types[1]);

        let intrinsic = rir.add_intrinsic_args(&[r0, r1]).unwrap();
        let internal = rir.add_internal_intrinsic_args(&[r1]).unwrap();
        let block = rir.add_block_insts(&[r0, r1]).unwrap();
        let methods = rir.add_struct_methods(&[r0]).unwrap();
        let anon_methods = rir.add_anon_struct_methods(&[r1]).unwrap();
        let elements = rir.add_array_elements(&[r0, r1]).unwrap();
        assert_eq!(
            rir.intrinsic_args(&intrinsic).values().collect::<Vec<_>>(),
            [r0, r1]
        );
        assert_eq!(
            rir.internal_intrinsic_args(&internal)
                .values()
                .collect::<Vec<_>>(),
            [r1]
        );
        assert_eq!(
            rir.block_insts(&block).values().collect::<Vec<_>>(),
            [r0, r1]
        );
        assert_eq!(rir.block_inst_count(&block), 2);
        assert_eq!(rir.block_inst(&block, 0), Some(r0));
        assert_eq!(rir.block_inst(&block, 1), Some(r1));
        assert_eq!(rir.block_inst(&block, 2), None);
        assert_eq!(
            rir.struct_methods(&methods).values().collect::<Vec<_>>(),
            [r0]
        );
        assert_eq!(
            rir.anon_struct_methods(&anon_methods)
                .values()
                .collect::<Vec<_>>(),
            [r1]
        );
        assert_eq!(
            rir.array_elements(&elements).values().collect::<Vec<_>>(),
            [r0, r1]
        );

        let call = rir
            .add_call_args(&[RirCallArg {
                value: r1,
                mode: RirArgMode::Inout,
            }])
            .unwrap();
        assert_eq!(rir.call_args(&call).get(0).unwrap().value, r1);
        let params = rir
            .add_params(&[RirParam {
                name: a,
                ty: type_b,
                mode: RirParamMode::Borrow,
                is_comptime: true,
                span: span(),
            }])
            .unwrap();
        assert_eq!(rir.params(&params).get(0).unwrap().name, a);
        let arms = rir
            .add_match_arms(&[(RirPattern::Wildcard(span()), r0)])
            .unwrap();
        assert_eq!(rir.match_arms(&arms).get(0).unwrap().1, r0);
        let inits = rir.add_field_inits(&[(a, r1)]).unwrap();
        assert_eq!(rir.field_inits(&inits).get(0).unwrap(), (a, r1));
        let fields = rir.add_struct_fields(&[(a, type_b)]).unwrap();
        let anon_fields = rir.add_anon_struct_fields(&[(b, type_a)]).unwrap();
        assert_eq!(rir.struct_fields(&fields).get(0).unwrap(), (a, type_b));
        assert_eq!(
            rir.anon_struct_fields(&anon_fields).get(0).unwrap(),
            (b, type_a)
        );
        let directives = rir
            .add_directives(&[RirDirective {
                name: a,
                args: vec![b],
                span: span(),
            }])
            .unwrap();
        assert_eq!(rir.directives(&directives).get(0).unwrap().name, a);
        let variants = rir.add_enum_variants(&[a, b]).unwrap();
        let anon_variants = rir.add_anon_enum_variants(&[b]).unwrap();
        assert_eq!(rir.enum_variants(&variants).to_vec(), [a, b]);
        assert_eq!(rir.anon_enum_variants(&anon_variants).to_vec(), [b]);
        let payloads = rir.add_enum_payloads(&[vec![type_a], vec![]]).unwrap();
        let anon_payloads = rir.add_anon_enum_payloads(&[vec![type_b]]).unwrap();
        assert_eq!(
            rir.enum_payloads(&payloads, &variants)
                .map(|payload| payload.to_vec())
                .collect::<Vec<_>>(),
            [vec![type_a], vec![]]
        );
        assert_eq!(
            rir.anon_enum_payloads(&anon_payloads, &anon_variants)
                .map(|payload| payload.to_vec())
                .collect::<Vec<_>>(),
            [vec![type_b]]
        );
    }

    fn every_payload_family_validated_rir() -> (ValidatedRir, ThreadedRodeo) {
        let interner = ThreadedRodeo::new();
        let a = interner.get_or_intern("a");
        let b = interner.get_or_intern("b");
        let mut editor = RirEditor::new();
        let type_a = editor.add_named_type(a).unwrap();
        let type_b = editor.add_named_type(b).unwrap();
        let value = editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: span(),
        });
        let block = editor.add_block(&[value], span()).unwrap();
        let directives = [RirDirective {
            name: a,
            args: vec![b],
            span: span(),
        }];
        let function = editor
            .add_fn_decl(
                &directives,
                true,
                false,
                false,
                false,
                a,
                &[RirParam {
                    name: a,
                    ty: type_b,
                    mode: RirParamMode::Borrow,
                    is_comptime: false,
                    span: span(),
                }],
                type_b,
                block,
                false,
                RirParamMode::Normal,
                false,
                false,
                span(),
            )
            .unwrap();
        editor
            .add_match(
                value,
                &[(
                    RirPattern::Path {
                        module: Some(value),
                        ctor_head: Some(value),
                        type_name: a,
                        variant: b,
                        bindings: vec![a],
                        span: span(),
                    },
                    value,
                )],
                span(),
            )
            .unwrap();
        let arguments = [RirCallArg {
            value,
            mode: RirArgMode::Borrow,
        }];
        editor.add_call(a, &arguments, span()).unwrap();
        editor.add_intrinsic(a, &[value], span()).unwrap();
        editor
            .add_internal_intrinsic(InternalIntrinsic::IterLen, &[value], span())
            .unwrap();
        editor
            .add_struct_decl(
                &directives,
                true,
                false,
                a,
                &[(a, type_b)],
                &[function],
                span(),
            )
            .unwrap();
        editor
            .add_struct_init(
                Some(value),
                Some(value),
                a,
                &[(a, value)],
                Some(span()),
                span(),
            )
            .unwrap();
        editor
            .add_enum_decl(true, false, a, &[a, b], &[vec![type_b], vec![]], span())
            .unwrap();
        editor.add_array_init(&[value, block], span()).unwrap();
        editor
            .add_anon_struct_type(
                &[(b, type_a)],
                &[function],
                RirStructuralAnchor::new(vec![RirStructuralPathSegment::AnonymousType(0)]),
                span(),
            )
            .unwrap();
        editor
            .add_anon_enum_type(
                &[b],
                &[vec![type_a]],
                RirStructuralAnchor::new(vec![RirStructuralPathSegment::AnonymousType(1)]),
                span(),
            )
            .unwrap();
        let context = RirValidationContext {
            symbol_count: interner.len(),
            source_lengths: &[(FileId::new(7), 100)],
        };
        (ValidatedRir::finish(editor, &context).unwrap(), interner)
    }

    fn every_span_family_validated_rir(
        shorthand: bool,
        file: FileId,
        shift: u32,
    ) -> (ValidatedRir, ThreadedRodeo) {
        let interner = ThreadedRodeo::new();
        let a = interner.get_or_intern("a");
        let b = interner.get_or_intern("b");
        let at = |position| Span::with_file(file, shift + position, shift + position + 1);
        let directive = |position| RirDirective {
            name: a,
            args: vec![b],
            span: at(position),
        };

        let mut editor = RirEditor::new();
        let type_b = editor.add_named_type(b).unwrap();
        let value = editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: at(0),
        });
        editor
            .add_fn_decl(
                &[directive(2)],
                false,
                false,
                false,
                false,
                a,
                &[RirParam {
                    name: a,
                    ty: type_b,
                    mode: RirParamMode::Normal,
                    is_comptime: false,
                    span: at(3),
                }],
                type_b,
                value,
                false,
                RirParamMode::Normal,
                false,
                false,
                at(1),
            )
            .unwrap();
        editor
            .add_match(
                value,
                &[
                    (RirPattern::Wildcard(at(5)), value),
                    (
                        RirPattern::Int {
                            value: 1,
                            negative: false,
                            span: at(6),
                        },
                        value,
                    ),
                    (RirPattern::Bool(true, at(7)), value),
                    (
                        RirPattern::Path {
                            module: None,
                            ctor_head: None,
                            type_name: a,
                            variant: b,
                            bindings: vec![a],
                            span: at(8),
                        },
                        value,
                    ),
                ],
                at(4),
            )
            .unwrap();
        editor
            .add_const_decl(&[directive(10)], false, a, Some(type_b), value, at(9))
            .unwrap();
        editor
            .add_alloc(
                &[directive(12)],
                Some(a),
                false,
                Some(type_b),
                value,
                false,
                at(11),
            )
            .unwrap();
        editor
            .add_struct_decl(
                &[directive(14)],
                false,
                false,
                a,
                &[(a, type_b)],
                &[],
                at(13),
            )
            .unwrap();
        editor
            .add_struct_init(
                None,
                None,
                a,
                &[(a, value)],
                shorthand.then(|| at(16)),
                at(15),
            )
            .unwrap();
        editor
            .add_anon_struct_type(
                &[(a, type_b)],
                &[],
                RirStructuralAnchor::new(vec![RirStructuralPathSegment::AnonymousType(7)]),
                at(17),
            )
            .unwrap();
        editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: at(18),
        });

        let context = RirValidationContext {
            symbol_count: interner.len(),
            source_lengths: &[(file, shift + 100)],
        };
        (ValidatedRir::finish(editor, &context).unwrap(), interner)
    }

    fn span_entries(rir: &ValidatedRir) -> Vec<(RirSpanSlot, Span)> {
        let mut entries = Vec::new();
        rir.try_visit_span_slots(
            || Ok::<_, std::convert::Infallible>(()),
            |slot, span| {
                entries.push((slot, span));
                Ok(())
            },
        )
        .unwrap();
        entries
    }

    #[test]
    fn canonical_span_slots_inventory_every_storage_family() {
        let (rir, _) = every_span_family_validated_rir(true, FileId::new(7), 0);
        let entries = span_entries(&rir);
        assert!(entries.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(
            entries
                .iter()
                .filter(|(slot, _)| slot.field() == RirSpanField::Instruction)
                .count(),
            rir.len()
        );
        for expected in [
            RirSpanField::FunctionDirective { directive: 0 },
            RirSpanField::FunctionParameter { parameter: 0 },
            RirSpanField::ConstDirective { directive: 0 },
            RirSpanField::AllocDirective { directive: 0 },
            RirSpanField::StructDirective { directive: 0 },
            RirSpanField::StructInitShorthand,
        ] {
            assert_eq!(
                entries
                    .iter()
                    .filter(|(slot, _)| slot.field() == expected)
                    .count(),
                1,
                "missing span family {expected:?}"
            );
        }
        assert_eq!(
            entries
                .iter()
                .filter(|(slot, _)| matches!(slot.field(), RirSpanField::MatchPattern { .. }))
                .count(),
            4
        );
    }

    #[test]
    fn span_slot_schema_ignores_coordinates_and_optional_slots_do_not_renumber_peers() {
        let (first, _) = every_span_family_validated_rir(true, FileId::new(7), 0);
        let (relocated, _) = every_span_family_validated_rir(true, FileId::new(9), 40);
        let first_entries = span_entries(&first);
        let relocated_entries = span_entries(&relocated);
        assert_eq!(
            first_entries
                .iter()
                .map(|(slot, _)| slot)
                .collect::<Vec<_>>(),
            relocated_entries
                .iter()
                .map(|(slot, _)| slot)
                .collect::<Vec<_>>()
        );
        assert!(
            first_entries
                .iter()
                .zip(&relocated_entries)
                .all(|((_, left), (_, right))| left != right)
        );

        let (explicit, _) = every_span_family_validated_rir(false, FileId::new(7), 0);
        let explicit_slots = span_entries(&explicit)
            .into_iter()
            .map(|(slot, _)| slot)
            .collect::<Vec<_>>();
        let shorthand_slot = first_entries
            .iter()
            .find(|(slot, _)| slot.field() == RirSpanField::StructInitShorthand)
            .unwrap()
            .0;
        let without_optional = first_entries
            .iter()
            .map(|(slot, _)| *slot)
            .filter(|slot| *slot != shorthand_slot)
            .collect::<Vec<_>>();
        assert_eq!(explicit_slots, without_optional);
        assert!(explicit_slots.iter().any(|slot| {
            slot.instruction().as_u32() > shorthand_slot.instruction().as_u32()
                && slot.field() == RirSpanField::Instruction
        }));
    }

    #[test]
    fn slot_aware_remap_is_atomic_and_preserves_anonymous_anchors() {
        let (source, interner) = every_span_family_validated_rir(true, FileId::new(7), 0);
        let source_anchor = source
            .iter()
            .find_map(|(_, inst)| match &inst.data {
                InstData::AnonStructType { anchor, .. } => Some(anchor.clone()),
                _ => None,
            })
            .unwrap();
        let mut destination = RirEditor::new();
        destination
            .try_append_remapped_with_span_slots(
                &source,
                std::convert::identity,
                || Ok::<_, &'static str>(()),
                |slot, span| {
                    let tag_offset = match slot.field() {
                        RirSpanField::Instruction => 100,
                        _ => 200,
                    };
                    Ok(Span::with_file(
                        FileId::new(9),
                        span.start + tag_offset,
                        span.end + tag_offset,
                    ))
                },
            )
            .unwrap();
        let destination = ValidatedRir::finish(
            destination,
            &RirValidationContext {
                symbol_count: interner.len(),
                source_lengths: &[(FileId::new(9), 1000)],
            },
        )
        .unwrap();
        let destination_entries = span_entries(&destination);
        assert!(destination_entries.iter().all(|(slot, span)| {
            span.file_id == FileId::new(9)
                && span.start
                    >= if slot.field() == RirSpanField::Instruction {
                        100
                    } else {
                        200
                    }
        }));
        let destination_anchor = destination
            .iter()
            .find_map(|(_, inst)| match &inst.data {
                InstData::AnonStructType { anchor, .. } => Some(anchor.clone()),
                _ => None,
            })
            .unwrap();
        assert_eq!(destination_anchor, source_anchor);
    }

    #[test]
    fn validated_span_rewrite_preserves_storage_and_covers_every_span_family() {
        let (source, interner) = every_span_family_validated_rir(true, FileId::new(7), 0);
        let instruction_storage = source.0.instructions.as_ptr();
        let instruction_capacity = source.0.instructions.capacity();
        let payload_storage = source.0.extra.as_ptr();
        let payload_capacity = source.0.extra.capacity();
        let source_entries = span_entries(&source);
        let mut visited = Vec::new();

        let rewritten = source
            .try_rewrite_span_slots(
                &RirValidationContext {
                    symbol_count: interner.len(),
                    source_lengths: &[(FileId::new(9), 1000)],
                },
                || Ok::<_, &'static str>(()),
                |slot, span| {
                    visited.push(slot);
                    Ok(Span::with_file(
                        FileId::new(9),
                        span.start + 40,
                        span.end + 40,
                    ))
                },
            )
            .unwrap();
        let (expected, _) = every_span_family_validated_rir(true, FileId::new(9), 40);

        assert_eq!(rewritten.0.instructions.as_ptr(), instruction_storage);
        assert_eq!(rewritten.0.instructions.capacity(), instruction_capacity);
        assert_eq!(rewritten.0.extra.as_ptr(), payload_storage);
        assert_eq!(rewritten.0.extra.capacity(), payload_capacity);
        assert_eq!(
            visited,
            source_entries
                .iter()
                .map(|(slot, _)| *slot)
                .collect::<Vec<_>>()
        );
        assert!(rewritten.exact_eq(&expected));
        assert_eq!(span_entries(&rewritten), span_entries(&expected));
    }

    #[test]
    fn validated_span_rewrite_rejects_mapping_and_context_failures() {
        let (source, interner) = every_span_family_validated_rir(true, FileId::new(7), 0);
        let rejected_slot = span_entries(&source)
            .into_iter()
            .find(|(slot, _)| slot.field() == RirSpanField::StructInitShorthand)
            .unwrap()
            .0;
        let error = source
            .try_rewrite_span_slots(
                &RirValidationContext {
                    symbol_count: interner.len(),
                    source_lengths: &[(FileId::new(9), 1000)],
                },
                || Ok::<_, &'static str>(()),
                |slot, span| {
                    if slot == rejected_slot {
                        Err("rejected mapping")
                    } else {
                        Ok(span)
                    }
                },
            )
            .unwrap_err();
        assert!(matches!(
            error,
            RirSpanRemapError::Mapping {
                slot,
                error: "rejected mapping"
            } if slot == rejected_slot
        ));

        let (source, interner) = every_span_family_validated_rir(true, FileId::new(7), 0);
        let error = source
            .try_rewrite_span_slots(
                &RirValidationContext {
                    symbol_count: interner.len(),
                    source_lengths: &[(FileId::new(9), 10)],
                },
                || Ok::<_, std::convert::Infallible>(()),
                |_slot, span| Ok(Span::with_file(FileId::new(9), span.start, span.end)),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            RirSpanRemapError::MalformedPayload(RirPayloadError {
                reason: "span range is outside its canonical source",
                ..
            })
        ));

        let (source, interner) = every_span_family_validated_rir(true, FileId::new(7), 0);
        let remaining = std::cell::Cell::new(span_entries(&source).len());
        let error = source
            .try_rewrite_span_slots(
                &RirValidationContext {
                    symbol_count: interner.len(),
                    source_lengths: &[(FileId::new(7), 100)],
                },
                || {
                    if remaining.get() == 0 {
                        Err("target validation canceled")
                    } else {
                        Ok(())
                    }
                },
                |_slot, span| {
                    remaining.set(remaining.get() - 1);
                    Ok(span)
                },
            )
            .unwrap_err();
        assert!(matches!(
            error,
            RirSpanRemapError::Checkpoint("target validation canceled")
        ));
    }

    #[test]
    fn validated_rir_retained_charge_counts_shared_anchor_pointees_per_path() {
        let interner = ThreadedRodeo::new();
        let name = interner.get_or_intern("anchor-heavy");
        let anchor = RirStructuralAnchor::new(vec![
            RirStructuralPathSegment::Body,
            RirStructuralPathSegment::Statement(1),
            RirStructuralPathSegment::Operand(2),
            RirStructuralPathSegment::StringLiteral(3),
        ]);
        let anchor_pointee = std::mem::size_of_val(anchor.segments()) as u64;
        let span = Span::with_file(FileId::new(7), 0, 1);
        let mut editor = RirEditor::new();
        let named_type = editor.add_named_type(name).unwrap();
        editor.add_inst(Inst {
            data: InstData::TypeConst {
                type_name: named_type,
            },
            span,
        });
        editor.add_inst(Inst {
            data: InstData::StringConst {
                content: name,
                anchor: anchor.clone(),
            },
            span,
        });
        editor.add_inst(Inst {
            data: InstData::VarRef {
                name,
                anchor: Some(anchor.clone()),
            },
            span,
        });
        editor
            .add_anon_struct_type(&[], &[], anchor.clone(), span)
            .unwrap();
        editor.add_anon_enum_type(&[], &[], anchor, span).unwrap();
        let rir = ValidatedRir::finish(
            editor,
            &RirValidationContext {
                symbol_count: interner.len(),
                source_lengths: &[(FileId::new(7), 1)],
            },
        )
        .unwrap();

        let dense = (rir.len() * std::mem::size_of::<Inst>()) as u64
            + (rir.extra_len() * std::mem::size_of::<u32>()) as u64
            + rir.type_syntax().retained_allocation_charge();
        assert_eq!(
            rir.retained_allocation_charge(),
            dense + 4 * anchor_pointee,
            "each of four reaching instructions charges the shared Arc pointee in full"
        );
    }

    #[test]
    fn declaration_interval_projection_is_candidate_local_and_rejects_open_owner_edges() {
        let interner = ThreadedRodeo::new();
        let name = interner.get_or_intern("f");
        let mut source = RirEditor::new();
        let unit = source.add_unit_type().unwrap();
        let mut method_roots = Vec::new();
        for _ in 0..3 {
            let body = source.add_inst(Inst {
                data: InstData::UnitConst,
                span: span(),
            });
            method_roots.push(
                source
                    .add_fn_decl(
                        &[],
                        false,
                        false,
                        false,
                        false,
                        name,
                        &[],
                        unit,
                        body,
                        true,
                        RirParamMode::Normal,
                        false,
                        false,
                        span(),
                    )
                    .unwrap(),
            );
        }
        source
            .add_struct_decl(&[], false, false, name, &[], &method_roots, span())
            .unwrap();
        let source = ValidatedRir::finish(
            source,
            &RirValidationContext {
                symbol_count: interner.len(),
                source_lengths: &[(FileId::new(7), 100)],
            },
        )
        .unwrap();

        let mut projected = RirEditor::new();
        projected
            .try_append_instruction_range_remapped_with_span_slots(
                &source,
                2..4,
                std::convert::identity,
                || Ok::<_, std::convert::Infallible>(()),
                |_, span| Ok(span),
            )
            .unwrap();
        assert_eq!(projected.len(), 2);
        let InstData::FnDecl { body, .. } = projected.get(InstRef::from_raw(1)).data else {
            panic!("middle method root must remain a function declaration")
        };
        assert_eq!(body, InstRef::from_raw(0));

        let before = (projected.len(), projected.extra_len());
        let error = projected
            .try_append_instruction_range_remapped_with_span_slots(
                &source,
                6..7,
                std::convert::identity,
                || Ok::<_, std::convert::Infallible>(()),
                |_, span| Ok(span),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            RirSpanRemapError::ForeignInstructionEdge {
                instruction,
                child
            } if instruction == InstRef::from_raw(6) && method_roots.contains(&child)
        ));
        assert_eq!((projected.len(), projected.extra_len()), before);
    }

    #[test]
    fn methodless_struct_shell_composition_preserves_payloads_and_wires_existing_methods() {
        let source_symbols = ThreadedRodeo::new();
        let source_name = source_symbols.get_or_intern("Container");
        let source_field = source_symbols.get_or_intern("value");
        let source_ty = source_symbols.get_or_intern("i32");
        let source_directive = source_symbols.get_or_intern("derive");
        let source_arg = source_symbols.get_or_intern("copy");
        let mut source = RirEditor::new();
        let source_ty = source.add_named_type(source_ty).unwrap();
        let source_root = source
            .add_struct_decl(
                &[RirDirective {
                    name: source_directive,
                    args: vec![source_arg],
                    span: Span::with_file(FileId::new(7), 11, 17),
                }],
                true,
                true,
                source_name,
                &[(source_field, source_ty)],
                &[],
                Span::with_file(FileId::new(7), 3, 40),
            )
            .unwrap();
        let source = ValidatedRir::finish(
            source,
            &RirValidationContext {
                symbol_count: source_symbols.len(),
                source_lengths: &[(FileId::new(7), 100)],
            },
        )
        .unwrap();

        let destination_symbols = ThreadedRodeo::new();
        let method_name = destination_symbols.get_or_intern("method");
        let mut destination = RirEditor::new();
        let unit = destination.add_unit_type().unwrap();
        let mut methods = Vec::new();
        for _ in 0..2 {
            let body = destination.add_inst(Inst {
                data: InstData::UnitConst,
                span: Span::with_file(FileId::new(9), 1, 2),
            });
            methods.push(
                destination
                    .add_fn_decl(
                        &[],
                        false,
                        false,
                        false,
                        false,
                        method_name,
                        &[],
                        unit,
                        body,
                        true,
                        RirParamMode::Normal,
                        false,
                        false,
                        Span::with_file(FileId::new(9), 1, 2),
                    )
                    .unwrap(),
            );
        }
        let range = destination
            .try_append_methodless_struct_shell_with_methods(
                &source,
                source_root,
                &methods,
                |symbol| {
                    destination_symbols.get_or_intern(
                        source_symbols
                            .try_resolve(&symbol)
                            .expect("source shell symbol belongs to its interner"),
                    )
                },
                || Ok::<_, std::convert::Infallible>(()),
                |_, span| {
                    Ok(Span::with_file(
                        FileId::new(9),
                        span.start + 100,
                        span.end + 100,
                    ))
                },
            )
            .unwrap();
        assert_eq!(range.instructions, 4..5);
        let destination = ValidatedRir::finish(
            destination,
            &RirValidationContext {
                symbol_count: destination_symbols.len(),
                source_lengths: &[(FileId::new(9), 1000)],
            },
        )
        .unwrap();
        let InstData::StructDecl {
            directives,
            is_pub,
            is_linear,
            name,
            fields,
            methods: actual_methods,
        } = &destination.get(InstRef::from_raw(4)).data
        else {
            panic!("composed shell must remain a struct declaration")
        };
        assert!(*is_pub);
        assert!(*is_linear);
        assert_eq!(destination_symbols.resolve(name), "Container");
        assert_eq!(
            destination
                .struct_fields(fields)
                .values()
                .map(|(name, ty)| (
                    destination_symbols.resolve(&name),
                    destination
                        .type_syntax()
                        .render_type_with(ty, |symbol| destination_symbols.resolve(symbol))
                        .unwrap(),
                ))
                .collect::<Vec<_>>(),
            [("value", "i32".to_owned())]
        );
        assert_eq!(
            destination
                .struct_methods(actual_methods)
                .values()
                .collect::<Vec<_>>(),
            methods
        );
        let directives = destination
            .directives(directives)
            .iter()
            .collect::<Vec<_>>();
        assert_eq!(directives.len(), 1);
        assert_eq!(destination_symbols.resolve(&directives[0].name), "derive");
        assert_eq!(
            directives[0]
                .args
                .values()
                .map(|arg| destination_symbols.resolve(&arg))
                .collect::<Vec<_>>(),
            ["copy"]
        );
        assert_eq!(
            directives[0].span,
            Span::with_file(FileId::new(9), 111, 117)
        );
        assert_eq!(
            destination.get(InstRef::from_raw(4)).span,
            Span::with_file(FileId::new(9), 103, 140)
        );
    }

    #[test]
    fn struct_shell_composition_rejects_invalid_sources_atomically() {
        let symbols = ThreadedRodeo::new();
        let name = symbols.get_or_intern("S");
        let validation = RirValidationContext {
            symbol_count: symbols.len(),
            source_lengths: &[(FileId::new(7), 100)],
        };

        let mut non_struct = RirEditor::new();
        non_struct.add_inst(Inst {
            data: InstData::UnitConst,
            span: span(),
        });
        let non_struct = ValidatedRir::finish(non_struct, &validation).unwrap();

        let mut with_method = RirEditor::new();
        let unit = with_method.add_unit_type().unwrap();
        let body = with_method.add_inst(Inst {
            data: InstData::UnitConst,
            span: span(),
        });
        let method = with_method
            .add_fn_decl(
                &[],
                false,
                false,
                false,
                false,
                name,
                &[],
                unit,
                body,
                true,
                RirParamMode::Normal,
                false,
                false,
                span(),
            )
            .unwrap();
        let nonempty_root = with_method
            .add_struct_decl(&[], false, false, name, &[], &[method], span())
            .unwrap();
        let with_method = ValidatedRir::finish(with_method, &validation).unwrap();

        let mut destination = RirEditor::new();
        let before = (destination.len(), destination.extra_len());
        for (source, root, expected_reason) in [
            (
                &non_struct,
                InstRef::from_raw(0),
                "source root is not a struct declaration",
            ),
            (
                &with_method,
                nonempty_root,
                "source struct declaration is not methodless",
            ),
        ] {
            let error = destination
                .try_append_methodless_struct_shell_with_methods(
                    source,
                    root,
                    &[],
                    std::convert::identity,
                    || Ok::<_, std::convert::Infallible>(()),
                    |_, span| Ok(span),
                )
                .unwrap_err();
            assert!(matches!(
                error,
                RirSpanRemapError::Build(RirPayloadBuildError::InvalidBuilderInput {
                    family: "struct shell composition",
                    reason,
                }) if reason == expected_reason
            ));
            assert_eq!((destination.len(), destination.extra_len()), before);
        }
    }

    #[test]
    fn declaration_interval_projection_cancellation_rolls_back_atomically() {
        let (source, _) = every_span_family_validated_rir(true, FileId::new(7), 0);
        let mut destination = RirEditor::new();
        let mut checkpoints = 0_u32;
        let before = (destination.len(), destination.extra_len());
        let error = destination
            .try_append_instruction_range_remapped_with_span_slots(
                &source,
                0..u32::try_from(source.len()).unwrap(),
                std::convert::identity,
                || {
                    checkpoints += 1;
                    (checkpoints < 3).then_some(()).ok_or("canceled")
                },
                |_, span| Ok(span),
            )
            .unwrap_err();
        assert!(matches!(error, RirSpanRemapError::Checkpoint("canceled")));
        assert_eq!((destination.len(), destination.extra_len()), before);
    }

    #[test]
    fn slot_aware_remap_cancellation_rolls_back_partial_append() {
        let (source, interner) = every_span_family_validated_rir(true, FileId::new(7), 0);
        let mut traversal_checkpoints = 0;
        source
            .try_visit_span_slots(
                || {
                    traversal_checkpoints += 1;
                    Ok::<_, std::convert::Infallible>(())
                },
                |_, _| Ok(()),
            )
            .unwrap();

        let mut destination = RirEditor::new();
        let prefix = destination.add_inst(Inst {
            data: InstData::UnitConst,
            span: span(),
        });
        destination
            .add_call(interner.get("a").unwrap(), &[], span())
            .unwrap();
        let before = (destination.len(), destination.extra_len(), prefix);
        let mut checkpoints = 0;
        let error = destination
            .try_append_remapped_with_span_slots(
                &source,
                std::convert::identity,
                || {
                    checkpoints += 1;
                    if checkpoints > traversal_checkpoints + 2 {
                        Err("cancelled")
                    } else {
                        Ok(())
                    }
                },
                |_, span| Ok(span),
            )
            .unwrap_err();
        assert!(matches!(error, RirSpanRemapError::Checkpoint("cancelled")));
        assert_eq!(
            (destination.len(), destination.extra_len()),
            (before.0, before.1)
        );
    }

    #[test]
    fn raw_span_traversal_reports_malformed_payload() {
        let mut rir = Rir::new();
        rir.extra.extend([1, PatternKind::Path as u32]);
        let arms = RirMatchArmsRange::from_parts(0, 2);
        rir.add_inst(Inst {
            data: InstData::Match {
                scrutinee: InstRef::from_raw(0),
                arms,
            },
            span: span(),
        });
        let error = rir
            .try_visit_span_slots(|| Ok::<_, std::convert::Infallible>(()), |_, _| Ok(()))
            .unwrap_err();
        assert!(matches!(error, RirSpanTraversalError::MalformedPayload(_)));
    }

    #[test]
    fn large_span_remap_work_and_allocations_are_linear() {
        const COUNT: usize = 4096;
        let mut source = RirEditor::new();
        for index in 0..COUNT {
            source.add_inst(Inst {
                data: InstData::UnitConst,
                span: Span::new(index as u32, index as u32 + 1),
            });
        }
        let source = ValidatedRir::finish(
            source,
            &RirValidationContext {
                symbol_count: 0,
                source_lengths: &[(FileId::new(0), COUNT as u32 + 1)],
            },
        )
        .unwrap();
        let mut destination = RirEditor::new();
        let mut checkpoints = 0;
        let mut mappings = 0;
        let (allocations, _) = allocation_evidence(|| {
            destination
                .try_append_remapped_with_span_slots(
                    &source,
                    std::convert::identity,
                    || {
                        checkpoints += 1;
                        Ok::<_, std::convert::Infallible>(())
                    },
                    |_, span| {
                        mappings += 1;
                        Ok(span)
                    },
                )
                .unwrap();
        });
        assert_eq!(mappings, COUNT);
        assert_eq!(checkpoints, COUNT * 2);
        assert!(
            allocations < 64,
            "dense remap unexpectedly allocated {allocations} times"
        );
    }

    #[test]
    fn append_remapped_covers_every_payload_family_at_nonzero_offsets() {
        let (source, interner) = every_payload_family_validated_rir();
        let a = interner.get("a").unwrap();
        let b = interner.get("b").unwrap();
        let mut destination = RirEditor::new();
        let prefix = destination.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::with_file(FileId::new(9), 0, 1),
        });
        destination
            .add_call(
                a,
                &[RirCallArg {
                    value: prefix,
                    mode: RirArgMode::Normal,
                }],
                span(),
            )
            .unwrap();
        let instruction_offset = destination.len() as u32;
        let payload_offset = destination.extra_len() as u32;
        assert_ne!(instruction_offset, 0);
        assert_ne!(payload_offset, 0);

        let appended = destination
            .append_remapped_with_spans(
                &source,
                |symbol| if symbol == a { b } else { a },
                |source| Span::with_file(FileId::new(9), source.start + 10, source.end + 10),
            )
            .unwrap();
        assert_eq!(appended.instructions.start, instruction_offset);
        assert_eq!(appended.extra.start, payload_offset);
        assert_eq!(appended.instructions.len(), source.len());
        assert_eq!(appended.extra.len(), source.extra_len());

        let context = RirValidationContext {
            symbol_count: interner.len(),
            source_lengths: &[(FileId::new(7), 100), (FileId::new(9), 1000)],
        };
        let destination = ValidatedRir::finish(destination, &context).unwrap();
        assert!(
            destination
                .iter()
                .skip(instruction_offset as usize)
                .all(|(_, instruction)| instruction.span.file_id == FileId::new(9))
        );
        let appended_function = destination
            .iter()
            .skip(instruction_offset as usize)
            .find_map(|(_, instruction)| match &instruction.data {
                InstData::FnDecl {
                    directives, params, ..
                } => Some((directives, params)),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            destination
                .directives(appended_function.0)
                .get(0)
                .unwrap()
                .name,
            b
        );
        assert_eq!(
            destination.params(appended_function.1).get(0).unwrap().span,
            Span::with_file(FileId::new(9), 13, 19)
        );
        let appended_match = destination
            .iter()
            .skip(instruction_offset as usize)
            .find_map(|(_, instruction)| match &instruction.data {
                InstData::Match { arms, .. } => Some(arms),
                _ => None,
            })
            .unwrap();
        let (pattern, body) = destination.match_arms(appended_match).get(0).unwrap();
        assert_eq!(body.as_u32(), instruction_offset);
        match pattern {
            RirPatternView::Path {
                type_name,
                bindings,
                span,
                ..
            } => {
                assert_eq!(type_name, b);
                assert_eq!(bindings.to_vec(), [b]);
                assert_eq!(span.file_id, FileId::new(9));
            }
            _ => panic!("expected remapped path pattern"),
        }
        let remapped_anchors = destination
            .iter()
            .skip(instruction_offset as usize)
            .filter_map(|(_, instruction)| match &instruction.data {
                InstData::AnonStructType { anchor, .. } | InstData::AnonEnumType { anchor, .. } => {
                    Some(anchor.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            remapped_anchors,
            [
                RirStructuralAnchor::new(vec![RirStructuralPathSegment::AnonymousType(0)]),
                RirStructuralAnchor::new(vec![RirStructuralPathSegment::AnonymousType(1)]),
            ]
        );
    }

    #[test]
    fn append_remapped_preserves_string_anchor_across_symbol_and_file_domains() {
        let interner = ThreadedRodeo::new();
        let source_symbol = interner.get_or_intern("source");
        let destination_symbol = interner.get_or_intern("destination");
        let anchor = RirStructuralAnchor::new(vec![
            RirStructuralPathSegment::Body,
            RirStructuralPathSegment::Statement(2),
            RirStructuralPathSegment::StringLiteral(0),
        ]);
        let mut source = RirEditor::new();
        source.add_inst(Inst {
            data: InstData::StringConst {
                content: source_symbol,
                anchor: anchor.clone(),
            },
            span: Span::with_file(FileId::new(7), 3, 11),
        });
        let source = ValidatedRir::finish(
            source,
            &RirValidationContext {
                symbol_count: interner.len(),
                source_lengths: &[(FileId::new(7), 20)],
            },
        )
        .unwrap();
        let mut destination = RirEditor::new();
        destination
            .append_remapped_with_spans(
                &source,
                |_| destination_symbol,
                |span| Span::with_file(FileId::new(9), span.start + 20, span.end + 20),
            )
            .unwrap();
        let destination = ValidatedRir::finish(
            destination,
            &RirValidationContext {
                symbol_count: interner.len(),
                source_lengths: &[(FileId::new(9), 100)],
            },
        )
        .unwrap();
        let (_, instruction) = destination.iter().next().unwrap();
        let InstData::StringConst {
            content,
            anchor: remapped_anchor,
        } = &instruction.data
        else {
            panic!("expected string const")
        };
        assert_eq!(*content, destination_symbol);
        assert_eq!(*remapped_anchor, anchor);
        assert_eq!(instruction.span, Span::with_file(FileId::new(9), 23, 31));
    }

    #[test]
    fn float_const_text_is_remapped_across_symbol_domains() {
        // Module merging re-homes every instruction into the program-wide RIR,
        // translating owner-local symbols as it goes. A `FloatConst`'s text is
        // a symbol, so it must be translated rather than copied — a merged
        // program that kept the source symbol would resolve the literal
        // against the wrong interner (ADR-0065 §3, RUE-1069).
        let interner = ThreadedRodeo::new();
        let source_symbol = interner.get_or_intern("6.022e23");
        let destination_symbol = interner.get_or_intern("0.5");
        let mut source = RirEditor::new();
        source.add_inst(Inst {
            data: InstData::FloatConst {
                text: source_symbol,
            },
            span: Span::with_file(FileId::new(7), 3, 11),
        });
        let source = ValidatedRir::finish(
            source,
            &RirValidationContext {
                symbol_count: interner.len(),
                source_lengths: &[(FileId::new(7), 20)],
            },
        )
        .unwrap();

        let mut destination = RirEditor::new();
        destination
            .append_remapped_with_spans(
                &source,
                |_| destination_symbol,
                |span| Span::with_file(FileId::new(9), span.start + 20, span.end + 20),
            )
            .unwrap();
        let destination = ValidatedRir::finish(
            destination,
            &RirValidationContext {
                symbol_count: interner.len(),
                source_lengths: &[(FileId::new(9), 100)],
            },
        )
        .unwrap();

        let (_, instruction) = destination.iter().next().unwrap();
        let InstData::FloatConst { text } = &instruction.data else {
            panic!("expected a float const, got {:?}", instruction.data);
        };
        assert_eq!(*text, destination_symbol);
        assert_eq!(instruction.span, Span::with_file(FileId::new(9), 23, 31));
    }

    #[test]
    fn float_const_symbol_is_validated_like_every_other_symbol_payload() {
        // A `FloatConst` whose text symbol is outside the compilation's
        // interner is a malformed producer request, caught by RIR validation
        // rather than surfacing as a bogus literal downstream.
        let mut rir = RirEditor::new();
        rir.add_inst(Inst {
            data: InstData::FloatConst {
                text: Spur::try_from_usize(41).unwrap(),
            },
            span: Span::with_file(FileId::new(0), 0, 3),
        });
        let error = ValidatedRir::finish(
            rir,
            &RirValidationContext {
                symbol_count: 3,
                source_lengths: &[(FileId::new(0), 20)],
            },
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("symbol"),
            "unexpected validation error: {error}"
        );
    }

    #[test]
    fn empty_payloads_are_canonical_and_borrowed_views_are_empty() {
        let mut rir = Rir::new();
        let call = rir.add_call_args(&[]).unwrap();
        let params = rir.add_params(&[]).unwrap();
        let arms = rir.add_match_arms(&[]).unwrap();
        let directives = rir.add_directives(&[]).unwrap();
        assert_eq!(rir.extra_len(), 0);
        assert!(rir.call_args(&call).is_empty());
        assert!(rir.params(&params).is_empty());
        assert!(rir.match_arms(&arms).is_empty());
        assert!(rir.directives(&directives).is_empty());
    }

    #[test]
    fn validation_reports_family_range_and_record_deterministically() {
        let mut rir = Rir::new();
        rir.extra.extend([1, PatternKind::Path as u32]);
        let arms = RirMatchArmsRange::from_parts(0, 2);
        rir.add_inst(Inst {
            data: InstData::Match {
                scrutinee: InstRef::from_raw(0),
                arms,
            },
            span: span(),
        });
        let error = rir.validate_payloads().unwrap_err();
        assert_eq!(
            error,
            rir_payload_error! {
                family: "match arms",
                start: 0,
                extent: 2,
                record: Some(0),
                expected: MATCH_PATH_BINDING_COUNT + 1,
                actual: 1,
                reason: "path record header is truncated",
            }
        );
        assert_eq!(error.phase(), "RIR payload decode");
        assert_eq!(error.expected_width(), MATCH_PATH_BINDING_COUNT + 1);
        assert_eq!(error.actual_width(), 1);
        let rendered = error.to_string();
        assert!(rendered.contains("match arms"));
        assert!(rendered.contains("start=0"));
        assert!(rendered.contains("record 0"));
        assert!(rendered.contains(&format!(
            "expected width={}, actual width=1",
            MATCH_PATH_BINDING_COUNT + 1
        )));
    }

    #[test]
    fn validation_rejects_noncanonical_empty_ranges() {
        let mut rir = Rir::new();
        let args = RirCallArgsRange::from_parts(1, 0);
        rir.add_inst(Inst {
            data: InstData::Call {
                name: Spur::default(),
                args,
            },
            span: span(),
        });
        assert_eq!(
            rir.validate_payloads().unwrap_err().reason,
            "noncanonical empty range"
        );
    }

    #[test]
    fn validation_rejects_partial_fixed_records_and_invalid_modes() {
        let mut partial = Rir::new();
        partial.extra.push(0);
        let args = RirCallArgsRange::from_parts(0, 1);
        partial.add_inst(Inst {
            data: InstData::Call {
                name: Spur::default(),
                args,
            },
            span: span(),
        });
        let error = partial.validate_payloads().unwrap_err();
        assert_eq!(error.reason, "payload ends in a partial record");
        assert_eq!(
            (error.expected_width(), error.actual_width()),
            (CALL_ARG_SCHEMA.width, 1)
        );

        let mut invalid_mode = Rir::new();
        invalid_mode.extra.extend([0, 99]);
        let args = RirCallArgsRange::from_parts(0, 2);
        invalid_mode.add_inst(Inst {
            data: InstData::Call {
                name: Spur::default(),
                args,
            },
            span: span(),
        });
        assert_eq!(
            invalid_mode.validate_payloads().unwrap_err().reason,
            "invalid argument mode"
        );
    }

    #[test]
    fn validation_rejects_unknown_tags_trailing_words_and_bad_enum_cardinality() {
        let mut unknown = Rir::new();
        unknown.extra.extend([1, 99]);
        let arms = RirMatchArmsRange::from_parts(0, 2);
        unknown.add_inst(Inst {
            data: InstData::Match {
                scrutinee: InstRef::from_raw(0),
                arms,
            },
            span: span(),
        });
        let error = unknown.validate_payloads().unwrap_err();
        assert_eq!(error.reason, "invalid pattern kind");
        assert_eq!((error.expected_width(), error.actual_width()), (1, 1));

        let mut trailing = Rir::new();
        trailing.extra.extend([0, 7]);
        let directives = RirDirectivesRange::from_parts(0, 2);
        trailing.add_inst(Inst {
            data: InstData::ConstDecl {
                directives,
                is_pub: false,
                name: Spur::default(),
                ty: None,
                init: InstRef::from_raw(0),
            },
            span: span(),
        });
        assert_eq!(
            trailing.validate_payloads().unwrap_err().reason,
            "trailing words after final record"
        );

        let mut cardinality = Rir::new();
        cardinality.extra.extend([0, 0, 7]);
        let variants = RirEnumVariantsRange::from_parts(0, 1);
        let payloads = RirEnumPayloadsRange::from_parts(1, 2);
        cardinality.add_inst(Inst {
            data: InstData::EnumDecl {
                is_pub: false,
                is_non_exhaustive: false,
                name: Spur::default(),
                variants,
                payloads,
            },
            span: span(),
        });
        assert_eq!(
            cardinality.validate_payloads().unwrap_err().reason,
            "trailing words after variant payloads"
        );
    }

    fn context() -> RirValidationContext<'static> {
        static SOURCES: [(FileId, u32); 1] = [(FileId::new(7), 100)];
        RirValidationContext {
            symbol_count: 1,
            source_lengths: &SOURCES,
        }
    }

    #[test]
    fn finish_rejects_noncanonical_match_scalars_before_iteration() {
        let mut boolean = RirEditor::new();
        let value = boolean.add_inst(Inst {
            data: InstData::UnitConst,
            span: span(),
        });
        boolean
            .add_match(value, &[(RirPattern::Bool(true, span()), value)], span())
            .unwrap();
        boolean.rir.extra[5] = 2;
        assert_eq!(
            ValidatedRir::finish(boolean, &context())
                .unwrap_err()
                .reason,
            "invalid boolean scalar"
        );

        let mut integer = RirEditor::new();
        let value = integer.add_inst(Inst {
            data: InstData::UnitConst,
            span: span(),
        });
        integer
            .add_match(
                value,
                &[(
                    RirPattern::Int {
                        value: 1,
                        negative: false,
                        span: span(),
                    },
                    value,
                )],
                span(),
            )
            .unwrap();
        integer.rir.extra[7] = 2;
        assert_eq!(
            ValidatedRir::finish(integer, &context())
                .unwrap_err()
                .reason,
            "invalid integer-sign flag"
        );
    }

    #[test]
    fn finish_rejects_unrepresentable_directive_argument_before_iteration() {
        let symbol = Spur::try_from_usize(0).unwrap();
        let mut editor = RirEditor::new();
        let value = editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: span(),
        });
        editor
            .add_const_decl(
                &[RirDirective {
                    name: symbol,
                    args: vec![symbol],
                    span: span(),
                }],
                false,
                symbol,
                None,
                value,
                span(),
            )
            .unwrap();
        editor.rir.extra[DIRECTIVE_ARGS_START + 1] = u32::MAX;

        assert_eq!(
            ValidatedRir::finish(editor, &context()).unwrap_err(),
            rir_payload_error! {
                family: RirDirectivesRange::FAMILY,
                start: 0,
                extent: 7,
                record: Some(0),
                expected: 6,
                actual: 6,
                reason: "symbol word is not representable",
            }
        );
    }

    #[test]
    fn finish_rejects_unrepresentable_match_binding_before_iteration() {
        let symbol = Spur::try_from_usize(0).unwrap();
        let mut editor = RirEditor::new();
        let value = editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: span(),
        });
        editor
            .add_match(
                value,
                &[(
                    RirPattern::Path {
                        module: None,
                        ctor_head: None,
                        type_name: symbol,
                        variant: symbol,
                        bindings: vec![symbol],
                        span: span(),
                    },
                    value,
                )],
                span(),
            )
            .unwrap();
        editor.rir.extra[MATCH_PATH_BINDINGS_START + 1] = u32::MAX;

        assert_eq!(
            ValidatedRir::finish(editor, &context()).unwrap_err(),
            rir_payload_error! {
                family: RirMatchArmsRange::FAMILY,
                start: 0,
                extent: 12,
                record: Some(0),
                expected: 11,
                actual: 11,
                reason: "symbol word is not representable",
            }
        );
    }

    #[test]
    fn finish_rejects_out_of_owner_match_refs_and_context_values() {
        let symbol = Spur::try_from_usize(0).unwrap();
        for (module, ctor, body) in [
            (Some(InstRef::from_raw(99)), None, InstRef::from_raw(0)),
            (None, Some(InstRef::from_raw(99)), InstRef::from_raw(0)),
            (None, None, InstRef::from_raw(99)),
        ] {
            let mut editor = RirEditor::new();
            let scrutinee = editor.add_inst(Inst {
                data: InstData::UnitConst,
                span: span(),
            });
            editor
                .add_match(
                    scrutinee,
                    &[(
                        RirPattern::Path {
                            module,
                            ctor_head: ctor,
                            type_name: symbol,
                            variant: symbol,
                            bindings: vec![],
                            span: span(),
                        },
                        body,
                    )],
                    span(),
                )
                .unwrap();
            assert_eq!(
                ValidatedRir::finish(editor, &context()).unwrap_err().reason,
                "instruction reference is outside the owner"
            );
        }

        let mut bad_symbol = RirEditor::new();
        bad_symbol.add_inst(Inst {
            data: InstData::StringConst {
                content: Spur::try_from_usize(77).unwrap(),
                anchor: RirStructuralAnchor::new(vec![RirStructuralPathSegment::StringLiteral(0)]),
            },
            span: span(),
        });
        assert_eq!(
            ValidatedRir::finish(bad_symbol, &context())
                .unwrap_err()
                .reason,
            "symbol is outside the canonical interner"
        );

        let mut bad_symbol_word = RirEditor::new();
        bad_symbol_word.rir.extra.push(77);
        bad_symbol_word.rir.add_inst(Inst {
            data: InstData::EnumDecl {
                is_pub: false,
                is_non_exhaustive: false,
                name: symbol,
                variants: RirEnumVariantsRange::from_parts(0, 1),
                payloads: RirEnumPayloadsRange::payload_fallback(),
            },
            span: span(),
        });
        assert_eq!(
            ValidatedRir::finish(bad_symbol_word, &context())
                .unwrap_err()
                .reason,
            "symbol is outside the canonical interner"
        );

        let mut overflow = RirEditor::new();
        let value = overflow.add_inst(Inst {
            data: InstData::UnitConst,
            span: span(),
        });
        overflow
            .add_match(value, &[(RirPattern::Wildcard(span()), value)], span())
            .unwrap();
        overflow.rir.extra[2] = u32::MAX;
        overflow.rir.extra[3] = 1;
        assert_eq!(
            ValidatedRir::finish(overflow, &context())
                .unwrap_err()
                .reason,
            "pattern span overflows u32"
        );
    }

    #[test]
    fn borrowed_payload_traversal_allocates_nothing() {
        let interner = ThreadedRodeo::new();
        let a = interner.get_or_intern("a");
        let mut rir = Rir::new();
        let type_a = install_named_types(&mut rir, &[a])[0];
        let refs = rir
            .add_block_insts(&[InstRef::from_raw(0), InstRef::from_raw(1)])
            .unwrap();
        let intrinsic = rir.add_intrinsic_args(&[InstRef::from_raw(0)]).unwrap();
        let internal = rir
            .add_internal_intrinsic_args(&[InstRef::from_raw(0)])
            .unwrap();
        let methods = rir.add_struct_methods(&[InstRef::from_raw(0)]).unwrap();
        let anon_methods = rir
            .add_anon_struct_methods(&[InstRef::from_raw(0)])
            .unwrap();
        let elements = rir.add_array_elements(&[InstRef::from_raw(0)]).unwrap();
        let calls = rir
            .add_call_args(&[RirCallArg {
                value: InstRef::from_raw(0),
                mode: RirArgMode::Normal,
            }])
            .unwrap();
        let directives = rir
            .add_directives(&[RirDirective {
                name: a,
                args: vec![a],
                span: span(),
            }])
            .unwrap();
        let arms = rir
            .add_match_arms(&[(RirPattern::Wildcard(span()), InstRef::from_raw(1))])
            .unwrap();
        let params = rir
            .add_params(&[RirParam {
                name: a,
                ty: type_a,
                mode: RirParamMode::Normal,
                is_comptime: false,
                span: span(),
            }])
            .unwrap();
        let inits = rir.add_field_inits(&[(a, InstRef::from_raw(0))]).unwrap();
        let fields = rir.add_struct_fields(&[(a, type_a)]).unwrap();
        let anon_fields = rir.add_anon_struct_fields(&[(a, type_a)]).unwrap();
        let variants = rir.add_enum_variants(&[a]).unwrap();
        let anon_variants = rir.add_anon_enum_variants(&[a]).unwrap();
        let payloads = rir.add_enum_payloads(&[vec![type_a]]).unwrap();
        let anon_payloads = rir.add_anon_enum_payloads(&[vec![type_a]]).unwrap();
        assert_eq!(
            allocations_during(|| {
                std::hint::black_box(rir.block_insts(&refs).values().count());
                std::hint::black_box(rir.intrinsic_args(&intrinsic).values().count());
                std::hint::black_box(rir.internal_intrinsic_args(&internal).values().count());
                std::hint::black_box(rir.struct_methods(&methods).values().count());
                std::hint::black_box(rir.anon_struct_methods(&anon_methods).values().count());
                std::hint::black_box(rir.array_elements(&elements).values().count());
                std::hint::black_box(rir.call_args(&calls).values().count());
                std::hint::black_box(rir.params(&params).values().count());
                std::hint::black_box(rir.directives(&directives).iter().count());
                std::hint::black_box(rir.match_arms(&arms).iter().count());
                std::hint::black_box(rir.field_inits(&inits).values().count());
                std::hint::black_box(rir.struct_fields(&fields).values().count());
                std::hint::black_box(rir.anon_struct_fields(&anon_fields).values().count());
                std::hint::black_box(rir.enum_variants(&variants).values().count());
                std::hint::black_box(rir.anon_enum_variants(&anon_variants).values().count());
                std::hint::black_box(rir.enum_payloads(&payloads, &variants).flatten().count());
                std::hint::black_box(
                    rir.anon_enum_payloads(&anon_payloads, &anon_variants)
                        .flatten()
                        .count(),
                );
            }),
            0
        );
    }

    #[test]
    fn every_symbol_bearing_schema_rejects_u32_max_before_views() {
        let interner = ThreadedRodeo::new();
        let a = interner.get_or_intern("a");
        let reference = InstRef::from_raw(0);
        let assert_rejected = |mut rir: Rir, corrupt: usize, data: InstData| {
            rir.extra[corrupt] = u32::MAX;
            rir.add_inst(Inst { data, span: span() });
            let error = rir.validate_payloads().unwrap_err();
            assert!(
                error.reason.contains("symbol") || error.reason.contains("schema"),
                "{error:?}"
            );
        };

        let mut rir = Rir::new();
        let type_a = install_named_types(&mut rir, &[a])[0];
        let params = rir
            .add_params(&[RirParam {
                name: a,
                ty: type_a,
                mode: RirParamMode::Normal,
                is_comptime: false,
                span: span(),
            }])
            .unwrap();
        assert_rejected(
            rir,
            0,
            InstData::FnDecl {
                directives: RirDirectivesRange::payload_fallback(),
                is_pub: false,
                is_unchecked: false,
                is_extern: false,
                is_c_export: false,
                name: a,
                params,
                return_type: type_a,
                body: reference,
                has_self: false,
                self_mode: RirParamMode::Normal,
                self_is_mut: false,
                returns_borrow: false,
                returns_inout: false,
            },
        );

        let mut rir = Rir::new();
        let directives = rir
            .add_directives(&[RirDirective {
                name: a,
                args: vec![a],
                span: span(),
            }])
            .unwrap();
        assert_rejected(
            rir,
            1,
            InstData::ConstDecl {
                directives,
                is_pub: false,
                name: a,
                ty: None,
                init: reference,
            },
        );

        let mut rir = Rir::new();
        let arms = rir
            .add_match_arms(&[(
                RirPattern::Path {
                    module: None,
                    ctor_head: None,
                    type_name: a,
                    variant: a,
                    bindings: vec![a],
                    span: span(),
                },
                reference,
            )])
            .unwrap();
        assert_rejected(
            rir,
            7,
            InstData::Match {
                scrutinee: reference,
                arms,
            },
        );

        macro_rules! fixed_symbol_case {
            ($builder:expr, $data:expr) => {{
                let mut rir = Rir::new();
                let _ = install_named_types(&mut rir, &[a]);
                let range = ($builder)(&mut rir);
                assert_rejected(rir, 0, ($data)(range));
            }};
        }
        fixed_symbol_case!(
            |rir: &mut Rir| rir.add_field_inits(&[(a, reference)]).unwrap(),
            |fields| InstData::StructInit {
                module: None,
                ctor_head: None,
                type_name: a,
                fields,
                shorthand_span: None,
            }
        );
        fixed_symbol_case!(
            |rir: &mut Rir| rir.add_struct_fields(&[(a, type_a)]).unwrap(),
            |fields| InstData::StructDecl {
                directives: RirDirectivesRange::payload_fallback(),
                is_pub: false,
                is_linear: false,
                name: a,
                fields,
                methods: RirStructMethodsRange::payload_fallback(),
            }
        );
        fixed_symbol_case!(
            |rir: &mut Rir| rir.add_anon_struct_fields(&[(a, type_a)]).unwrap(),
            |fields| InstData::AnonStructType {
                fields,
                methods: RirAnonStructMethodsRange::payload_fallback(),
                anchor: RirStructuralAnchor::new(vec![RirStructuralPathSegment::AnonymousType(0),]),
            }
        );
        fixed_symbol_case!(
            |rir: &mut Rir| rir.add_enum_variants(&[a]).unwrap(),
            |variants| InstData::EnumDecl {
                is_pub: false,
                is_non_exhaustive: false,
                name: a,
                variants,
                payloads: RirEnumPayloadsRange::payload_fallback(),
            }
        );
        fixed_symbol_case!(
            |rir: &mut Rir| rir.add_anon_enum_variants(&[a]).unwrap(),
            |variants| InstData::AnonEnumType {
                variants,
                payloads: RirAnonEnumPayloadsRange::payload_fallback(),
                anchor: RirStructuralAnchor::new(vec![RirStructuralPathSegment::AnonymousType(0),]),
            }
        );
    }

    #[test]
    fn every_payload_builder_records_per_family_allocation_and_storage_evidence() {
        let interner = ThreadedRodeo::new();
        let a = interner.get_or_intern("a");
        let b = interner.get_or_intern("b");
        let type_a = RirTypeSyntaxRef::from_u32(0);
        let type_b = RirTypeSyntaxRef::from_u32(1);
        let r0 = InstRef::from_raw(0);
        let r1 = InstRef::from_raw(1);
        let directives = [RirDirective {
            name: a,
            args: vec![b],
            span: span(),
        }];
        let params = [RirParam {
            name: a,
            ty: type_b,
            mode: RirParamMode::Borrow,
            is_comptime: true,
            span: span(),
        }];
        let calls = [RirCallArg {
            value: r0,
            mode: RirArgMode::Inout,
        }];
        let enum_payloads = [vec![type_a], vec![]];
        let anon_payloads = [vec![type_b]];
        #[derive(Debug)]
        struct Evidence {
            family: &'static str,
            allocation_calls: usize,
            allocated_bytes: usize,
            logical_bytes: usize,
            retained_capacity_bytes: usize,
            elements: usize,
            build_ns: u128,
            build_elements_per_second: f64,
            traversal_ns: u128,
            elements_per_second: f64,
            peak_staging_bytes: usize,
        }
        macro_rules! evidence {
            ($family:expr, $build:expr, $consume:expr) => {{
                let mut rir = Rir::new();
                let installed = install_named_types(&mut rir, &[a, b]);
                assert_eq!(installed, [type_a, type_b]);
                let mut range = None;
                let build_started = std::time::Instant::now();
                let (allocation_calls, allocated_bytes) = allocation_evidence(|| {
                    range = Some(($build)(&mut rir));
                });
                let build_ns = build_started.elapsed().as_nanos();
                let range = range.unwrap();
                const TRAVERSALS: usize = 20_000;
                let started = std::time::Instant::now();
                let mut consumed = 0usize;
                for _ in 0..TRAVERSALS {
                    consumed += std::hint::black_box(($consume)(&rir, &range));
                }
                let traversal_ns = started.elapsed().as_nanos();
                let elements = consumed / TRAVERSALS;
                let logical_bytes = rir.extra.len() * std::mem::size_of::<u32>();
                let peak_staging_bytes = match $family {
                    RirIntrinsicArgsRange::FAMILY
                    | RirInternalIntrinsicArgsRange::FAMILY
                    | RirBlockInstsRange::FAMILY
                    | RirStructMethodsRange::FAMILY
                    | RirAnonStructMethodsRange::FAMILY
                    | RirArrayElemsRange::FAMILY
                    | RirCallArgsRange::FAMILY
                    | RirParamsRange::FAMILY
                    | RirMatchArmsRange::FAMILY
                    | RirFieldInitsRange::FAMILY
                    | RirStructFieldsRange::FAMILY
                    | RirAnonStructFieldsRange::FAMILY => 0,
                    _ => logical_bytes,
                };
                Evidence {
                    family: $family,
                    allocation_calls,
                    allocated_bytes,
                    logical_bytes,
                    retained_capacity_bytes: rir.extra.capacity() * std::mem::size_of::<u32>(),
                    elements,
                    build_ns,
                    build_elements_per_second: elements as f64 / (build_ns as f64 / 1e9),
                    traversal_ns,
                    elements_per_second: consumed as f64 / (traversal_ns as f64 / 1e9),
                    peak_staging_bytes,
                }
            }};
        }
        let evidence = [
            evidence!(
                RirIntrinsicArgsRange::FAMILY,
                |rir: &mut Rir| { rir.add_intrinsic_args(&[r0, r1]).unwrap() },
                |rir: &Rir, range| rir.intrinsic_args(range).len()
            ),
            evidence!(
                RirInternalIntrinsicArgsRange::FAMILY,
                |rir: &mut Rir| { rir.add_internal_intrinsic_args(&[r0]).unwrap() },
                |rir: &Rir, range| rir.internal_intrinsic_args(range).len()
            ),
            evidence!(
                RirBlockInstsRange::FAMILY,
                |rir: &mut Rir| { rir.add_block_insts(&[r0, r1]).unwrap() },
                |rir: &Rir, range| rir.block_insts(range).len()
            ),
            evidence!(
                RirStructMethodsRange::FAMILY,
                |rir: &mut Rir| { rir.add_struct_methods(&[r0]).unwrap() },
                |rir: &Rir, range| rir.struct_methods(range).len()
            ),
            evidence!(
                RirAnonStructMethodsRange::FAMILY,
                |rir: &mut Rir| { rir.add_anon_struct_methods(&[r1]).unwrap() },
                |rir: &Rir, range| rir.anon_struct_methods(range).len()
            ),
            evidence!(
                RirArrayElemsRange::FAMILY,
                |rir: &mut Rir| { rir.add_array_elements(&[r0, r1]).unwrap() },
                |rir: &Rir, range| rir.array_elements(range).len()
            ),
            evidence!(
                RirCallArgsRange::FAMILY,
                |rir: &mut Rir| { rir.add_call_args(&calls).unwrap() },
                |rir: &Rir, range| rir.call_args(range).len()
            ),
            evidence!(
                RirParamsRange::FAMILY,
                |rir: &mut Rir| { rir.add_params(&params).unwrap() },
                |rir: &Rir, range| rir.params(range).len()
            ),
            evidence!(
                RirMatchArmsRange::FAMILY,
                |rir: &mut Rir| {
                    rir.add_match_arms(&[(RirPattern::Wildcard(span()), r0)])
                        .unwrap()
                },
                |rir: &Rir, range| rir.match_arms(range).len()
            ),
            evidence!(
                RirFieldInitsRange::FAMILY,
                |rir: &mut Rir| { rir.add_field_inits(&[(a, r0)]).unwrap() },
                |rir: &Rir, range| rir.field_inits(range).len()
            ),
            evidence!(
                RirStructFieldsRange::FAMILY,
                |rir: &mut Rir| { rir.add_struct_fields(&[(a, type_b)]).unwrap() },
                |rir: &Rir, range| rir.struct_fields(range).len()
            ),
            evidence!(
                RirAnonStructFieldsRange::FAMILY,
                |rir: &mut Rir| { rir.add_anon_struct_fields(&[(a, type_b)]).unwrap() },
                |rir: &Rir, range| rir.anon_struct_fields(range).len()
            ),
            evidence!(
                RirDirectivesRange::FAMILY,
                |rir: &mut Rir| { rir.add_directives(&directives).unwrap() },
                |rir: &Rir, range| rir.directives(range).len()
            ),
            evidence!(
                RirEnumVariantsRange::FAMILY,
                |rir: &mut Rir| { rir.add_enum_variants(&[a, b]).unwrap() },
                |rir: &Rir, range| rir.enum_variants(range).len()
            ),
            evidence!(
                RirAnonEnumVariantsRange::FAMILY,
                |rir: &mut Rir| { rir.add_anon_enum_variants(&[b]).unwrap() },
                |rir: &Rir, range| rir.anon_enum_variants(range).len()
            ),
            evidence!(
                RirEnumPayloadsRange::FAMILY,
                |rir: &mut Rir| { rir.add_enum_payloads(&enum_payloads).unwrap() },
                |rir: &Rir, range| rir
                    .enum_payloads(range, &RirEnumVariantsRange::from_parts(0, 2))
                    .map(|v| v.len())
                    .sum::<usize>()
            ),
            evidence!(
                RirAnonEnumPayloadsRange::FAMILY,
                |rir: &mut Rir| { rir.add_anon_enum_payloads(&anon_payloads).unwrap() },
                |rir: &Rir, range| rir
                    .anon_enum_payloads(range, &RirAnonEnumVariantsRange::from_parts(0, 1))
                    .map(|v| v.len())
                    .sum::<usize>()
            ),
        ];
        assert_eq!(evidence.len(), 17);
        for item in &evidence {
            let minimum_allocations = if item.peak_staging_bytes == 0 { 1 } else { 2 };
            assert!(
                item.allocation_calls >= minimum_allocations,
                "{}: {item:?}",
                item.family
            );
            assert!(item.logical_bytes > 0, "{}: {item:?}", item.family);
            assert!(
                item.retained_capacity_bytes >= item.logical_bytes,
                "{}: {item:?}",
                item.family
            );
            assert!(
                item.allocated_bytes >= item.retained_capacity_bytes,
                "{}: {item:?}",
                item.family
            );
            assert!(item.elements > 0 && item.traversal_ns > 0);
            assert!(item.elements_per_second.is_finite());
            assert!(item.build_ns > 0 && item.build_elements_per_second.is_finite());
            assert!(item.peak_staging_bytes == 0 || item.peak_staging_bytes == item.logical_bytes);
            eprintln!(
                "RUE843_FAMILY\tphase=RIR\tfamily={}\telements={}\tbuild_ns={}\tbuild_elements_per_second={}\tbuild_allocations={}\tbuild_allocated_bytes={}\ttraversal_ns={}\ttraversal_elements_per_second={}\ttraversal_allocations=0\tlogical_bytes={}\tcapacity_bytes={}\ttotal_bytes={}\tenvelopes={}\tpeak_staging_bytes={}",
                item.family,
                item.elements,
                item.build_ns,
                item.build_elements_per_second,
                item.allocation_calls,
                item.allocated_bytes,
                item.traversal_ns,
                item.elements_per_second,
                item.logical_bytes,
                item.retained_capacity_bytes,
                item.logical_bytes + item.retained_capacity_bytes,
                usize::from(matches!(
                    item.family,
                    "match arms"
                        | "directives"
                        | "enum variant payloads"
                        | "anonymous enum variant payloads"
                )),
                item.peak_staging_bytes,
            );
        }
        eprintln!("RUE-843 RIR family evidence: {evidence:#?}");
        std::hint::black_box(evidence);
    }
}
