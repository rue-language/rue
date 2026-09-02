//! Compact payload schemas, storage adapters, and zero-allocation borrowed views.

use super::*;

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

#[path = "schema.rs"]
mod schema;
#[path = "spans.rs"]
mod spans;

pub use schema::*;
pub use spans::*;

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
    /// Construct a view over words that have not passed publication
    /// validation. Decoding every record here keeps this boundary fail-closed:
    /// callers cannot expose a slice whose decoder rejects one of its records.
    fn new_unvalidated(words: &'a [u32], width: usize, decode: fn(&[u32]) -> T) -> Self {
        assert!(width != 0 && words.len().is_multiple_of(width));
        for record in words.chunks_exact(width) {
            decode(record);
        }
        Self::new_validated(words, width, decode)
    }

    /// Construct a view after the owning RIR has passed publication
    /// validation. Only fixed-width range invariants are checked here; records
    /// remain borrowed and are decoded on iteration or indexed access.
    fn new_validated(words: &'a [u32], width: usize, decode: fn(&[u32]) -> T) -> Self {
        assert!(width != 0 && words.len().is_multiple_of(width));
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
        let start = index.checked_mul(self.width)?;
        let end = start.checked_add(self.width)?;
        Some((self.decode)(self.words.get(start..end)?))
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
    validated: bool,
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
                    bindings: if validated {
                        RirSlice::new_validated(
                            &words[binding_start..binding_start + count],
                            SYMBOL_SCHEMA.width,
                            |record| validated_symbol_word(record[0]),
                        )
                    } else {
                        RirSlice::new_unvalidated(
                            &words[binding_start..binding_start + count],
                            SYMBOL_SCHEMA.width,
                            |record| validated_symbol_word(record[0]),
                        )
                    },
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
    validated: bool,
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
            args: if validated {
                RirSlice::new_validated(&words[args_start..end], SYMBOL_SCHEMA.width, |record| {
                    validated_symbol_word(record[0])
                })
            } else {
                RirSlice::new_unvalidated(&words[args_start..end], SYMBOL_SCHEMA.width, |record| {
                    validated_symbol_word(record[0])
                })
            },
            span: embedded_span(words, position)?,
        },
        extent,
    ))
}

/// Stored representation of directive in the extra array.
/// Layout: [name: u32, span_start: u32, span_len: u32, args_len: u32, args...]
/// Variable size due to args.

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
}

impl Rir {
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
        self.deferred_structural_anchors.push(None);
        InstRef::from_raw(index)
    }

    pub(crate) fn set_deferred_structural_anchor(
        &mut self,
        instruction: InstRef,
        anchor: RirDeferredStructuralAnchor,
    ) {
        self.deferred_structural_anchors[instruction.as_u32() as usize] = Some(anchor);
    }

    pub(crate) fn deferred_structural_anchor(
        &self,
        instruction: InstRef,
    ) -> Option<&RirDeferredStructuralAnchor> {
        self.deferred_structural_anchors
            .get(instruction.as_u32() as usize)
            .and_then(Option::as_ref)
    }

    /// Materialize the structural anchor for a semantically proven named
    /// string-constant use. Explicit public anchors take precedence; ordinary
    /// source reads consult the producer-private deferred side table.
    pub fn materialize_const_use_anchor(
        &self,
        instruction: InstRef,
    ) -> Option<RirStructuralAnchor> {
        match &self.get(instruction).data {
            InstData::VarRef {
                anchor: Some(anchor),
                ..
            } => Some(anchor.clone()),
            InstData::VarRef { anchor: None, .. } => self
                .deferred_structural_anchor(instruction)
                .map(RirDeferredStructuralAnchor::materialize),
            _ => None,
        }
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
        &self.instructions[inst_ref.as_u32() as usize]
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
}

impl Rir {
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

    fn fixed_view<'a, T: 'a>(
        &self,
        words: &'a [u32],
        width: usize,
        decode: fn(&[u32]) -> T,
    ) -> RirSlice<'a, T> {
        if self.views_validated {
            RirSlice::new_validated(words, width, decode)
        } else {
            RirSlice::new_unvalidated(words, width, decode)
        }
    }

    fn ref_view<R>(
        &self,
        range: &R,
        parts: impl FnOnce(&R) -> (u32, u32, &'static str),
    ) -> RirSlice<'_, InstRef> {
        self.fixed_view(
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
        self.fixed_view(data, CALL_ARG_SCHEMA.width, |chunk| RirCallArg {
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
        self.fixed_view(data, PARAM_SCHEMA.width, |chunk| RirParam {
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
                validated: self.views_validated,
            };
        }
        let view = RirMatchArms {
            extra: words,
            start: 1,
            len: words[0] as usize,
            validated: self.views_validated,
        };
        if !view.validated {
            let mut records = view.iter();
            while records.next().is_some() {}
        }
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
        self.fixed_view(data, FIELD_INIT_SCHEMA.width, |chunk| {
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
        self.fixed_view(data, FIELD_DECL_SCHEMA.width, |chunk| {
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
                validated: self.views_validated,
            };
        }
        let view = RirDirectives {
            extra: words,
            start: 1,
            len: words[0] as usize,
            validated: self.views_validated,
        };
        if !view.validated {
            let mut records = view.iter();
            while records.next().is_some() {}
        }
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
        self.fixed_view(words, SYMBOL_SCHEMA.width, |record| {
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
            validated: self.views_validated,
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
    validated: bool,
}

impl<'a> Iterator for RirEnumPayloads<'a> {
    type Item = RirTypeSyntaxRefs<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        if self.words.is_empty() {
            return Some(if self.validated {
                RirSlice::new_validated(&[], SYMBOL_SCHEMA.width, |_| unreachable!())
            } else {
                RirSlice::new_unvalidated(&[], SYMBOL_SCHEMA.width, |_| unreachable!())
            });
        }
        let (start, end) = enum_payload_record(self.words, self.position)
            .expect("validated enum payload descriptor");
        self.position = end;
        Some(if self.validated {
            RirSlice::new_validated(&self.words[start..end], SYMBOL_SCHEMA.width, |record| {
                RirTypeSyntaxRef::from_u32(record[0])
            })
        } else {
            RirSlice::new_unvalidated(&self.words[start..end], SYMBOL_SCHEMA.width, |record| {
                RirTypeSyntaxRef::from_u32(record[0])
            })
        })
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
    validated: bool,
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
            validated: self.validated,
        }
    }

    pub fn get(&self, index: usize) -> Option<(RirPatternView<'a>, InstRef)> {
        let mut records = self.iter();
        for _ in 0..index {
            records.next()?;
        }
        records.next()
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
    validated: bool,
}

impl<'a> Iterator for RirMatchArmsIter<'a> {
    type Item = (RirPatternView<'a>, InstRef);

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let (pattern, body, extent) =
            match decode_match_record(self.extra, self.pos, self.validated) {
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
    validated: bool,
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
            validated: self.validated,
        }
    }

    pub fn get(&self, index: usize) -> Option<RirDirectiveView<'a>> {
        let mut records = self.iter();
        for _ in 0..index {
            records.next()?;
        }
        records.next()
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
    validated: bool,
}

impl<'a> Iterator for RirDirectivesIter<'a> {
    type Item = RirDirectiveView<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let (directive, extent) =
            match decode_directive_record(self.extra, self.pos, self.validated) {
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
