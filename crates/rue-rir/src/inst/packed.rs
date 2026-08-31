//! Canonical compact transport for validated RIR.
//!
//! This is an exact representation codec, not a lowering path. It deliberately
//! lives below `ValidatedRir` so every consumer shares one schema for payloads,
//! anchors, symbols, references, and spans. Decoding appends directly into the
//! caller's editor and rolls the complete append back on any failure.

use std::collections::{HashMap, hash_map::RandomState};
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::sync::Arc;

use lasso::{Key, Spur, ThreadedRodeo};
use rue_span::{FileId, Span};

use crate::{RirTypeSyntaxNode, RirTypeSyntaxRange, RirTypeSyntaxSymbol};

use super::*;

const MAGIC: &[u8; 4] = b"RIRP";
const VERSION: u8 = 4;
const HEADER_LEN: usize = 64;

/// One fallible source intrinsic whose result requires a trusted `Option`
/// payload. Stable bit assignments are part of the packed-RIR wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RirFallibleIntrinsic {
    ParseI32,
    ParseI64,
    ParseU32,
    ParseU64,
    ReadLine,
}

impl RirFallibleIntrinsic {
    const ALL: [Self; 5] = [
        Self::ParseI32,
        Self::ParseI64,
        Self::ParseU32,
        Self::ParseU64,
        Self::ReadLine,
    ];

    const fn bit(self) -> u8 {
        match self {
            Self::ParseI32 => 1 << 0,
            Self::ParseI64 => 1 << 1,
            Self::ParseU32 => 1 << 2,
            Self::ParseU64 => 1 << 3,
            Self::ReadLine => 1 << 4,
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "parse_i32" => Self::ParseI32,
            "parse_i64" => Self::ParseI64,
            "parse_u32" => Self::ParseU32,
            "parse_u64" => Self::ParseU64,
            "read_line" => Self::ReadLine,
            _ => return None,
        })
    }
}

/// Canonical five-bit set of fallible source intrinsics present in one packed
/// candidate. It is derived while the encoder visits typed `Intrinsic` nodes;
/// source text, comments, and string literals never participate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RirFallibleIntrinsicSet(u8);

impl RirFallibleIntrinsicSet {
    const VALID_BITS: u8 = 0b1_1111;

    fn insert(&mut self, intrinsic: RirFallibleIntrinsic) {
        self.0 |= intrinsic.bit();
    }

    pub fn contains(self, intrinsic: RirFallibleIntrinsic) -> bool {
        self.0 & intrinsic.bit() != 0
    }

    pub fn iter(self) -> impl Iterator<Item = RirFallibleIntrinsic> {
        RirFallibleIntrinsic::ALL
            .into_iter()
            .filter(move |intrinsic| self.contains(*intrinsic))
    }
}

/// Candidate identity carried in the same allocation as its packed RIR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackedRirMetadata {
    pub declaration: InstRef,
    pub method_owner: Option<PackedRirMethodOwner>,
}

/// Exact request-local authority for projecting one declaration-relative
/// packed candidate into its current source revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackedRirProjection {
    pub symbol_count: usize,
    pub file_id: FileId,
    pub declaration_start: u32,
    pub source_length: u32,
}

/// RIR-level method-owner shell metadata. Compiler-specific identities stay
/// outside this crate; these are precisely the scalars required to rebuild the
/// canonical shell through `RirEditor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackedRirMethodOwner {
    pub declaration: InstRef,
    pub name: Spur,
    pub is_public: bool,
    pub is_linear: bool,
}

/// Destination-local metadata returned by an atomic packed append.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackedRirAppendMetadata {
    pub declaration: InstRef,
    pub method_owner: Option<PackedRirMethodOwner>,
}

/// The stores and candidate identities created by an atomic packed append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedRirAppend {
    pub range: RirAppendRange,
    pub metadata: PackedRirAppendMetadata,
}

/// Exact, versioned byte representation of one validated RIR owner.
///
/// Construction is private to [`ValidatedRir::try_pack_candidate`], ensuring all values
/// have passed the ordinary RIR validator before becoming durable bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedValidatedRir(Arc<[u8]>);

impl PackedValidatedRir {
    /// Canonical bytes. Equality of this slice is exact semantic equality for
    /// the transported RIR because every integer has one minimal encoding.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Number of candidate-local instructions in the validated envelope.
    pub fn instruction_count(&self) -> usize {
        Header::parse(&self.0)
            .expect("privately constructed packed RIR is valid")
            .instructions as usize
    }

    /// Number of dense candidate-local symbol spellings in the envelope.
    ///
    /// This reads only the validated fixed header. Consumers that need both
    /// the count and the spellings should create one [`PackedRirSymbols`]
    /// iterator and use its exact size rather than rescanning the section.
    pub fn symbol_count(&self) -> usize {
        Header::parse(&self.0)
            .expect("privately constructed packed RIR is valid")
            .symbols as usize
    }

    /// Complete dense spelling table transported with the owner, including
    /// empty and unreferenced ordinals.
    pub fn symbols(&self) -> PackedRirSymbols<'_> {
        PackedRirSymbols::new(&self.0).expect("privately constructed packed RIR is valid")
    }

    /// Typed fallible-intrinsic set derived by the canonical packing traversal.
    pub fn fallible_intrinsics(&self) -> RirFallibleIntrinsicSet {
        Header::parse(&self.0)
            .expect("privately constructed packed RIR is valid")
            .fallible_intrinsics
    }

    /// Append directly into `destination`. Candidate-local instruction
    /// references use the checked affine destination prefix; symbol ordinals
    /// and declaration-relative spans are remapped by the caller.
    ///
    /// Checkpoints occur at instruction and variable-payload element
    /// granularity. Any decode, callback, capacity, or cancellation failure
    /// restores both destination stores and the capacity latch exactly.
    pub fn try_append_remapped<E>(
        &self,
        destination: &mut RirEditor,
        checkpoint: impl FnMut() -> Result<(), E>,
        remap_symbol: impl FnMut(u32) -> Result<Spur, E>,
        remap_span: impl FnMut(RirSpanSlot, (u32, u32)) -> Result<Span, E>,
    ) -> Result<PackedRirAppend, PackedRirAppendError<E>> {
        self.try_append_remapped_internal(
            destination,
            None,
            true,
            checkpoint,
            remap_symbol,
            remap_span,
        )
    }

    /// Append a methodless struct-shell candidate while installing the
    /// composer's already-remapped method roots on its declaration.
    pub fn try_append_remapped_with_root_methods<E>(
        &self,
        destination: &mut RirEditor,
        methods: &[InstRef],
        checkpoint: impl FnMut() -> Result<(), E>,
        remap_symbol: impl FnMut(u32) -> Result<Spur, E>,
        remap_span: impl FnMut(RirSpanSlot, (u32, u32)) -> Result<Span, E>,
    ) -> Result<PackedRirAppend, PackedRirAppendError<E>> {
        self.try_append_remapped_internal(
            destination,
            Some(methods),
            true,
            checkpoint,
            remap_symbol,
            remap_span,
        )
    }

    /// Decode one candidate into a fresh validated owner without repeating the
    /// generic post-construction RIR scan.
    ///
    /// The packed decoder already checks the complete typed payload schema,
    /// every instruction reference, every symbol ordinal, and every span slot
    /// while constructing the editor. This boundary also checks the complete
    /// dense symbol count and projects every declaration-relative span through
    /// one current source authority, so the resulting [`ValidatedRir`] has the
    /// same guarantees as [`ValidatedRir::finish`] without walking the finished
    /// arena a second time.
    pub fn try_decode_validated<E>(
        &self,
        projection: PackedRirProjection,
        checkpoint: impl FnMut() -> Result<(), E>,
    ) -> Result<(ValidatedRir, PackedRirAppendMetadata), PackedRirAppendError<E>> {
        self.try_decode_validated_internal(projection, false, checkpoint, Self::dense_symbol)
    }

    /// Decode one candidate and append its analysis-only named-method owner
    /// shell before publishing the validated owner.
    pub fn try_decode_validated_with_method_owner<E>(
        &self,
        projection: PackedRirProjection,
        checkpoint: impl FnMut() -> Result<(), E>,
    ) -> Result<(ValidatedRir, PackedRirAppendMetadata), PackedRirAppendError<E>> {
        self.try_decode_validated_internal(projection, true, checkpoint, Self::dense_symbol)
    }

    /// Decode one candidate while translating its dense symbol ordinals into
    /// another interner's handles.
    ///
    /// The packed envelope always speaks the body-private dense encoding space
    /// (ADR-0076 §1): a symbol *is* its ordinal in this owner's dense spelling
    /// section. A caller whose analysis state uses the revision-shared equality
    /// space supplies the body's dense remap here, so the decoded RIR names
    /// symbols in the same space its analysis does. `remap_symbol` returning
    /// `None` is an out-of-range ordinal and fails the decode.
    pub fn try_decode_validated_remapped<E>(
        &self,
        projection: PackedRirProjection,
        include_method_owner: bool,
        checkpoint: impl FnMut() -> Result<(), E>,
        remap_symbol: impl FnMut(u32) -> Option<Spur>,
    ) -> Result<(ValidatedRir, PackedRirAppendMetadata), PackedRirAppendError<E>> {
        self.try_decode_validated_internal(
            projection,
            include_method_owner,
            checkpoint,
            remap_symbol,
        )
    }

    /// The identity remap: a dense ordinal is its own symbol handle. This is
    /// the body-private dense encoding space read back as itself, used by the
    /// declaration-time candidate path whose spelling table is a `&[&str]`
    /// indexed by the same ordinals.
    fn dense_symbol(ordinal: u32) -> Option<Spur> {
        Spur::try_from_usize(ordinal as usize)
    }

    fn try_decode_validated_internal<E>(
        &self,
        projection: PackedRirProjection,
        include_method_owner: bool,
        mut checkpoint: impl FnMut() -> Result<(), E>,
        mut remap_symbol: impl FnMut(u32) -> Option<Spur>,
    ) -> Result<(ValidatedRir, PackedRirAppendMetadata), PackedRirAppendError<E>> {
        enum Checked<E> {
            Callback(E),
            Invalid(PackedRirDecodeError),
        }

        fn map_checked<E>(error: PackedRirAppendError<Checked<E>>) -> PackedRirAppendError<E> {
            match error {
                PackedRirAppendError::Decode(error) => PackedRirAppendError::Decode(error),
                PackedRirAppendError::Build(error) => PackedRirAppendError::Build(error),
                PackedRirAppendError::Checkpoint(Checked::Callback(error)) => {
                    PackedRirAppendError::Checkpoint(error)
                }
                PackedRirAppendError::SymbolRemap(Checked::Callback(error)) => {
                    PackedRirAppendError::SymbolRemap(error)
                }
                PackedRirAppendError::SpanRemap {
                    slot,
                    error: Checked::Callback(error),
                } => PackedRirAppendError::SpanRemap { slot, error },
                PackedRirAppendError::Checkpoint(Checked::Invalid(error))
                | PackedRirAppendError::SymbolRemap(Checked::Invalid(error))
                | PackedRirAppendError::SpanRemap {
                    error: Checked::Invalid(error),
                    ..
                } => PackedRirAppendError::Decode(error),
            }
        }

        let header = Header::parse(&self.0).map_err(PackedRirAppendError::Decode)?;
        if header.symbols as usize != projection.symbol_count {
            return Err(PackedRirAppendError::Decode(
                PackedRirDecodeError::CountOutOfBounds {
                    family: "projected symbol universe",
                },
            ));
        }
        let mut editor = RirEditor::new();
        let appended = self
            .try_append_remapped_internal(
                &mut editor,
                None,
                // This type has no public unchecked constructor: packing
                // validated the immutable dense-symbol section once. The
                // generic append boundary retains full corruption checking,
                // while a fresh direct decode need not allocate and populate
                // the duplicate-detection map for every body request.
                false,
                || checkpoint().map_err(Checked::Callback),
                |ordinal| {
                    remap_symbol(ordinal).ok_or(Checked::Invalid(
                        PackedRirDecodeError::CountOutOfBounds {
                            family: "projected symbol ordinal",
                        },
                    ))
                },
                |_slot, (relative_start, relative_end)| {
                    let start = projection
                        .declaration_start
                        .checked_add(relative_start)
                        .ok_or(Checked::Invalid(PackedRirDecodeError::CountOutOfBounds {
                            family: "projected span start",
                        }))?;
                    let end = projection
                        .declaration_start
                        .checked_add(relative_end)
                        .ok_or(Checked::Invalid(PackedRirDecodeError::CountOutOfBounds {
                            family: "projected span end",
                        }))?;
                    if start > end || end > projection.source_length {
                        return Err(Checked::Invalid(PackedRirDecodeError::CountOutOfBounds {
                            family: "projected span range",
                        }));
                    }
                    Ok(Span::with_file(projection.file_id, start, end))
                },
            )
            .map_err(map_checked)?;
        if include_method_owner && let Some(owner) = appended.metadata.method_owner {
            let span = editor.get(owner.declaration).span;
            editor
                .add_struct_decl(
                    &[],
                    owner.is_public,
                    owner.is_linear,
                    owner.name,
                    &[],
                    &[owner.declaration],
                    span,
                )
                .map_err(PackedRirAppendError::Build)?;
        }
        let rir = editor.into_unvalidated();
        // The packed envelope and append traversal have already checked the
        // complete payload graph. Publication still goes through validation's
        // sole `ValidatedRir` construction boundary.
        Ok((ValidatedRir::from_prevalidated(rir), appended.metadata))
    }

    fn try_append_remapped_internal<E>(
        &self,
        destination: &mut RirEditor,
        root_methods: Option<&[InstRef]>,
        revalidate_symbols: bool,
        checkpoint: impl FnMut() -> Result<(), E>,
        remap_symbol: impl FnMut(u32) -> Result<Spur, E>,
        remap_span: impl FnMut(RirSpanSlot, (u32, u32)) -> Result<Span, E>,
    ) -> Result<PackedRirAppend, PackedRirAppendError<E>> {
        let instruction_len = destination.rir.instructions.len();
        let extra_len = destination.rir.extra.len();
        let type_snapshot = destination.type_syntax.snapshot();
        let capacity_latch = destination.rir.instruction_limit_exceeded;
        let result = Decoder::new(
            &self.0,
            destination,
            root_methods,
            revalidate_symbols,
            checkpoint,
            remap_symbol,
            remap_span,
        )
        .decode();
        if result.is_err() {
            destination.rir.instructions.truncate(instruction_len);
            destination.rir.extra.truncate(extra_len);
            destination.type_syntax.rollback(type_snapshot);
            destination.rir.instruction_limit_exceeded = capacity_latch;
        }
        result
    }

    /// Exact bytes retained by the packed envelope's single Arc pointee.
    pub fn retained_allocation_charge(&self) -> u64 {
        self.0.len() as u64
    }
}

/// Failure while producing canonical packed bytes.
#[derive(Debug)]
pub enum PackedRirEncodeError<E> {
    Checkpoint(E),
    CapacityFailure,
    ResourceLimit,
    Validation(RirPayloadError),
    SpanProjection {
        slot: RirSpanSlot,
        error: E,
    },
    InvalidProjectedSpan {
        slot: RirSpanSlot,
        start: u32,
        end: u32,
    },
    ForwardReference {
        instruction: u32,
        reference: u32,
    },
    InvalidMetadata,
}

impl<E: fmt::Display> fmt::Display for PackedRirEncodeError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Checkpoint(error) => write!(formatter, "packed RIR encoding canceled: {error}"),
            Self::CapacityFailure => formatter.write_str("could not allocate packed RIR bytes"),
            Self::ResourceLimit => formatter.write_str("packed RIR exceeds its u32 count limit"),
            Self::Validation(error) => write!(formatter, "packed RIR validation failed: {error:?}"),
            Self::SpanProjection { slot, error } => {
                write!(
                    formatter,
                    "packed RIR span projection failed at {slot:?}: {error}"
                )
            }
            Self::InvalidProjectedSpan { slot, start, end } => {
                write!(
                    formatter,
                    "packed RIR span projection at {slot:?} is inverted: {start}..{end}"
                )
            }
            Self::ForwardReference {
                instruction,
                reference,
            } => {
                write!(
                    formatter,
                    "candidate instruction %{instruction} has non-postorder reference %{reference}"
                )
            }
            Self::InvalidMetadata => {
                formatter.write_str("packed RIR candidate metadata is invalid")
            }
        }
    }
}

/// Structural corruption in canonical packed bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackedRirDecodeError {
    InvalidMagic,
    UnsupportedVersion(u8),
    Truncated,
    NonMinimalVarint,
    VarintOverflow,
    InvalidOpcode(u8),
    InvalidTag { family: &'static str, tag: u8 },
    ReferenceOutOfBounds { reference: u32, instructions: u32 },
    ForwardReference { instruction: u32, reference: u32 },
    SymbolOutOfBounds { symbol: u32, symbols: u32 },
    TypeReferenceOutOfBounds { reference: u32, types: u32 },
    ForwardTypeReference { owner: u32, reference: u32 },
    CountOutOfBounds { family: &'static str },
    InvalidUtf8Symbol { symbol: u32 },
    DuplicateSymbol { first: u32, duplicate: u32 },
    InvalidBasisSpan { start: u32, end: u32 },
    Payload(RirPayloadError),
    DestinationInstructionMismatch,
    TrailingBytes,
}

impl fmt::Display for PackedRirDecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid packed RIR: {self:?}")
    }
}

impl std::error::Error for PackedRirDecodeError {}

/// Failure while atomically appending packed RIR.
#[derive(Debug)]
pub enum PackedRirAppendError<E> {
    Decode(PackedRirDecodeError),
    Checkpoint(E),
    SymbolRemap(E),
    SpanRemap { slot: RirSpanSlot, error: E },
    Build(RirPayloadBuildError),
}

impl<E> From<PackedRirDecodeError> for PackedRirAppendError<E> {
    fn from(error: PackedRirDecodeError) -> Self {
        Self::Decode(error)
    }
}

impl<E> From<RirPayloadBuildError> for PackedRirAppendError<E> {
    fn from(error: RirPayloadBuildError) -> Self {
        Self::Build(error)
    }
}

impl ValidatedRir {
    /// Encode one validated candidate directly, projecting its absolute spans
    /// into the declaration-relative diagnostic basis stored in the same
    /// allocation. Absolute file identities and coordinates never enter the
    /// semantic byte section.
    pub fn try_pack_candidate<E>(
        &self,
        symbols: &ThreadedRodeo,
        metadata: PackedRirMetadata,
        checkpoint: impl FnMut() -> Result<(), E>,
        project_span: impl FnMut(RirSpanSlot, Span) -> Result<(u32, u32), E>,
    ) -> Result<PackedValidatedRir, PackedRirEncodeError<E>> {
        Encoder::new(checkpoint, project_span).encode(self, symbols, metadata)
    }
}

struct Encoder<E, C, P> {
    bytes: Vec<u8>,
    types: Vec<u8>,
    basis: Vec<u8>,
    span_count: u32,
    checkpoint: C,
    project_span: P,
    current_instruction: u32,
    symbol_count: usize,
    type_count: usize,
    current_type: u32,
    fallible_intrinsics: RirFallibleIntrinsicSet,
    marker: std::marker::PhantomData<fn() -> E>,
}

impl<E, C: FnMut() -> Result<(), E>, P: FnMut(RirSpanSlot, Span) -> Result<(u32, u32), E>>
    Encoder<E, C, P>
{
    fn new(checkpoint: C, project_span: P) -> Self {
        Self {
            bytes: Vec::new(),
            types: Vec::new(),
            basis: Vec::new(),
            span_count: 0,
            checkpoint,
            project_span,
            current_instruction: 0,
            symbol_count: 0,
            type_count: 0,
            current_type: 0,
            fallible_intrinsics: RirFallibleIntrinsicSet::default(),
            marker: std::marker::PhantomData,
        }
    }

    fn encode(
        mut self,
        rir: &ValidatedRir,
        symbols: &ThreadedRodeo,
        metadata: PackedRirMetadata,
    ) -> Result<PackedValidatedRir, PackedRirEncodeError<E>> {
        let instruction_count =
            u32::try_from(rir.len()).map_err(|_| PackedRirEncodeError::ResourceLimit)?;
        let symbol_count =
            u32::try_from(symbols.len()).map_err(|_| PackedRirEncodeError::ResourceLimit)?;
        self.symbol_count = symbols.len();
        self.type_count = rir.type_syntax().nodes().len();
        let type_count =
            u32::try_from(self.type_count).map_err(|_| PackedRirEncodeError::ResourceLimit)?;
        // `ValidatedRir` already proved the arena's ranges, postorder child
        // references, and symbol bounds. The typed encoder still checks every
        // symbol/ref as it writes it, so repeating the complete arena walk here
        // added candidate work without strengthening this boundary.
        self.type_arena(rir.type_syntax())?;

        for (instruction, value) in rir.iter() {
            self.check()?;
            self.current_instruction = instruction.as_u32();
            self.instruction(rir, symbols, instruction, value)?;
        }

        let mut symbol_bytes = Vec::new();
        let spelling_bytes = (0..symbols.len()).try_fold(0usize, |total, ordinal| {
            let symbol =
                Spur::try_from_usize(ordinal).ok_or(PackedRirEncodeError::ResourceLimit)?;
            total
                .checked_add(symbols.resolve(&symbol).len() + 5)
                .ok_or(PackedRirEncodeError::ResourceLimit)
        })?;
        symbol_bytes
            .try_reserve_exact(spelling_bytes)
            .map_err(|_| PackedRirEncodeError::CapacityFailure)?;
        for ordinal in 0..symbols.len() {
            self.check()?;
            let symbol =
                Spur::try_from_usize(ordinal).ok_or(PackedRirEncodeError::ResourceLimit)?;
            let spelling = symbols.resolve(&symbol).as_bytes();
            put_u32(
                &mut symbol_bytes,
                u32::try_from(spelling.len()).map_err(|_| PackedRirEncodeError::ResourceLimit)?,
            );
            for chunk in spelling.chunks(4096) {
                self.check()?;
                symbol_bytes.extend_from_slice(chunk);
            }
        }

        if metadata.declaration.as_u32() >= instruction_count
            || metadata.declaration.as_u32().checked_add(1) != Some(instruction_count)
            || metadata.method_owner.is_some_and(|owner| {
                owner.declaration != metadata.declaration
                    || owner.name.into_usize() >= symbols.len()
            })
        {
            return Err(PackedRirEncodeError::InvalidMetadata);
        }
        let declaration = &rir.get(metadata.declaration).data;
        let valid_declaration = match declaration {
            InstData::FnDecl { .. }
            | InstData::ConstDecl { .. }
            | InstData::EnumDecl { .. }
            | InstData::DropFnDecl { .. } => true,
            InstData::StructDecl { methods, .. } => rir.struct_methods(methods).len() == 0,
            _ => false,
        };
        if !valid_declaration {
            return Err(PackedRirEncodeError::InvalidMetadata);
        }
        if metadata.method_owner.is_some()
            && !matches!(&rir.get(metadata.declaration).data, InstData::FnDecl { .. })
        {
            return Err(PackedRirEncodeError::InvalidMetadata);
        }

        let symbols_offset = HEADER_LEN;
        let types_offset = symbols_offset + symbol_bytes.len();
        let instructions_offset = types_offset + self.types.len();
        let basis_offset = instructions_offset + self.bytes.len();
        let end_offset = basis_offset + self.basis.len();
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(end_offset)
            .map_err(|_| PackedRirEncodeError::CapacityFailure)?;
        bytes.resize(HEADER_LEN, 0);
        bytes[0..4].copy_from_slice(MAGIC);
        bytes[4] = VERSION;
        bytes[5] = self.fallible_intrinsics.0;
        write_header_u32(&mut bytes, 8, instruction_count)?;
        write_header_u32(&mut bytes, 12, symbol_count)?;
        write_header_u32(&mut bytes, 16, type_count)?;
        write_header_u32(&mut bytes, 20, metadata.declaration.as_u32())?;
        if let Some(owner) = metadata.method_owner {
            bytes[24] = 1;
            bytes[25] = owner.is_public as u8 | ((owner.is_linear as u8) << 1);
            write_header_u32(&mut bytes, 28, owner.declaration.as_u32())?;
            write_header_u32(
                &mut bytes,
                32,
                u32::try_from(owner.name.into_usize())
                    .map_err(|_| PackedRirEncodeError::ResourceLimit)?,
            )?;
        }
        write_header_u32(
            &mut bytes,
            36,
            u32::try_from(symbols_offset).map_err(|_| PackedRirEncodeError::ResourceLimit)?,
        )?;
        write_header_u32(
            &mut bytes,
            40,
            u32::try_from(types_offset).map_err(|_| PackedRirEncodeError::ResourceLimit)?,
        )?;
        write_header_u32(
            &mut bytes,
            44,
            u32::try_from(instructions_offset).map_err(|_| PackedRirEncodeError::ResourceLimit)?,
        )?;
        write_header_u32(
            &mut bytes,
            48,
            u32::try_from(basis_offset).map_err(|_| PackedRirEncodeError::ResourceLimit)?,
        )?;
        write_header_u32(
            &mut bytes,
            52,
            u32::try_from(end_offset).map_err(|_| PackedRirEncodeError::ResourceLimit)?,
        )?;
        write_header_u32(&mut bytes, 56, self.span_count)?;
        bytes.extend_from_slice(&symbol_bytes);
        bytes.extend_from_slice(&self.types);
        bytes.extend_from_slice(&self.bytes);
        bytes.extend_from_slice(&self.basis);
        assert_eq!(
            bytes.len(),
            end_offset,
            "packed RIR section sizing must exactly match the emitted envelope",
        );
        Ok(PackedValidatedRir(Arc::from(bytes)))
    }

    fn check(&mut self) -> Result<(), PackedRirEncodeError<E>> {
        (self.checkpoint)().map_err(PackedRirEncodeError::Checkpoint)
    }

    fn byte(&mut self, value: u8) -> Result<(), PackedRirEncodeError<E>> {
        self.bytes
            .try_reserve(1)
            .map_err(|_| PackedRirEncodeError::CapacityFailure)?;
        self.bytes.push(value);
        Ok(())
    }

    fn u32(&mut self, value: u32) -> Result<(), PackedRirEncodeError<E>> {
        self.bytes
            .try_reserve(5)
            .map_err(|_| PackedRirEncodeError::CapacityFailure)?;
        put_u32(&mut self.bytes, value);
        Ok(())
    }

    fn u64(&mut self, value: u64) -> Result<(), PackedRirEncodeError<E>> {
        self.bytes
            .try_reserve(10)
            .map_err(|_| PackedRirEncodeError::CapacityFailure)?;
        put_u64(&mut self.bytes, value);
        Ok(())
    }

    fn boolean(&mut self, value: bool) -> Result<(), PackedRirEncodeError<E>> {
        self.byte(value as u8)
    }

    fn reference(&mut self, value: InstRef) -> Result<(), PackedRirEncodeError<E>> {
        if value.as_u32() >= self.current_instruction {
            return Err(PackedRirEncodeError::ForwardReference {
                instruction: self.current_instruction,
                reference: value.as_u32(),
            });
        }
        self.u32(value.as_u32())
    }

    fn optional_ref(&mut self, value: Option<InstRef>) -> Result<(), PackedRirEncodeError<E>> {
        self.boolean(value.is_some())?;
        if let Some(value) = value {
            self.reference(value)?;
        }
        Ok(())
    }

    fn symbol(&mut self, value: Spur) -> Result<(), PackedRirEncodeError<E>> {
        let ordinal = value.into_usize();
        if ordinal >= self.symbol_count {
            return Err(PackedRirEncodeError::InvalidMetadata);
        }
        let value = u32::try_from(ordinal).map_err(|_| PackedRirEncodeError::ResourceLimit)?;
        self.u32(value)
    }

    fn optional_symbol(&mut self, value: Option<Spur>) -> Result<(), PackedRirEncodeError<E>> {
        self.boolean(value.is_some())?;
        if let Some(value) = value {
            self.symbol(value)?;
        }
        Ok(())
    }

    fn type_reference(&mut self, value: RirTypeSyntaxRef) -> Result<(), PackedRirEncodeError<E>> {
        if value.index() >= self.type_count {
            return Err(PackedRirEncodeError::InvalidMetadata);
        }
        self.u32(value.as_u32())
    }

    fn optional_type_reference(
        &mut self,
        value: Option<RirTypeSyntaxRef>,
    ) -> Result<(), PackedRirEncodeError<E>> {
        self.boolean(value.is_some())?;
        if let Some(value) = value {
            self.type_reference(value)?;
        }
        Ok(())
    }

    fn type_byte(&mut self, value: u8) -> Result<(), PackedRirEncodeError<E>> {
        self.types
            .try_reserve(1)
            .map_err(|_| PackedRirEncodeError::CapacityFailure)?;
        self.types.push(value);
        Ok(())
    }

    fn type_u32(&mut self, value: u32) -> Result<(), PackedRirEncodeError<E>> {
        self.types
            .try_reserve(5)
            .map_err(|_| PackedRirEncodeError::CapacityFailure)?;
        put_u32(&mut self.types, value);
        Ok(())
    }

    fn type_count(&mut self, value: usize) -> Result<(), PackedRirEncodeError<E>> {
        self.type_u32(u32::try_from(value).map_err(|_| PackedRirEncodeError::ResourceLimit)?)
    }

    fn type_symbol(&mut self, value: Spur) -> Result<(), PackedRirEncodeError<E>> {
        let ordinal = value.into_usize();
        if ordinal >= self.symbol_count {
            return Err(PackedRirEncodeError::InvalidMetadata);
        }
        self.type_u32(u32::try_from(ordinal).map_err(|_| PackedRirEncodeError::ResourceLimit)?)
    }

    fn type_child(&mut self, value: RirTypeSyntaxRef) -> Result<(), PackedRirEncodeError<E>> {
        if value.as_u32() >= self.current_type {
            return Err(PackedRirEncodeError::InvalidMetadata);
        }
        self.type_u32(value.as_u32())
    }

    fn arena_symbol(
        arena: &RirTypeSyntaxArena<Spur>,
        symbol: RirTypeSyntaxSymbol,
    ) -> Result<Spur, PackedRirEncodeError<E>> {
        arena
            .symbol(symbol)
            .copied()
            .ok_or(PackedRirEncodeError::InvalidMetadata)
    }

    fn type_path(
        &mut self,
        arena: &RirTypeSyntaxArena<Spur>,
        range: RirTypeSyntaxRange,
    ) -> Result<(), PackedRirEncodeError<E>> {
        let words = arena
            .words(range)
            .ok_or(PackedRirEncodeError::InvalidMetadata)?;
        self.type_count(words.len())?;
        for word in words {
            self.check()?;
            let symbol = Self::arena_symbol(arena, RirTypeSyntaxSymbol::from_u32(*word))?;
            self.type_symbol(symbol)?;
        }
        Ok(())
    }

    fn type_children(
        &mut self,
        arena: &RirTypeSyntaxArena<Spur>,
        range: RirTypeSyntaxRange,
    ) -> Result<(), PackedRirEncodeError<E>> {
        let words = arena
            .words(range)
            .ok_or(PackedRirEncodeError::InvalidMetadata)?;
        self.type_count(words.len())?;
        for word in words {
            self.check()?;
            self.type_child(RirTypeSyntaxRef::from_u32(*word))?;
        }
        Ok(())
    }

    fn type_arena(
        &mut self,
        arena: &RirTypeSyntaxArena<Spur>,
    ) -> Result<(), PackedRirEncodeError<E>> {
        for (owner, node) in arena.nodes().iter().enumerate() {
            self.check()?;
            self.current_type =
                u32::try_from(owner).map_err(|_| PackedRirEncodeError::ResourceLimit)?;
            match node {
                RirTypeSyntaxNode::Named(symbol) => {
                    self.type_byte(0)?;
                    self.type_symbol(Self::arena_symbol(arena, *symbol)?)?;
                }
                RirTypeSyntaxNode::Qualified { path } => {
                    self.type_byte(1)?;
                    self.type_path(arena, *path)?;
                }
                RirTypeSyntaxNode::Unit => self.type_byte(2)?,
                RirTypeSyntaxNode::Never => self.type_byte(3)?,
                RirTypeSyntaxNode::Array { element, length } => {
                    self.type_byte(4)?;
                    self.type_child(*element)?;
                    self.type_child(*length)?;
                }
                RirTypeSyntaxNode::Slice { element } => {
                    self.type_byte(5)?;
                    self.type_child(*element)?;
                }
                RirTypeSyntaxNode::AnonymousStruct { fields, methods } => {
                    self.type_byte(6)?;
                    let fields = arena
                        .words(*fields)
                        .ok_or(PackedRirEncodeError::InvalidMetadata)?;
                    self.type_count(fields.len() / 2)?;
                    for field in fields.chunks_exact(2) {
                        self.check()?;
                        self.type_symbol(Self::arena_symbol(
                            arena,
                            RirTypeSyntaxSymbol::from_u32(field[0]),
                        )?)?;
                        self.type_child(RirTypeSyntaxRef::from_u32(field[1]))?;
                    }

                    let methods = arena
                        .words(*methods)
                        .ok_or(PackedRirEncodeError::InvalidMetadata)?;
                    let mut position = 0usize;
                    let mut method_count = 0usize;
                    while position < methods.len() {
                        method_count = method_count
                            .checked_add(1)
                            .ok_or(PackedRirEncodeError::ResourceLimit)?;
                        let header = &methods[position..position + 4];
                        position += 4 + (header[3] as usize) * 3;
                        let directives = methods[position + 1] as usize;
                        position += 2;
                        for _ in 0..directives {
                            let arguments = methods[position + 1] as usize;
                            position += 2 + arguments;
                        }
                    }
                    self.type_count(method_count)?;
                    position = 0;
                    while position < methods.len() {
                        self.check()?;
                        let header = &methods[position..position + 4];
                        self.type_symbol(Self::arena_symbol(
                            arena,
                            RirTypeSyntaxSymbol::from_u32(header[0]),
                        )?)?;
                        self.type_byte(u8::from(header[1] != u32::MAX))?;
                        if header[1] != u32::MAX {
                            self.type_byte(
                                u8::try_from(header[1])
                                    .map_err(|_| PackedRirEncodeError::InvalidMetadata)?,
                            )?;
                        }
                        self.type_byte(
                            u8::try_from(header[2])
                                .map_err(|_| PackedRirEncodeError::InvalidMetadata)?,
                        )?;
                        self.type_u32(header[3])?;
                        position += 4;
                        for _ in 0..header[3] {
                            let parameter = &methods[position..position + 3];
                            self.type_byte(
                                u8::try_from(parameter[0])
                                    .map_err(|_| PackedRirEncodeError::InvalidMetadata)?,
                            )?;
                            self.type_symbol(Self::arena_symbol(
                                arena,
                                RirTypeSyntaxSymbol::from_u32(parameter[1]),
                            )?)?;
                            self.type_child(RirTypeSyntaxRef::from_u32(parameter[2]))?;
                            position += 3;
                        }
                        self.type_child(RirTypeSyntaxRef::from_u32(methods[position]))?;
                        let directive_count = methods[position + 1];
                        self.type_u32(directive_count)?;
                        position += 2;
                        for _ in 0..directive_count {
                            self.type_symbol(Self::arena_symbol(
                                arena,
                                RirTypeSyntaxSymbol::from_u32(methods[position]),
                            )?)?;
                            let argument_count = methods[position + 1];
                            self.type_u32(argument_count)?;
                            position += 2;
                            for _ in 0..argument_count {
                                self.type_symbol(Self::arena_symbol(
                                    arena,
                                    RirTypeSyntaxSymbol::from_u32(methods[position]),
                                )?)?;
                                position += 1;
                            }
                        }
                    }
                }
                RirTypeSyntaxNode::AnonymousEnum { variants } => {
                    self.type_byte(7)?;
                    let words = arena
                        .words(*variants)
                        .ok_or(PackedRirEncodeError::InvalidMetadata)?;
                    let mut position = 0usize;
                    let mut count = 0usize;
                    while position < words.len() {
                        count = count
                            .checked_add(1)
                            .ok_or(PackedRirEncodeError::ResourceLimit)?;
                        position += 2 + words[position + 1] as usize;
                    }
                    self.type_count(count)?;
                    position = 0;
                    while position < words.len() {
                        self.check()?;
                        self.type_symbol(Self::arena_symbol(
                            arena,
                            RirTypeSyntaxSymbol::from_u32(words[position]),
                        )?)?;
                        let payload_count = words[position + 1];
                        self.type_u32(payload_count)?;
                        position += 2;
                        for _ in 0..payload_count {
                            self.type_child(RirTypeSyntaxRef::from_u32(words[position]))?;
                            position += 1;
                        }
                    }
                }
                RirTypeSyntaxNode::PointerConst { pointee } => {
                    self.type_byte(8)?;
                    self.type_child(*pointee)?;
                }
                RirTypeSyntaxNode::PointerMut { pointee } => {
                    self.type_byte(9)?;
                    self.type_child(*pointee)?;
                }
                RirTypeSyntaxNode::TypeCall { path, arguments } => {
                    self.type_byte(10)?;
                    self.type_path(arena, *path)?;
                    self.type_children(arena, *arguments)?;
                }
                RirTypeSyntaxNode::ValueCall { name, arguments } => {
                    self.type_byte(11)?;
                    self.type_symbol(Self::arena_symbol(arena, *name)?)?;
                    self.type_children(arena, *arguments)?;
                }
                RirTypeSyntaxNode::Integer(value) => {
                    self.type_byte(12)?;
                    self.types
                        .try_reserve(16)
                        .map_err(|_| PackedRirEncodeError::CapacityFailure)?;
                    self.types.extend_from_slice(&value.to_le_bytes());
                }
            }
        }
        Ok(())
    }

    fn count(&mut self, count: usize) -> Result<(), PackedRirEncodeError<E>> {
        self.u32(u32::try_from(count).map_err(|_| PackedRirEncodeError::ResourceLimit)?)
    }

    fn basis_span(&mut self, slot: RirSpanSlot, span: Span) -> Result<(), PackedRirEncodeError<E>> {
        self.check()?;
        let (start, end) = (self.project_span)(slot, span)
            .map_err(|error| PackedRirEncodeError::SpanProjection { slot, error })?;
        if start > end {
            return Err(PackedRirEncodeError::InvalidProjectedSpan { slot, start, end });
        }
        self.basis
            .try_reserve(10)
            .map_err(|_| PackedRirEncodeError::CapacityFailure)?;
        put_u32(&mut self.basis, start);
        put_u32(&mut self.basis, end);
        self.span_count = self
            .span_count
            .checked_add(1)
            .ok_or(PackedRirEncodeError::ResourceLimit)?;
        Ok(())
    }

    fn anchor(&mut self, anchor: &RirStructuralAnchor) -> Result<(), PackedRirEncodeError<E>> {
        self.count(anchor.segments().len())?;
        for segment in anchor.segments() {
            self.check()?;
            match *segment {
                RirStructuralPathSegment::Body => self.byte(0)?,
                RirStructuralPathSegment::ParameterType(value) => {
                    self.byte(1)?;
                    self.u32(value)?;
                }
                RirStructuralPathSegment::ReturnType => self.byte(2)?,
                RirStructuralPathSegment::Statement(value) => {
                    self.byte(3)?;
                    self.u32(value)?;
                }
                RirStructuralPathSegment::Operand(value) => {
                    self.byte(4)?;
                    self.u32(value)?;
                }
                RirStructuralPathSegment::Branch(value) => {
                    self.byte(5)?;
                    self.u32(value)?;
                }
                RirStructuralPathSegment::MatchArm(value) => {
                    self.byte(6)?;
                    self.u32(value)?;
                }
                RirStructuralPathSegment::FieldType(value) => {
                    self.byte(7)?;
                    self.u32(value)?;
                }
                RirStructuralPathSegment::VariantPayload { variant, payload } => {
                    self.byte(8)?;
                    self.u32(variant)?;
                    self.u32(payload)?;
                }
                RirStructuralPathSegment::Method(value) => {
                    self.byte(9)?;
                    self.u32(value)?;
                }
                RirStructuralPathSegment::AnonymousType(value) => {
                    self.byte(10)?;
                    self.u32(value)?;
                }
                RirStructuralPathSegment::StringLiteral(value) => {
                    self.byte(11)?;
                    self.u32(value)?;
                }
                RirStructuralPathSegment::ReadOnlyData(value) => {
                    self.byte(12)?;
                    self.u32(value)?;
                }
            }
        }
        Ok(())
    }

    fn optional_anchor(
        &mut self,
        anchor: Option<&RirStructuralAnchor>,
    ) -> Result<(), PackedRirEncodeError<E>> {
        self.boolean(anchor.is_some())?;
        if let Some(anchor) = anchor {
            self.anchor(anchor)?;
        }
        Ok(())
    }

    fn refs(&mut self, values: RirSlice<'_, InstRef>) -> Result<(), PackedRirEncodeError<E>> {
        self.count(values.len())?;
        for value in values.values() {
            self.check()?;
            self.reference(value)?;
        }
        Ok(())
    }

    fn call_args(
        &mut self,
        rir: &ValidatedRir,
        range: &RirCallArgsRange,
    ) -> Result<(), PackedRirEncodeError<E>> {
        let values = rir.call_args(range);
        self.count(values.len())?;
        for value in values.values() {
            self.check()?;
            self.reference(value.value)?;
            self.byte(encode_arg_mode(value.mode))?;
        }
        Ok(())
    }

    fn fields(
        &mut self,
        values: RirSlice<'_, (Spur, RirTypeSyntaxRef)>,
    ) -> Result<(), PackedRirEncodeError<E>> {
        self.count(values.len())?;
        for (name, ty) in values.values() {
            self.check()?;
            self.symbol(name)?;
            self.type_reference(ty)?;
        }
        Ok(())
    }

    fn field_inits(
        &mut self,
        values: RirSlice<'_, (Spur, InstRef)>,
    ) -> Result<(), PackedRirEncodeError<E>> {
        self.count(values.len())?;
        for (name, value) in values.values() {
            self.check()?;
            self.symbol(name)?;
            self.reference(value)?;
        }
        Ok(())
    }

    fn directives(
        &mut self,
        rir: &ValidatedRir,
        instruction: InstRef,
        range: &RirDirectivesRange,
        mut field: impl FnMut(u32) -> RirSpanField,
    ) -> Result<(), PackedRirEncodeError<E>> {
        let values = rir.directives(range);
        self.count(values.len())?;
        for (ordinal, value) in values.iter().enumerate() {
            self.check()?;
            self.symbol(value.name)?;
            self.count(value.args.len())?;
            for argument in value.args.values() {
                self.check()?;
                self.symbol(argument)?;
            }
            let ordinal =
                u32::try_from(ordinal).map_err(|_| PackedRirEncodeError::ResourceLimit)?;
            self.basis_span(RirSpanSlot::new(instruction, field(ordinal)), value.span)?;
        }
        Ok(())
    }

    fn pattern(&mut self, value: &RirPatternView<'_>) -> Result<(), PackedRirEncodeError<E>> {
        match value {
            RirPatternView::Wildcard(_) => self.byte(0)?,
            RirPatternView::Int {
                value, negative, ..
            } => {
                self.byte(1)?;
                self.u64(*value)?;
                self.boolean(*negative)?;
            }
            RirPatternView::Bool(value, _) => {
                self.byte(2)?;
                self.boolean(*value)?;
            }
            RirPatternView::Path {
                module,
                ctor_head,
                type_name,
                variant,
                bindings,
                ..
            } => {
                self.byte(3)?;
                self.optional_ref(*module)?;
                self.optional_ref(*ctor_head)?;
                self.symbol(*type_name)?;
                self.symbol(*variant)?;
                self.count(bindings.len())?;
                for binding in bindings.values() {
                    self.check()?;
                    self.symbol(binding)?;
                }
            }
        }
        Ok(())
    }

    fn enum_payload(
        &mut self,
        variants: RirSymbols<'_>,
        payloads: RirEnumPayloads<'_>,
    ) -> Result<(), PackedRirEncodeError<E>> {
        self.count(variants.len())?;
        for (variant, payload) in variants.values().zip(payloads) {
            self.check()?;
            self.symbol(variant)?;
            self.count(payload.len())?;
            for ty in payload.values() {
                self.check()?;
                self.type_reference(ty)?;
            }
        }
        Ok(())
    }

    fn instruction(
        &mut self,
        rir: &ValidatedRir,
        symbols: &ThreadedRodeo,
        instruction: InstRef,
        value: &Inst,
    ) -> Result<(), PackedRirEncodeError<E>> {
        self.basis_span(
            RirSpanSlot::new(instruction, RirSpanField::Instruction),
            value.span,
        )?;
        macro_rules! binary {
            ($opcode:expr, $lhs:expr, $rhs:expr) => {{
                self.byte($opcode)?;
                self.reference(*$lhs)?;
                self.reference(*$rhs)?;
            }};
        }
        macro_rules! unary {
            ($opcode:expr, $operand:expr) => {{
                self.byte($opcode)?;
                self.reference(*$operand)?;
            }};
        }
        match &value.data {
            InstData::IntConst(value) => {
                self.byte(0)?;
                self.u64(*value)?;
            }
            InstData::FloatConst { text } => {
                self.byte(1)?;
                self.symbol(*text)?;
            }
            InstData::BoolConst(value) => {
                self.byte(2)?;
                self.boolean(*value)?;
            }
            InstData::StringConst { content, anchor } => {
                self.byte(3)?;
                self.symbol(*content)?;
                self.anchor(anchor)?;
            }
            InstData::UnitConst => self.byte(4)?,
            InstData::Add { lhs, rhs } => binary!(5, lhs, rhs),
            InstData::Sub { lhs, rhs } => binary!(6, lhs, rhs),
            InstData::Mul { lhs, rhs } => binary!(7, lhs, rhs),
            InstData::Div { lhs, rhs } => binary!(8, lhs, rhs),
            InstData::Mod { lhs, rhs } => binary!(9, lhs, rhs),
            InstData::Eq { lhs, rhs } => binary!(10, lhs, rhs),
            InstData::Ne { lhs, rhs } => binary!(11, lhs, rhs),
            InstData::Lt { lhs, rhs } => binary!(12, lhs, rhs),
            InstData::Gt { lhs, rhs } => binary!(13, lhs, rhs),
            InstData::Le { lhs, rhs } => binary!(14, lhs, rhs),
            InstData::Ge { lhs, rhs } => binary!(15, lhs, rhs),
            InstData::And { lhs, rhs } => binary!(16, lhs, rhs),
            InstData::Or { lhs, rhs } => binary!(17, lhs, rhs),
            InstData::BitAnd { lhs, rhs } => binary!(18, lhs, rhs),
            InstData::BitOr { lhs, rhs } => binary!(19, lhs, rhs),
            InstData::BitXor { lhs, rhs } => binary!(20, lhs, rhs),
            InstData::Shl { lhs, rhs } => binary!(21, lhs, rhs),
            InstData::Shr { lhs, rhs } => binary!(22, lhs, rhs),
            InstData::Neg { operand } => unary!(23, operand),
            InstData::Not { operand } => unary!(24, operand),
            InstData::BitNot { operand } => unary!(25, operand),
            InstData::Try { operand } => unary!(26, operand),
            InstData::Branch {
                cond,
                then_block,
                else_block,
            } => {
                self.byte(27)?;
                self.reference(*cond)?;
                self.reference(*then_block)?;
                self.optional_ref(*else_block)?;
            }
            InstData::Loop { cond, body } => binary!(28, cond, body),
            InstData::InfiniteLoop { body, iter_borrow } => {
                self.byte(29)?;
                self.reference(*body)?;
                self.optional_symbol(*iter_borrow)?;
            }
            InstData::Match { scrutinee, arms } => {
                self.byte(30)?;
                self.reference(*scrutinee)?;
                let arms = rir.match_arms(arms);
                self.count(arms.len())?;
                for (ordinal, (pattern, body)) in arms.iter().enumerate() {
                    self.check()?;
                    self.pattern(&pattern)?;
                    self.reference(body)?;
                    self.basis_span(
                        RirSpanSlot::new(
                            instruction,
                            RirSpanField::MatchPattern {
                                arm: u32::try_from(ordinal)
                                    .map_err(|_| PackedRirEncodeError::ResourceLimit)?,
                            },
                        ),
                        pattern.span(),
                    )?;
                }
            }
            InstData::Break { value } => {
                self.byte(31)?;
                self.optional_ref(*value)?;
            }
            InstData::Continue => self.byte(32)?,
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
                self.byte(33)?;
                self.directives(rir, instruction, directives, |ordinal| {
                    RirSpanField::FunctionDirective { directive: ordinal }
                })?;
                self.byte(
                    *is_pub as u8
                        | ((*is_unchecked as u8) << 1)
                        | ((*is_extern as u8) << 2)
                        | ((*is_c_export as u8) << 3)
                        | ((*has_self as u8) << 4)
                        | ((*self_is_mut as u8) << 5)
                        | ((*returns_borrow as u8) << 6)
                        | ((*returns_inout as u8) << 7),
                )?;
                self.symbol(*name)?;
                let params = rir.params(params);
                self.count(params.len())?;
                for (ordinal, parameter) in params.values().enumerate() {
                    self.check()?;
                    self.symbol(parameter.name)?;
                    self.type_reference(parameter.ty)?;
                    self.byte(encode_param_mode(parameter.mode))?;
                    self.boolean(parameter.is_comptime)?;
                    self.basis_span(
                        RirSpanSlot::new(
                            instruction,
                            RirSpanField::FunctionParameter {
                                parameter: u32::try_from(ordinal)
                                    .map_err(|_| PackedRirEncodeError::ResourceLimit)?,
                            },
                        ),
                        parameter.span,
                    )?;
                }
                self.type_reference(*return_type)?;
                self.reference(*body)?;
                self.byte(encode_param_mode(*self_mode))?;
            }
            InstData::ConstDecl {
                directives,
                is_pub,
                name,
                ty,
                init,
            } => {
                self.byte(34)?;
                self.directives(rir, instruction, directives, |ordinal| {
                    RirSpanField::ConstDirective { directive: ordinal }
                })?;
                self.boolean(*is_pub)?;
                self.symbol(*name)?;
                self.optional_type_reference(*ty)?;
                self.reference(*init)?;
            }
            InstData::Call { name, args } => {
                self.byte(35)?;
                self.symbol(*name)?;
                self.call_args(rir, args)?;
            }
            InstData::Intrinsic { name, args } => {
                self.byte(36)?;
                self.symbol(*name)?;
                if let Some(intrinsic) = RirFallibleIntrinsic::from_name(symbols.resolve(name)) {
                    self.fallible_intrinsics.insert(intrinsic);
                }
                self.refs(rir.intrinsic_args(args))?;
            }
            InstData::InternalIntrinsic { intrinsic, args } => {
                self.byte(37)?;
                self.byte(encode_internal_intrinsic(*intrinsic))?;
                self.refs(rir.internal_intrinsic_args(args))?;
            }
            InstData::TypeIntrinsic { name, type_arg } => {
                self.byte(38)?;
                self.symbol(*name)?;
                self.type_reference(*type_arg)?;
            }
            InstData::OffsetOf { type_arg, field } => {
                self.byte(39)?;
                self.type_reference(*type_arg)?;
                self.symbol(*field)?;
            }
            InstData::Ret(value) => {
                self.byte(40)?;
                self.optional_ref(*value)?;
            }
            InstData::Yield(value) => unary!(41, value),
            InstData::Block { instructions } => {
                self.byte(42)?;
                self.refs(rir.block_insts(instructions))?;
            }
            InstData::Alloc {
                directives,
                name,
                is_mut,
                ty,
                init,
                iter_elem,
            } => {
                self.byte(43)?;
                self.directives(rir, instruction, directives, |ordinal| {
                    RirSpanField::AllocDirective { directive: ordinal }
                })?;
                self.optional_symbol(*name)?;
                self.boolean(*is_mut)?;
                self.optional_type_reference(*ty)?;
                self.reference(*init)?;
                self.boolean(*iter_elem)?;
            }
            InstData::VarRef { name, anchor } => {
                self.byte(44)?;
                self.symbol(*name)?;
                self.optional_anchor(anchor.as_ref())?;
            }
            InstData::Assign { name, value } => {
                self.byte(45)?;
                self.symbol(*name)?;
                self.reference(*value)?;
            }
            InstData::PlaceSet { place, value } => {
                self.byte(63)?;
                self.reference(*place)?;
                self.reference(*value)?;
            }
            InstData::StructDecl {
                directives,
                is_pub,
                is_linear,
                name,
                fields,
                methods,
            } => {
                self.byte(46)?;
                self.directives(rir, instruction, directives, |ordinal| {
                    RirSpanField::StructDirective { directive: ordinal }
                })?;
                self.byte(*is_pub as u8 | ((*is_linear as u8) << 1))?;
                self.symbol(*name)?;
                self.fields(rir.struct_fields(fields))?;
                self.refs(rir.struct_methods(methods))?;
            }
            InstData::StructInit {
                module,
                ctor_head,
                type_name,
                fields,
                shorthand_span,
            } => {
                self.byte(47)?;
                self.optional_ref(*module)?;
                self.optional_ref(*ctor_head)?;
                self.symbol(*type_name)?;
                self.field_inits(rir.field_inits(fields))?;
                self.boolean(shorthand_span.is_some())?;
                if let Some(span) = shorthand_span {
                    self.basis_span(
                        RirSpanSlot::new(instruction, RirSpanField::StructInitShorthand),
                        *span,
                    )?;
                }
            }
            InstData::FieldGet { base, field } => {
                self.byte(48)?;
                self.reference(*base)?;
                self.symbol(*field)?;
            }
            InstData::FieldSet { base, field, value } => {
                self.byte(49)?;
                self.reference(*base)?;
                self.symbol(*field)?;
                self.reference(*value)?;
            }
            InstData::EnumDecl {
                is_pub,
                is_non_exhaustive,
                name,
                variants,
                payloads,
            } => {
                self.byte(50)?;
                self.boolean(*is_pub)?;
                self.boolean(*is_non_exhaustive)?;
                self.symbol(*name)?;
                self.enum_payload(
                    rir.enum_variants(variants),
                    rir.enum_payloads(payloads, variants),
                )?;
            }
            InstData::EnumVariant {
                module,
                type_name,
                variant,
            } => {
                self.byte(51)?;
                self.optional_ref(*module)?;
                self.symbol(*type_name)?;
                self.symbol(*variant)?;
            }
            InstData::ArrayInit { elements } => {
                self.byte(52)?;
                self.refs(rir.array_elements(elements))?;
            }
            InstData::ArrayRepeat { value, count } => {
                self.byte(53)?;
                self.reference(*value)?;
                match count {
                    RepeatCount::Literal(value) => {
                        self.byte(0)?;
                        self.u64(*value)?;
                    }
                    RepeatCount::Named(value) => {
                        self.byte(1)?;
                        self.symbol(*value)?;
                    }
                }
            }
            InstData::IndexGet { base, index } => binary!(54, base, index),
            InstData::IndexSet { base, index, value } => {
                self.byte(55)?;
                self.reference(*base)?;
                self.reference(*index)?;
                self.reference(*value)?;
            }
            InstData::MethodCall {
                receiver,
                method,
                args,
            } => {
                self.byte(56)?;
                self.reference(*receiver)?;
                self.symbol(*method)?;
                self.call_args(rir, args)?;
            }
            InstData::DropFnDecl { type_name, body } => {
                self.byte(57)?;
                self.symbol(*type_name)?;
                self.reference(*body)?;
            }
            InstData::Comptime { expr } => unary!(58, expr),
            InstData::Checked { expr } => unary!(59, expr),
            InstData::TypeConst { type_name } => {
                self.byte(60)?;
                self.type_reference(*type_name)?;
            }
            InstData::AnonStructType {
                fields,
                methods,
                anchor,
            } => {
                self.byte(61)?;
                self.fields(rir.anon_struct_fields(fields))?;
                self.refs(rir.anon_struct_methods(methods))?;
                self.anchor(anchor)?;
            }
            InstData::AnonEnumType {
                variants,
                payloads,
                anchor,
            } => {
                self.byte(62)?;
                self.enum_payload(
                    rir.anon_enum_variants(variants),
                    rir.anon_enum_payloads(payloads, variants),
                )?;
                self.anchor(anchor)?;
            }
        }
        Ok(())
    }
}

fn put_u32(bytes: &mut Vec<u8>, value: u32) {
    put_u64(bytes, u64::from(value));
}

fn put_u64(bytes: &mut Vec<u8>, mut value: u64) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        bytes.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn write_header_u32<E>(
    bytes: &mut [u8],
    offset: usize,
    value: u32,
) -> Result<(), PackedRirEncodeError<E>> {
    let target = bytes
        .get_mut(offset..offset + 4)
        .ok_or(PackedRirEncodeError::ResourceLimit)?;
    target.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn header_u32(bytes: &[u8], offset: usize) -> Result<u32, PackedRirDecodeError> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or(PackedRirDecodeError::Truncated)?;
    Ok(u32::from_le_bytes(
        value.try_into().expect("four-byte slice"),
    ))
}

#[derive(Debug, Clone, Copy)]
struct Header {
    instructions: u32,
    symbols: u32,
    types: u32,
    declaration: u32,
    owner: Option<(u32, u32, bool, bool)>,
    symbols_offset: usize,
    types_offset: usize,
    instructions_offset: usize,
    basis_offset: usize,
    end_offset: usize,
    spans: u32,
    fallible_intrinsics: RirFallibleIntrinsicSet,
}

impl Header {
    fn parse(bytes: &[u8]) -> Result<Self, PackedRirDecodeError> {
        if bytes.len() < HEADER_LEN {
            return Err(PackedRirDecodeError::Truncated);
        }
        if &bytes[0..4] != MAGIC {
            return Err(PackedRirDecodeError::InvalidMagic);
        }
        if bytes[4] != VERSION {
            return Err(PackedRirDecodeError::UnsupportedVersion(bytes[4]));
        }
        if bytes[5] & !RirFallibleIntrinsicSet::VALID_BITS != 0 {
            return Err(PackedRirDecodeError::InvalidTag {
                family: "fallible intrinsic set",
                tag: bytes[5],
            });
        }
        if bytes[6..8].iter().any(|byte| *byte != 0)
            || bytes[26..28].iter().any(|byte| *byte != 0)
            || bytes[60..64].iter().any(|byte| *byte != 0)
        {
            return Err(PackedRirDecodeError::InvalidTag {
                family: "header reserved byte",
                tag: 1,
            });
        }
        let owner = match bytes[24] {
            0 => {
                if bytes[25] != 0 || header_u32(bytes, 28)? != 0 || header_u32(bytes, 32)? != 0 {
                    return Err(PackedRirDecodeError::InvalidTag {
                        family: "absent method owner",
                        tag: bytes[25],
                    });
                }
                None
            }
            1 if bytes[25] & !0b11 == 0 => Some((
                header_u32(bytes, 28)?,
                header_u32(bytes, 32)?,
                bytes[25] & 1 != 0,
                bytes[25] & 2 != 0,
            )),
            tag => {
                return Err(PackedRirDecodeError::InvalidTag {
                    family: "method owner",
                    tag,
                });
            }
        };
        let header = Self {
            instructions: header_u32(bytes, 8)?,
            symbols: header_u32(bytes, 12)?,
            types: header_u32(bytes, 16)?,
            declaration: header_u32(bytes, 20)?,
            owner,
            symbols_offset: header_u32(bytes, 36)? as usize,
            types_offset: header_u32(bytes, 40)? as usize,
            instructions_offset: header_u32(bytes, 44)? as usize,
            basis_offset: header_u32(bytes, 48)? as usize,
            end_offset: header_u32(bytes, 52)? as usize,
            spans: header_u32(bytes, 56)?,
            fallible_intrinsics: RirFallibleIntrinsicSet(bytes[5]),
        };
        if header.symbols_offset != HEADER_LEN
            || header.symbols_offset > header.types_offset
            || header.types_offset > header.instructions_offset
            || header.instructions_offset > header.basis_offset
            || header.basis_offset > header.end_offset
            || header.end_offset != bytes.len()
            || header.declaration >= header.instructions
            || header.declaration.checked_add(1) != Some(header.instructions)
        {
            return Err(PackedRirDecodeError::CountOutOfBounds {
                family: "header section",
            });
        }
        if let Some((declaration, name, _, _)) = header.owner
            && (declaration != header.declaration || name >= header.symbols)
        {
            return Err(PackedRirDecodeError::CountOutOfBounds {
                family: "method owner",
            });
        }
        Ok(header)
    }
}

#[derive(Clone)]
struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn byte(&mut self) -> Result<u8, PackedRirDecodeError> {
        let value = *self
            .bytes
            .get(self.position)
            .ok_or(PackedRirDecodeError::Truncated)?;
        self.position += 1;
        Ok(value)
    }

    fn boolean(&mut self, family: &'static str) -> Result<bool, PackedRirDecodeError> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            tag => Err(PackedRirDecodeError::InvalidTag { family, tag }),
        }
    }

    fn u32(&mut self) -> Result<u32, PackedRirDecodeError> {
        let value = self.u64()?;
        u32::try_from(value).map_err(|_| PackedRirDecodeError::VarintOverflow)
    }

    fn u64(&mut self) -> Result<u64, PackedRirDecodeError> {
        let start = self.position;
        let mut value = 0u64;
        let mut shift = 0u32;
        loop {
            let byte = self.byte()?;
            let payload = u64::from(byte & 0x7f);
            if shift == 63 && payload > 1 {
                return Err(PackedRirDecodeError::VarintOverflow);
            }
            value |= payload
                .checked_shl(shift)
                .ok_or(PackedRirDecodeError::VarintOverflow)?;
            if byte & 0x80 == 0 {
                if self.position - start > 1 && payload == 0 {
                    return Err(PackedRirDecodeError::NonMinimalVarint);
                }
                return Ok(value);
            }
            shift = shift
                .checked_add(7)
                .ok_or(PackedRirDecodeError::VarintOverflow)?;
            if shift >= 70 {
                return Err(PackedRirDecodeError::VarintOverflow);
            }
        }
    }

    fn bytes(&mut self, count: usize) -> Result<&'a [u8], PackedRirDecodeError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or(PackedRirDecodeError::Truncated)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(PackedRirDecodeError::Truncated)?;
        self.position = end;
        Ok(value)
    }

    fn finished(&self) -> bool {
        self.position == self.bytes.len()
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }
}

/// Borrowing dense spelling iterator over a packed candidate.
pub struct PackedRirSymbols<'a> {
    reader: Reader<'a>,
    remaining: usize,
    ordinal: u32,
}

impl<'a> PackedRirSymbols<'a> {
    fn new(bytes: &'a [u8]) -> Result<Self, PackedRirDecodeError> {
        let header = Header::parse(bytes)?;
        let section = bytes
            .get(header.symbols_offset..header.types_offset)
            .ok_or(PackedRirDecodeError::Truncated)?;
        // PackedValidatedRir has no public unchecked constructor: the encoder
        // produced and validated this immutable section. The fallible decoder
        // still revalidates symbols when accepting bytes at the append
        // boundary; this borrowing accessor must not walk the entire section
        // before walking it again to yield the spellings.
        Ok(Self {
            reader: Reader::new(section),
            remaining: header.symbols as usize,
            ordinal: 0,
        })
    }
}

impl<'a> Iterator for PackedRirSymbols<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let length = self.reader.u32().expect("prevalidated spelling length") as usize;
        let bytes = self
            .reader
            .bytes(length)
            .expect("prevalidated spelling bytes");
        let value = std::str::from_utf8(bytes).expect("prevalidated UTF-8 spelling");
        self.remaining -= 1;
        self.ordinal += 1;
        Some(value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for PackedRirSymbols<'_> {}

struct Decoder<'a, E, C, S, P> {
    bytes: &'a [u8],
    destination: &'a mut RirEditor,
    root_methods: Option<&'a [InstRef]>,
    revalidate_symbols: bool,
    checkpoint: C,
    remap_symbol: S,
    remap_span: P,
    instructions: u32,
    symbols: u32,
    types: u32,
    type_nodes: Vec<RirTypeSyntaxRef>,
    source_instruction: u32,
    destination_instruction_start: u32,
    spans_read: u32,
    marker: std::marker::PhantomData<fn() -> E>,
}

impl<
    'a,
    E,
    C: FnMut() -> Result<(), E>,
    S: FnMut(u32) -> Result<Spur, E>,
    P: FnMut(RirSpanSlot, (u32, u32)) -> Result<Span, E>,
> Decoder<'a, E, C, S, P>
{
    fn new(
        bytes: &'a [u8],
        destination: &'a mut RirEditor,
        root_methods: Option<&'a [InstRef]>,
        revalidate_symbols: bool,
        checkpoint: C,
        remap_symbol: S,
        remap_span: P,
    ) -> Self {
        Self {
            bytes,
            destination,
            root_methods,
            revalidate_symbols,
            checkpoint,
            remap_symbol,
            remap_span,
            instructions: 0,
            symbols: 0,
            types: 0,
            type_nodes: Vec::new(),
            source_instruction: 0,
            destination_instruction_start: 0,
            spans_read: 0,
            marker: std::marker::PhantomData,
        }
    }

    fn decode(mut self) -> Result<PackedRirAppend, PackedRirAppendError<E>> {
        let header = Header::parse(self.bytes)?;
        if self.revalidate_symbols {
            self.validate_symbols(header)?;
        }
        self.instructions = header.instructions;
        self.symbols = header.symbols;
        self.types = header.types;
        let type_bytes = header.instructions_offset - header.types_offset;
        let instruction_bytes = header.basis_offset - header.instructions_offset;
        let basis_bytes = header.end_offset - header.basis_offset;
        if header.types as usize > type_bytes
            || header.instructions as usize > instruction_bytes
            || header.spans as usize > basis_bytes / 2
        {
            return Err(PackedRirDecodeError::CountOutOfBounds {
                family: "header counts",
            }
            .into());
        }
        let mut types = Reader::new(
            self.bytes
                .get(header.types_offset..header.instructions_offset)
                .ok_or(PackedRirDecodeError::Truncated)?,
        );
        self.decode_types(&mut types)?;
        if !types.finished() {
            return Err(PackedRirDecodeError::TrailingBytes.into());
        }
        let mut instructions = Reader::new(
            self.bytes
                .get(header.instructions_offset..header.basis_offset)
                .ok_or(PackedRirDecodeError::Truncated)?,
        );
        let mut basis = Reader::new(
            self.bytes
                .get(header.basis_offset..header.end_offset)
                .ok_or(PackedRirDecodeError::Truncated)?,
        );
        let instruction_start = self.destination.rir.instructions.len();
        if let Some(methods) = self.root_methods {
            for method in methods {
                self.check()?;
                let index = method.as_u32() as usize;
                if index >= instruction_start
                    || !matches!(
                        self.destination.rir.instructions[index].data,
                        InstData::FnDecl { .. }
                    )
                {
                    return Err(PackedRirDecodeError::InvalidTag {
                        family: "composed struct method",
                        tag: 0,
                    }
                    .into());
                }
            }
        }
        self.destination_instruction_start = u32::try_from(instruction_start)
            .map_err(|_| PackedRirDecodeError::DestinationInstructionMismatch)?;
        let extra_start = self.destination.rir.extra.len();
        for source in 0..header.instructions {
            self.check()?;
            self.source_instruction = source;
            let span = self.span(&mut basis, RirSpanField::Instruction)?;
            let opcode = instructions.byte()?;
            let destination = self.instruction(opcode, span, &mut instructions, &mut basis)?;
            let expected = instruction_start
                .checked_add(source as usize)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or(PackedRirDecodeError::DestinationInstructionMismatch)?;
            if destination.as_u32() != expected {
                return Err(PackedRirDecodeError::DestinationInstructionMismatch.into());
            }
        }
        if !instructions.finished() || !basis.finished() || self.spans_read != header.spans {
            return Err(PackedRirDecodeError::TrailingBytes.into());
        }
        if let Some(error) = self.destination.capacity_error() {
            return Err(PackedRirAppendError::Build(error));
        }
        let root = instruction_start
            .checked_add(header.declaration as usize)
            .ok_or(PackedRirDecodeError::DestinationInstructionMismatch)?;
        let root_data = &self.destination.rir.instructions[root].data;
        let valid_declaration = if self.root_methods.is_some() {
            matches!(root_data, InstData::StructDecl { .. })
        } else {
            match root_data {
                InstData::FnDecl { .. }
                | InstData::ConstDecl { .. }
                | InstData::EnumDecl { .. }
                | InstData::DropFnDecl { .. } => true,
                InstData::StructDecl { methods, .. } => {
                    self.destination.struct_methods(methods).len() == 0
                }
                _ => false,
            }
        };
        if !valid_declaration
            || (header.owner.is_some() && !matches!(root_data, InstData::FnDecl { .. }))
        {
            return Err(PackedRirDecodeError::InvalidTag {
                family: "declaration root",
                tag: 0,
            }
            .into());
        }
        let range = RirAppendRange {
            instructions: u32::try_from(instruction_start)
                .map_err(|_| PackedRirDecodeError::DestinationInstructionMismatch)?
                ..u32::try_from(self.destination.rir.instructions.len())
                    .map_err(|_| PackedRirDecodeError::DestinationInstructionMismatch)?,
            extra: u32::try_from(extra_start)
                .map_err(|_| PackedRirDecodeError::DestinationInstructionMismatch)?
                ..u32::try_from(self.destination.rir.extra.len())
                    .map_err(|_| PackedRirDecodeError::DestinationInstructionMismatch)?,
        };
        let declaration = self.destination_ref(header.declaration)?;
        let method_owner = match header.owner {
            Some((owner_declaration, name, is_public, is_linear)) => Some(PackedRirMethodOwner {
                declaration: self.destination_ref(owner_declaration)?,
                name: (self.remap_symbol)(name).map_err(PackedRirAppendError::SymbolRemap)?,
                is_public,
                is_linear,
            }),
            None => None,
        };
        Ok(PackedRirAppend {
            range,
            metadata: PackedRirAppendMetadata {
                declaration,
                method_owner,
            },
        })
    }

    fn check(&mut self) -> Result<(), PackedRirAppendError<E>> {
        (self.checkpoint)().map_err(PackedRirAppendError::Checkpoint)
    }

    fn capacity(family: &'static str) -> PackedRirAppendError<E> {
        PackedRirAppendError::Build(RirPayloadBuildError::CapacityFailure { family })
    }

    fn validate_symbols(&mut self, header: Header) -> Result<(), PackedRirAppendError<E>> {
        let section = self
            .bytes
            .get(header.symbols_offset..header.types_offset)
            .ok_or(PackedRirDecodeError::Truncated)?;
        if header.symbols as usize > section.len() {
            return Err(PackedRirDecodeError::CountOutOfBounds {
                family: "dense symbols",
            }
            .into());
        }
        let mut reader = Reader::new(section);
        let hash_builder = RandomState::new();
        let mut seen: HashMap<(u64, usize, u32), (u32, &[u8])> = HashMap::new();
        seen.try_reserve(header.symbols as usize)
            .map_err(|_| Self::capacity("dense symbols"))?;
        for ordinal in 0..header.symbols {
            self.check()?;
            let length = reader.u32()? as usize;
            if length > reader.remaining() {
                return Err(PackedRirDecodeError::Truncated.into());
            }
            let spelling = reader.bytes(length)?;
            let mut hasher = hash_builder.build_hasher();
            let mut position = 0;
            while position < spelling.len() {
                self.check()?;
                let mut end = (position + 4096).min(spelling.len());
                while end < spelling.len() && spelling[end] & 0xc0 == 0x80 {
                    end -= 1;
                }
                if end == position {
                    return Err(PackedRirDecodeError::InvalidUtf8Symbol { symbol: ordinal }.into());
                }
                let chunk = &spelling[position..end];
                std::str::from_utf8(chunk)
                    .map_err(|_| PackedRirDecodeError::InvalidUtf8Symbol { symbol: ordinal })?;
                hasher.write(chunk);
                position = end;
            }
            let hash = hasher.finish();
            let mut collision = 0u32;
            loop {
                let Some(&(first, existing)) = seen.get(&(hash, spelling.len(), collision)) else {
                    seen.insert((hash, spelling.len(), collision), (ordinal, spelling));
                    break;
                };
                let mut equal = true;
                for (lhs, rhs) in existing.chunks(4096).zip(spelling.chunks(4096)) {
                    self.check()?;
                    if lhs != rhs {
                        equal = false;
                        break;
                    }
                }
                if equal {
                    return Err(PackedRirDecodeError::DuplicateSymbol {
                        first,
                        duplicate: ordinal,
                    }
                    .into());
                }
                collision =
                    collision
                        .checked_add(1)
                        .ok_or(PackedRirDecodeError::CountOutOfBounds {
                            family: "dense symbol hash collisions",
                        })?;
            }
        }
        if !reader.finished() {
            return Err(PackedRirDecodeError::TrailingBytes.into());
        }
        Ok(())
    }

    fn type_symbol(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<RirTypeSyntaxSymbol, PackedRirAppendError<E>> {
        let symbol = self.symbol(reader)?;
        self.destination
            .type_syntax
            .intern_symbol(symbol)
            .map_err(|error| PackedRirAppendError::Build(type_syntax_build_error(error)))
    }

    fn type_child(
        &self,
        reader: &mut Reader<'_>,
    ) -> Result<RirTypeSyntaxRef, PackedRirAppendError<E>> {
        let reference = reader.u32()?;
        self.type_nodes
            .get(reference as usize)
            .copied()
            .ok_or_else(|| {
                PackedRirDecodeError::ForwardTypeReference {
                    owner: self.type_nodes.len() as u32,
                    reference,
                }
                .into()
            })
    }

    fn type_reference(
        &self,
        reader: &mut Reader<'_>,
    ) -> Result<RirTypeSyntaxRef, PackedRirAppendError<E>> {
        let reference = reader.u32()?;
        if reference >= self.types {
            return Err(PackedRirDecodeError::TypeReferenceOutOfBounds {
                reference,
                types: self.types,
            }
            .into());
        }
        self.type_nodes.get(reference as usize).copied().ok_or(
            PackedRirDecodeError::TypeReferenceOutOfBounds {
                reference,
                types: self.types,
            }
            .into(),
        )
    }

    fn optional_type_reference(
        &self,
        reader: &mut Reader<'_>,
    ) -> Result<Option<RirTypeSyntaxRef>, PackedRirAppendError<E>> {
        if reader.boolean("optional type-syntax reference")? {
            self.type_reference(reader).map(Some)
        } else {
            Ok(None)
        }
    }

    fn type_path(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<RirTypeSyntaxRange, PackedRirAppendError<E>> {
        let count = Self::count(reader, "type path", 1)?;
        if count == 0 {
            return Err(PackedRirDecodeError::CountOutOfBounds {
                family: "type path",
            }
            .into());
        }
        let mut words = Vec::new();
        words
            .try_reserve_exact(count)
            .map_err(|_| Self::capacity("type path"))?;
        for _ in 0..count {
            self.check()?;
            words.push(self.type_symbol(reader)?.as_u32());
        }
        self.destination
            .type_syntax
            .push_words(words)
            .map_err(|error| PackedRirAppendError::Build(type_syntax_build_error(error)))
    }

    fn type_children(
        &mut self,
        reader: &mut Reader<'_>,
        family: &'static str,
    ) -> Result<RirTypeSyntaxRange, PackedRirAppendError<E>> {
        let count = Self::count(reader, family, 1)?;
        let mut words = Vec::new();
        words
            .try_reserve_exact(count)
            .map_err(|_| Self::capacity(family))?;
        for _ in 0..count {
            self.check()?;
            words.push(self.type_child(reader)?.as_u32());
        }
        self.destination
            .type_syntax
            .push_words(words)
            .map_err(|error| PackedRirAppendError::Build(type_syntax_build_error(error)))
    }

    fn decode_types(&mut self, reader: &mut Reader<'_>) -> Result<(), PackedRirAppendError<E>> {
        self.type_nodes
            .try_reserve_exact(self.types as usize)
            .map_err(|_| Self::capacity("type syntax nodes"))?;
        for _ in 0..self.types {
            self.check()?;
            let node = match Self::byte_tag(reader, "type syntax node", 12)? {
                0 => RirTypeSyntaxNode::Named(self.type_symbol(reader)?),
                1 => RirTypeSyntaxNode::Qualified {
                    path: self.type_path(reader)?,
                },
                2 => RirTypeSyntaxNode::Unit,
                3 => RirTypeSyntaxNode::Never,
                4 => RirTypeSyntaxNode::Array {
                    element: self.type_child(reader)?,
                    length: self.type_child(reader)?,
                },
                5 => RirTypeSyntaxNode::Slice {
                    element: self.type_child(reader)?,
                },
                6 => {
                    let field_count = Self::count(reader, "anonymous type fields", 2)?;
                    let mut field_words = Vec::new();
                    field_words
                        .try_reserve_exact(field_count.saturating_mul(2))
                        .map_err(|_| Self::capacity("anonymous type fields"))?;
                    for _ in 0..field_count {
                        self.check()?;
                        field_words.push(self.type_symbol(reader)?.as_u32());
                        field_words.push(self.type_child(reader)?.as_u32());
                    }
                    let fields = self
                        .destination
                        .type_syntax
                        .push_words(field_words)
                        .map_err(|error| {
                            PackedRirAppendError::Build(type_syntax_build_error(error))
                        })?;

                    let method_count = Self::count(reader, "anonymous type methods", 6)?;
                    let mut method_words = Vec::new();
                    for _ in 0..method_count {
                        self.check()?;
                        method_words.push(self.type_symbol(reader)?.as_u32());
                        let has_receiver = reader.boolean("anonymous method receiver")?;
                        method_words.push(if has_receiver {
                            u32::from(Self::byte_tag(reader, "anonymous receiver mode", 3)?)
                        } else {
                            u32::MAX
                        });
                        method_words.push(u32::from(reader.boolean("anonymous borrow result")?));
                        let parameter_count = Self::count(reader, "anonymous parameters", 4)?;
                        method_words.push(u32::try_from(parameter_count).map_err(|_| {
                            PackedRirDecodeError::CountOutOfBounds {
                                family: "anonymous parameters",
                            }
                        })?);
                        for _ in 0..parameter_count {
                            self.check()?;
                            method_words.push(u32::from(Self::byte_tag(
                                reader,
                                "anonymous parameter mode",
                                3,
                            )?));
                            method_words.push(self.type_symbol(reader)?.as_u32());
                            method_words.push(self.type_child(reader)?.as_u32());
                        }
                        method_words.push(self.type_child(reader)?.as_u32());
                        let directive_count = Self::count(reader, "anonymous directives", 2)?;
                        method_words.push(u32::try_from(directive_count).map_err(|_| {
                            PackedRirDecodeError::CountOutOfBounds {
                                family: "anonymous directives",
                            }
                        })?);
                        for _ in 0..directive_count {
                            self.check()?;
                            method_words.push(self.type_symbol(reader)?.as_u32());
                            let argument_count =
                                Self::count(reader, "anonymous directive arguments", 1)?;
                            method_words.push(u32::try_from(argument_count).map_err(|_| {
                                PackedRirDecodeError::CountOutOfBounds {
                                    family: "anonymous directive arguments",
                                }
                            })?);
                            for _ in 0..argument_count {
                                self.check()?;
                                method_words.push(self.type_symbol(reader)?.as_u32());
                            }
                        }
                    }
                    let methods = self
                        .destination
                        .type_syntax
                        .push_words(method_words)
                        .map_err(|error| {
                            PackedRirAppendError::Build(type_syntax_build_error(error))
                        })?;
                    RirTypeSyntaxNode::AnonymousStruct { fields, methods }
                }
                7 => {
                    let variant_count = Self::count(reader, "anonymous type variants", 2)?;
                    let mut words = Vec::new();
                    for _ in 0..variant_count {
                        self.check()?;
                        words.push(self.type_symbol(reader)?.as_u32());
                        let payload_count =
                            Self::count(reader, "anonymous type variant payload", 1)?;
                        words.push(u32::try_from(payload_count).map_err(|_| {
                            PackedRirDecodeError::CountOutOfBounds {
                                family: "anonymous type variant payload",
                            }
                        })?);
                        for _ in 0..payload_count {
                            self.check()?;
                            words.push(self.type_child(reader)?.as_u32());
                        }
                    }
                    let variants =
                        self.destination
                            .type_syntax
                            .push_words(words)
                            .map_err(|error| {
                                PackedRirAppendError::Build(type_syntax_build_error(error))
                            })?;
                    RirTypeSyntaxNode::AnonymousEnum { variants }
                }
                8 => RirTypeSyntaxNode::PointerConst {
                    pointee: self.type_child(reader)?,
                },
                9 => RirTypeSyntaxNode::PointerMut {
                    pointee: self.type_child(reader)?,
                },
                10 => RirTypeSyntaxNode::TypeCall {
                    path: self.type_path(reader)?,
                    arguments: self.type_children(reader, "type arguments")?,
                },
                11 => RirTypeSyntaxNode::ValueCall {
                    name: self.type_symbol(reader)?,
                    arguments: self.type_children(reader, "value arguments")?,
                },
                12 => {
                    let bytes = reader.bytes(16)?;
                    RirTypeSyntaxNode::Integer(i128::from_le_bytes(
                        bytes.try_into().expect("sixteen-byte slice"),
                    ))
                }
                _ => unreachable!(),
            };
            let reference = self
                .destination
                .type_syntax
                .push_node(node)
                .map_err(|error| PackedRirAppendError::Build(type_syntax_build_error(error)))?;
            self.type_nodes.push(reference);
        }
        Ok(())
    }

    fn reference(&mut self, reader: &mut Reader<'_>) -> Result<InstRef, PackedRirAppendError<E>> {
        let value = reader.u32()?;
        if value >= self.instructions {
            return Err(PackedRirDecodeError::ReferenceOutOfBounds {
                reference: value,
                instructions: self.instructions,
            }
            .into());
        }
        if value >= self.source_instruction {
            return Err(PackedRirDecodeError::ForwardReference {
                instruction: self.source_instruction,
                reference: value,
            }
            .into());
        }
        self.destination_ref(value)
    }

    fn destination_ref(&self, source: u32) -> Result<InstRef, PackedRirAppendError<E>> {
        let destination = self
            .destination_instruction_start
            .checked_add(source)
            .ok_or(PackedRirDecodeError::DestinationInstructionMismatch)?;
        Ok(InstRef::from_raw(destination))
    }

    fn optional_ref(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<Option<InstRef>, PackedRirAppendError<E>> {
        if reader.boolean("optional instruction reference")? {
            self.reference(reader).map(Some)
        } else {
            Ok(None)
        }
    }

    fn refs(
        &mut self,
        reader: &mut Reader<'_>,
        family: &'static str,
    ) -> Result<Vec<InstRef>, PackedRirAppendError<E>> {
        let count = Self::count(reader, family, 1)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| Self::capacity(family))?;
        for _ in 0..count {
            self.check()?;
            values.push(self.reference(reader)?);
        }
        Ok(values)
    }

    fn call_args(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<Vec<RirCallArg>, PackedRirAppendError<E>> {
        let count = Self::count(reader, "call arguments", 2)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| Self::capacity("call arguments"))?;
        for _ in 0..count {
            self.check()?;
            values.push(RirCallArg {
                value: self.reference(reader)?,
                mode: decode_arg_mode(reader.byte()?)?,
            });
        }
        Ok(values)
    }

    fn fields(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<Vec<(Spur, RirTypeSyntaxRef)>, PackedRirAppendError<E>> {
        let count = Self::count(reader, "struct fields", 2)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| Self::capacity("struct fields"))?;
        for _ in 0..count {
            self.check()?;
            let name = self.symbol(reader)?;
            let ty = self.type_reference(reader)?;
            values.push((name, ty));
        }
        Ok(values)
    }

    fn field_inits(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<Vec<(Spur, InstRef)>, PackedRirAppendError<E>> {
        let count = Self::count(reader, "field initializers", 2)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| Self::capacity("field initializers"))?;
        for _ in 0..count {
            self.check()?;
            let name = self.symbol(reader)?;
            let value = self.reference(reader)?;
            values.push((name, value));
        }
        Ok(values)
    }

    fn directives(
        &mut self,
        reader: &mut Reader<'_>,
        basis: &mut Reader<'_>,
        mut field: impl FnMut(u32) -> RirSpanField,
    ) -> Result<Vec<RirDirective>, PackedRirAppendError<E>> {
        let count = Self::count(reader, "directives", 2)?;
        let mut values = Vec::new();
        values
            .try_reserve_exact(count)
            .map_err(|_| Self::capacity("directives"))?;
        for ordinal in 0..count {
            self.check()?;
            let name = self.symbol(reader)?;
            let argument_count = Self::count(reader, "directive arguments", 1)?;
            let mut args = Vec::new();
            args.try_reserve_exact(argument_count)
                .map_err(|_| Self::capacity("directive arguments"))?;
            for _ in 0..argument_count {
                self.check()?;
                args.push(self.symbol(reader)?);
            }
            let ordinal =
                u32::try_from(ordinal).map_err(|_| PackedRirDecodeError::CountOutOfBounds {
                    family: "directives",
                })?;
            let span = self.span(basis, field(ordinal))?;
            values.push(RirDirective { name, args, span });
        }
        Ok(values)
    }

    fn pattern(
        &mut self,
        reader: &mut Reader<'_>,
        span: Span,
    ) -> Result<RirPattern, PackedRirAppendError<E>> {
        Ok(match Self::byte_tag(reader, "match pattern", 3)? {
            0 => RirPattern::Wildcard(span),
            1 => RirPattern::Int {
                value: reader.u64()?,
                negative: reader.boolean("negative integer pattern")?,
                span,
            },
            2 => RirPattern::Bool(reader.boolean("boolean pattern")?, span),
            3 => {
                let module = self.optional_ref(reader)?;
                let ctor_head = self.optional_ref(reader)?;
                let type_name = self.symbol(reader)?;
                let variant = self.symbol(reader)?;
                let count = Self::count(reader, "pattern bindings", 1)?;
                let mut bindings = Vec::new();
                bindings
                    .try_reserve_exact(count)
                    .map_err(|_| Self::capacity("pattern bindings"))?;
                for _ in 0..count {
                    self.check()?;
                    bindings.push(self.symbol(reader)?);
                }
                RirPattern::Path {
                    module,
                    ctor_head,
                    type_name,
                    variant,
                    bindings,
                    span,
                }
            }
            _ => unreachable!(),
        })
    }

    fn enum_payload(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<(Vec<Spur>, Vec<Vec<RirTypeSyntaxRef>>), PackedRirAppendError<E>> {
        let count = Self::count(reader, "enum variants", 2)?;
        let mut variants = Vec::new();
        let mut payloads = Vec::new();
        variants
            .try_reserve_exact(count)
            .map_err(|_| Self::capacity("enum variants"))?;
        payloads
            .try_reserve_exact(count)
            .map_err(|_| Self::capacity("enum payloads"))?;
        for _ in 0..count {
            self.check()?;
            variants.push(self.symbol(reader)?);
            let payload_count = Self::count(reader, "enum payload", 1)?;
            let mut payload = Vec::new();
            payload
                .try_reserve_exact(payload_count)
                .map_err(|_| Self::capacity("enum payload"))?;
            for _ in 0..payload_count {
                self.check()?;
                payload.push(self.type_reference(reader)?);
            }
            payloads.push(payload);
        }
        Ok((variants, payloads))
    }

    fn instruction(
        &mut self,
        opcode: u8,
        span: Span,
        reader: &mut Reader<'_>,
        basis: &mut Reader<'_>,
    ) -> Result<InstRef, PackedRirAppendError<E>> {
        macro_rules! add {
            ($data:expr) => {
                self.destination.add_inst(Inst { data: $data, span })
            };
        }
        macro_rules! binary {
            ($variant:ident) => {{
                let lhs = self.reference(reader)?;
                let rhs = self.reference(reader)?;
                add!(InstData::$variant { lhs, rhs })
            }};
        }
        macro_rules! unary {
            ($variant:ident, $field:ident) => {{
                let $field = self.reference(reader)?;
                add!(InstData::$variant { $field })
            }};
        }
        let result = match opcode {
            0 => add!(InstData::IntConst(reader.u64()?)),
            1 => {
                let text = self.symbol(reader)?;
                add!(InstData::FloatConst { text })
            }
            2 => add!(InstData::BoolConst(reader.boolean("boolean constant")?)),
            3 => {
                let content = self.symbol(reader)?;
                let anchor = self.anchor(reader)?;
                add!(InstData::StringConst { content, anchor })
            }
            4 => add!(InstData::UnitConst),
            5 => binary!(Add),
            6 => binary!(Sub),
            7 => binary!(Mul),
            8 => binary!(Div),
            9 => binary!(Mod),
            10 => binary!(Eq),
            11 => binary!(Ne),
            12 => binary!(Lt),
            13 => binary!(Gt),
            14 => binary!(Le),
            15 => binary!(Ge),
            16 => binary!(And),
            17 => binary!(Or),
            18 => binary!(BitAnd),
            19 => binary!(BitOr),
            20 => binary!(BitXor),
            21 => binary!(Shl),
            22 => binary!(Shr),
            23 => unary!(Neg, operand),
            24 => unary!(Not, operand),
            25 => unary!(BitNot, operand),
            26 => unary!(Try, operand),
            27 => {
                let cond = self.reference(reader)?;
                let then_block = self.reference(reader)?;
                let else_block = self.optional_ref(reader)?;
                add!(InstData::Branch {
                    cond,
                    then_block,
                    else_block
                })
            }
            28 => {
                let cond = self.reference(reader)?;
                let body = self.reference(reader)?;
                add!(InstData::Loop { cond, body })
            }
            29 => {
                let body = self.reference(reader)?;
                let iter_borrow = self.optional_symbol(reader)?;
                add!(InstData::InfiniteLoop { body, iter_borrow })
            }
            30 => {
                let scrutinee = self.reference(reader)?;
                let count = Self::count(reader, "match arms", 2)?;
                let mut arms = Vec::new();
                arms.try_reserve_exact(count)
                    .map_err(|_| Self::capacity("match arms"))?;
                for ordinal in 0..count {
                    self.check()?;
                    let arm = u32::try_from(ordinal).map_err(|_| {
                        PackedRirDecodeError::CountOutOfBounds {
                            family: "match arms",
                        }
                    })?;
                    let pattern_span = self.span(basis, RirSpanField::MatchPattern { arm })?;
                    let pattern = self.pattern(reader, pattern_span)?;
                    let body = self.reference(reader)?;
                    arms.push((pattern, body));
                }
                self.destination.add_match(scrutinee, &arms, span)?
            }
            31 => {
                let value = self.optional_ref(reader)?;
                add!(InstData::Break { value })
            }
            32 => add!(InstData::Continue),
            33 => {
                let directives = self.directives(reader, basis, |directive| {
                    RirSpanField::FunctionDirective { directive }
                })?;
                let flags = reader.byte()?;
                let name = self.symbol(reader)?;
                let count = Self::count(reader, "parameters", 4)?;
                let mut params = Vec::new();
                params
                    .try_reserve_exact(count)
                    .map_err(|_| Self::capacity("parameters"))?;
                for ordinal in 0..count {
                    self.check()?;
                    let name = self.symbol(reader)?;
                    let ty = self.type_reference(reader)?;
                    let mode = decode_param_mode(reader.byte()?)?;
                    let is_comptime = reader.boolean("comptime parameter")?;
                    let parameter = u32::try_from(ordinal).map_err(|_| {
                        PackedRirDecodeError::CountOutOfBounds {
                            family: "parameters",
                        }
                    })?;
                    let parameter_span =
                        self.span(basis, RirSpanField::FunctionParameter { parameter })?;
                    params.push(RirParam {
                        name,
                        ty,
                        mode,
                        is_comptime,
                        span: parameter_span,
                    });
                }
                let return_type = self.type_reference(reader)?;
                let body = self.reference(reader)?;
                let self_mode = decode_param_mode(reader.byte()?)?;
                self.destination.add_fn_decl_with_return_modes(
                    &directives,
                    flags & 1 != 0,
                    flags & 2 != 0,
                    flags & 4 != 0,
                    flags & 8 != 0,
                    name,
                    &params,
                    return_type,
                    body,
                    flags & 16 != 0,
                    self_mode,
                    flags & 32 != 0,
                    flags & 64 != 0,
                    flags & 128 != 0,
                    span,
                )?
            }
            63 => {
                let place = self.reference(reader)?;
                let value = self.reference(reader)?;
                add!(InstData::PlaceSet { place, value })
            }
            34 => {
                let directives = self.directives(reader, basis, |directive| {
                    RirSpanField::ConstDirective { directive }
                })?;
                let is_pub = reader.boolean("constant visibility")?;
                let name = self.symbol(reader)?;
                let ty = self.optional_type_reference(reader)?;
                let init = self.reference(reader)?;
                self.destination
                    .add_const_decl(&directives, is_pub, name, ty, init, span)?
            }
            35 => {
                let name = self.symbol(reader)?;
                let args = self.call_args(reader)?;
                self.destination.add_call(name, &args, span)?
            }
            36 => {
                let name = self.symbol(reader)?;
                let args = self.refs(reader, "intrinsic arguments")?;
                self.destination.add_intrinsic(name, &args, span)?
            }
            37 => {
                let intrinsic = decode_internal_intrinsic(reader.byte()?)?;
                let args = self.refs(reader, "internal intrinsic arguments")?;
                self.destination
                    .add_internal_intrinsic(intrinsic, &args, span)?
            }
            38 => {
                let name = self.symbol(reader)?;
                let type_arg = self.type_reference(reader)?;
                add!(InstData::TypeIntrinsic { name, type_arg })
            }
            39 => {
                let type_arg = self.type_reference(reader)?;
                let field = self.symbol(reader)?;
                add!(InstData::OffsetOf { type_arg, field })
            }
            40 => {
                let value = self.optional_ref(reader)?;
                add!(InstData::Ret(value))
            }
            41 => {
                let value = self.reference(reader)?;
                add!(InstData::Yield(value))
            }
            42 => {
                let instructions = self.refs(reader, "block instructions")?;
                self.destination.add_block(&instructions, span)?
            }
            43 => {
                let directives = self.directives(reader, basis, |directive| {
                    RirSpanField::AllocDirective { directive }
                })?;
                let name = self.optional_symbol(reader)?;
                let is_mut = reader.boolean("mutable allocation")?;
                let ty = self.optional_type_reference(reader)?;
                let init = self.reference(reader)?;
                let iter_elem = reader.boolean("iteration element")?;
                self.destination
                    .add_alloc(&directives, name, is_mut, ty, init, iter_elem, span)?
            }
            44 => {
                let name = self.symbol(reader)?;
                let anchor = self.optional_anchor(reader)?;
                add!(InstData::VarRef { name, anchor })
            }
            45 => {
                let name = self.symbol(reader)?;
                let value = self.reference(reader)?;
                add!(InstData::Assign { name, value })
            }
            46 => {
                let directives = self.directives(reader, basis, |directive| {
                    RirSpanField::StructDirective { directive }
                })?;
                let flags = reader.byte()?;
                if flags & !3 != 0 {
                    return Err(PackedRirDecodeError::InvalidTag {
                        family: "struct flags",
                        tag: flags,
                    }
                    .into());
                }
                let name = self.symbol(reader)?;
                let fields = self.fields(reader)?;
                let encoded_methods = self.refs(reader, "struct methods")?;
                if self.source_instruction == self.instructions - 1
                    && self.root_methods.is_some()
                    && !encoded_methods.is_empty()
                {
                    return Err(PackedRirDecodeError::CountOutOfBounds {
                        family: "methodless struct root",
                    }
                    .into());
                }
                let methods = if self.source_instruction == self.instructions - 1 {
                    self.root_methods.unwrap_or(&encoded_methods)
                } else {
                    &encoded_methods
                };
                self.destination.add_struct_decl(
                    &directives,
                    flags & 1 != 0,
                    flags & 2 != 0,
                    name,
                    &fields,
                    methods,
                    span,
                )?
            }
            47 => {
                let module = self.optional_ref(reader)?;
                let ctor_head = self.optional_ref(reader)?;
                let type_name = self.symbol(reader)?;
                let fields = self.field_inits(reader)?;
                let shorthand_span = if reader.boolean("struct shorthand span")? {
                    Some(self.span(basis, RirSpanField::StructInitShorthand)?)
                } else {
                    None
                };
                self.destination.add_struct_init(
                    module,
                    ctor_head,
                    type_name,
                    &fields,
                    shorthand_span,
                    span,
                )?
            }
            48 => {
                let base = self.reference(reader)?;
                let field = self.symbol(reader)?;
                add!(InstData::FieldGet { base, field })
            }
            49 => {
                let base = self.reference(reader)?;
                let field = self.symbol(reader)?;
                let value = self.reference(reader)?;
                add!(InstData::FieldSet { base, field, value })
            }
            50 => {
                let is_pub = reader.boolean("enum visibility")?;
                let is_non_exhaustive = reader.boolean("enum non-exhaustive marker")?;
                let name = self.symbol(reader)?;
                let (variants, payloads) = self.enum_payload(reader)?;
                self.destination.add_enum_decl(
                    is_pub,
                    is_non_exhaustive,
                    name,
                    &variants,
                    &payloads,
                    span,
                )?
            }
            51 => {
                let module = self.optional_ref(reader)?;
                let type_name = self.symbol(reader)?;
                let variant = self.symbol(reader)?;
                add!(InstData::EnumVariant {
                    module,
                    type_name,
                    variant
                })
            }
            52 => {
                let elements = self.refs(reader, "array elements")?;
                self.destination.add_array_init(&elements, span)?
            }
            53 => {
                let value = self.reference(reader)?;
                let count = match Self::byte_tag(reader, "array repeat count", 1)? {
                    0 => RepeatCount::Literal(reader.u64()?),
                    1 => RepeatCount::Named(self.symbol(reader)?),
                    _ => unreachable!(),
                };
                add!(InstData::ArrayRepeat { value, count })
            }
            54 => {
                let base = self.reference(reader)?;
                let index = self.reference(reader)?;
                add!(InstData::IndexGet { base, index })
            }
            55 => {
                let base = self.reference(reader)?;
                let index = self.reference(reader)?;
                let value = self.reference(reader)?;
                add!(InstData::IndexSet { base, index, value })
            }
            56 => {
                let receiver = self.reference(reader)?;
                let method = self.symbol(reader)?;
                let args = self.call_args(reader)?;
                self.destination
                    .add_method_call(receiver, method, &args, span)?
            }
            57 => {
                let type_name = self.symbol(reader)?;
                let body = self.reference(reader)?;
                add!(InstData::DropFnDecl { type_name, body })
            }
            58 => unary!(Comptime, expr),
            59 => unary!(Checked, expr),
            60 => {
                let type_name = self.type_reference(reader)?;
                add!(InstData::TypeConst { type_name })
            }
            61 => {
                let fields = self.fields(reader)?;
                let methods = self.refs(reader, "anonymous struct methods")?;
                let anchor = self.anchor(reader)?;
                self.destination
                    .add_anon_struct_type(&fields, &methods, anchor, span)?
            }
            62 => {
                let (variants, payloads) = self.enum_payload(reader)?;
                let anchor = self.anchor(reader)?;
                self.destination
                    .add_anon_enum_type(&variants, &payloads, anchor, span)?
            }
            tag => return Err(PackedRirDecodeError::InvalidOpcode(tag).into()),
        };
        if let Some(error) = self.destination.capacity_error() {
            return Err(PackedRirAppendError::Build(error));
        }
        Ok(result)
    }

    fn symbol(&mut self, reader: &mut Reader<'_>) -> Result<Spur, PackedRirAppendError<E>> {
        let value = reader.u32()?;
        if value >= self.symbols {
            return Err(PackedRirDecodeError::SymbolOutOfBounds {
                symbol: value,
                symbols: self.symbols,
            }
            .into());
        }
        (self.remap_symbol)(value).map_err(PackedRirAppendError::SymbolRemap)
    }

    fn optional_symbol(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<Option<Spur>, PackedRirAppendError<E>> {
        if reader.boolean("optional symbol")? {
            self.symbol(reader).map(Some)
        } else {
            Ok(None)
        }
    }

    fn span(
        &mut self,
        basis: &mut Reader<'_>,
        field: RirSpanField,
    ) -> Result<Span, PackedRirAppendError<E>> {
        self.check()?;
        let start = basis.u32()?;
        let end = basis.u32()?;
        if start > end {
            return Err(PackedRirDecodeError::InvalidBasisSpan { start, end }.into());
        }
        let slot = RirSpanSlot::new(
            InstRef::from_raw(
                self.destination_instruction_start
                    .checked_add(self.source_instruction)
                    .ok_or(PackedRirDecodeError::DestinationInstructionMismatch)?,
            ),
            field,
        );
        self.spans_read =
            self.spans_read
                .checked_add(1)
                .ok_or(PackedRirDecodeError::CountOutOfBounds {
                    family: "span slots",
                })?;
        (self.remap_span)(slot, (start, end))
            .map_err(|error| PackedRirAppendError::SpanRemap { slot, error })
    }

    fn byte_tag(
        reader: &mut Reader<'_>,
        family: &'static str,
        max: u8,
    ) -> Result<u8, PackedRirDecodeError> {
        let tag = reader.byte()?;
        if tag > max {
            Err(PackedRirDecodeError::InvalidTag { family, tag })
        } else {
            Ok(tag)
        }
    }

    fn count(
        reader: &mut Reader<'_>,
        family: &'static str,
        minimum_bytes: usize,
    ) -> Result<usize, PackedRirDecodeError> {
        let count = reader.u32()? as usize;
        if count > MAX_RIR_ENTRIES_PER_PROGRAM as usize
            || count
                .checked_mul(minimum_bytes)
                .is_none_or(|minimum| minimum > reader.remaining())
        {
            return Err(PackedRirDecodeError::CountOutOfBounds { family });
        }
        Ok(count)
    }

    fn anchor(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<RirStructuralAnchor, PackedRirAppendError<E>> {
        let count = Self::count(reader, "structural anchor", 1)?;
        let mut segments = Vec::new();
        segments
            .try_reserve_exact(count)
            .map_err(|_| Self::capacity("structural anchor"))?;
        for _ in 0..count {
            self.check()?;
            let tag = Self::byte_tag(reader, "structural anchor segment", 12)?;
            let segment = match tag {
                0 => RirStructuralPathSegment::Body,
                1 => RirStructuralPathSegment::ParameterType(reader.u32()?),
                2 => RirStructuralPathSegment::ReturnType,
                3 => RirStructuralPathSegment::Statement(reader.u32()?),
                4 => RirStructuralPathSegment::Operand(reader.u32()?),
                5 => RirStructuralPathSegment::Branch(reader.u32()?),
                6 => RirStructuralPathSegment::MatchArm(reader.u32()?),
                7 => RirStructuralPathSegment::FieldType(reader.u32()?),
                8 => RirStructuralPathSegment::VariantPayload {
                    variant: reader.u32()?,
                    payload: reader.u32()?,
                },
                9 => RirStructuralPathSegment::Method(reader.u32()?),
                10 => RirStructuralPathSegment::AnonymousType(reader.u32()?),
                11 => RirStructuralPathSegment::StringLiteral(reader.u32()?),
                12 => RirStructuralPathSegment::ReadOnlyData(reader.u32()?),
                _ => unreachable!(),
            };
            segments.push(segment);
        }
        Ok(RirStructuralAnchor::new(segments))
    }

    fn optional_anchor(
        &mut self,
        reader: &mut Reader<'_>,
    ) -> Result<Option<RirStructuralAnchor>, PackedRirAppendError<E>> {
        if reader.boolean("optional structural anchor")? {
            self.anchor(reader).map(Some)
        } else {
            Ok(None)
        }
    }
}

const fn encode_param_mode(mode: RirParamMode) -> u8 {
    match mode {
        RirParamMode::Normal => 0,
        RirParamMode::Inout => 1,
        RirParamMode::Borrow => 2,
    }
}

fn decode_param_mode(tag: u8) -> Result<RirParamMode, PackedRirDecodeError> {
    match tag {
        0 => Ok(RirParamMode::Normal),
        1 => Ok(RirParamMode::Inout),
        2 => Ok(RirParamMode::Borrow),
        tag => Err(PackedRirDecodeError::InvalidTag {
            family: "parameter mode",
            tag,
        }),
    }
}

const fn encode_arg_mode(mode: RirArgMode) -> u8 {
    match mode {
        RirArgMode::Normal => 0,
        RirArgMode::Inout => 1,
        RirArgMode::Borrow => 2,
    }
}

fn decode_arg_mode(tag: u8) -> Result<RirArgMode, PackedRirDecodeError> {
    match tag {
        0 => Ok(RirArgMode::Normal),
        1 => Ok(RirArgMode::Inout),
        2 => Ok(RirArgMode::Borrow),
        tag => Err(PackedRirDecodeError::InvalidTag {
            family: "argument mode",
            tag,
        }),
    }
}

const fn encode_internal_intrinsic(intrinsic: InternalIntrinsic) -> u8 {
    match intrinsic {
        InternalIntrinsic::IterLen => 0,
        InternalIntrinsic::CharScalar => 1,
        InternalIntrinsic::CharNext => 2,
        InternalIntrinsic::CharScalarLossy => 3,
        InternalIntrinsic::CharNextLossy => 4,
    }
}

fn decode_internal_intrinsic(tag: u8) -> Result<InternalIntrinsic, PackedRirDecodeError> {
    match tag {
        0 => Ok(InternalIntrinsic::IterLen),
        1 => Ok(InternalIntrinsic::CharScalar),
        2 => Ok(InternalIntrinsic::CharNext),
        3 => Ok(InternalIntrinsic::CharScalarLossy),
        4 => Ok(InternalIntrinsic::CharNextLossy),
        tag => Err(PackedRirDecodeError::InvalidTag {
            family: "internal intrinsic",
            tag,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_lexer::Lexer;
    use rue_parser::Parser;
    use rue_parser::ast::{Expr, Item};
    use rue_span::FileId;

    fn validated_owner(orphan_prefix: bool) -> (ValidatedRir, ThreadedRodeo, InstRef) {
        let symbols = ThreadedRodeo::new();
        let name = symbols.get_or_intern("unused");
        let mut editor = RirEditor::new();
        let value = editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(1, 2),
        });
        if orphan_prefix {
            editor.rir.extra.push(0xfeed_beef);
        }
        let block = editor.add_block(&[value], Span::new(0, 3)).unwrap();
        let root = editor
            .add_const_decl(&[], false, name, None, block, Span::new(0, 3))
            .unwrap();
        let context = RirValidationContext {
            symbol_count: symbols.len(),
            source_lengths: &[(FileId::DEFAULT, 3)],
        };
        (
            ValidatedRir::finish(editor, &context).unwrap(),
            symbols,
            root,
        )
    }

    fn named_type(editor: &mut RirEditor, symbol: Spur) -> RirTypeSyntaxRef {
        editor
            .add_named_type(symbol)
            .expect("test type arena must remain representable")
    }

    fn parsed_signature_owner(source: &str) -> (ValidatedRir, ThreadedRodeo, InstRef) {
        let (tokens, symbols) = Lexer::new(source).tokenize().unwrap();
        let (ast, symbols) = Parser::new(tokens, symbols).parse().unwrap();
        let Item::Function(root_function) = &ast.items[0] else {
            panic!("packed type fixture must parse as a function")
        };
        let mut editor = RirEditor::new();
        let mut values = Vec::new();
        for item in &ast.items {
            let Item::Function(function) = item else {
                continue;
            };
            for ty in function
                .params
                .iter()
                .map(|parameter| &parameter.ty)
                .chain(function.return_type.iter())
            {
                let reference = editor.add_parser_type(ty, std::convert::identity).unwrap();
                values.push(editor.add_inst(Inst {
                    data: InstData::TypeConst {
                        type_name: reference,
                    },
                    span: ty.span(),
                }));
            }
            if let Expr::Block(block) = &function.body
                && let Expr::TypeLit(type_literal) = block.expr.as_ref()
            {
                let reference = editor
                    .add_parser_type(&type_literal.type_expr, std::convert::identity)
                    .unwrap();
                values.push(editor.add_inst(Inst {
                    data: InstData::TypeConst {
                        type_name: reference,
                    },
                    span: type_literal.span,
                }));
            }
        }
        let body = editor.add_block(&values, root_function.span).unwrap();
        let root = editor
            .add_const_decl(
                &[],
                false,
                root_function.name.name,
                None,
                body,
                root_function.span,
            )
            .unwrap();
        let source_length = u32::try_from(source.len()).unwrap();
        let rir = ValidatedRir::finish(
            editor,
            &RirValidationContext {
                symbol_count: symbols.len(),
                source_lengths: &[(FileId::DEFAULT, source_length)],
            },
        )
        .unwrap();
        (rir, symbols, root)
    }

    fn every_structured_type_owner() -> (ValidatedRir, ThreadedRodeo, InstRef) {
        parsed_signature_owner(
            "fn types(\
                named: Name,\
                qualified: lib.Name,\
                unit: (),\
                never: !,\
                array: [Widget; fact(N)],\
                literal_array: [u8; 4],\
                slice: [u8],\
                const_ptr: ptr const i32,\
                mut_ptr: ptr mut i32,\
                call: Result([Widget; fact(N)], lib.Option(Str(8)))\
            ) {}\
            fn make_struct() -> type {\
                struct {\
                    value: i32,\
                    fn get(borrow self, comptime index: u8) -> ptr const i32 { 0 }\
                }\
            }\
            fn make_enum() -> type { enum { First(i32), Second } }",
        )
    }

    fn pack(rir: &ValidatedRir, symbols: &ThreadedRodeo, root: InstRef) -> PackedValidatedRir {
        rir.try_pack_candidate(
            symbols,
            PackedRirMetadata {
                declaration: root,
                method_owner: None,
            },
            || Ok::<_, ()>(()),
            |_slot, span| Ok((span.start, span.end)),
        )
        .unwrap()
    }

    fn pack_intrinsics(names: &[&str]) -> PackedValidatedRir {
        let symbols = ThreadedRodeo::new();
        let declaration_name = symbols.get_or_intern("probe");
        let mut editor = RirEditor::new();
        let mut instructions = Vec::new();
        for name in names {
            instructions.push(
                editor
                    .add_intrinsic(symbols.get_or_intern(name), &[], Span::new(1, 2))
                    .unwrap(),
            );
        }
        let block = editor.add_block(&instructions, Span::new(0, 3)).unwrap();
        let root = editor
            .add_const_decl(&[], false, declaration_name, None, block, Span::new(0, 3))
            .unwrap();
        let rir = ValidatedRir::finish(
            editor,
            &RirValidationContext {
                symbol_count: symbols.len(),
                source_lengths: &[(FileId::DEFAULT, 3)],
            },
        )
        .unwrap();
        pack(&rir, &symbols, root)
    }

    fn append(
        packed: &PackedValidatedRir,
        destination: &mut RirEditor,
    ) -> Result<PackedRirAppend, PackedRirAppendError<()>> {
        packed.try_append_remapped(
            destination,
            || Ok(()),
            |ordinal| Ok(Spur::try_from_usize(ordinal as usize).unwrap()),
            |_slot, (start, end)| Ok(Span::new(start, end)),
        )
    }

    fn decode_error(packed: PackedValidatedRir) -> PackedRirDecodeError {
        let mut destination = RirEditor::new();
        match append(&packed, &mut destination).unwrap_err() {
            PackedRirAppendError::Decode(error) => error,
            error => panic!("expected packed decode error, got {error:?}"),
        }
    }

    fn set_header(bytes: &mut [u8], offset: usize, value: usize) {
        bytes[offset..offset + 4].copy_from_slice(&(value as u32).to_le_bytes());
    }

    #[test]
    fn roundtrip_is_exact_and_reencode_is_deterministic() {
        let (source, symbols, root) = validated_owner(false);
        let packed = pack(&source, &symbols, root);
        assert_eq!(packed.symbols().collect::<Vec<_>>(), ["unused"]);
        let mut destination = RirEditor::new();
        let appended = append(&packed, &mut destination).unwrap();
        assert_eq!(appended.metadata.declaration, root);
        let context = RirValidationContext {
            symbol_count: symbols.len(),
            source_lengths: &[(FileId::DEFAULT, 3)],
        };
        let decoded = ValidatedRir::finish(destination, &context).unwrap();
        assert!(source.exact_eq(&decoded));
        assert_eq!(pack(&decoded, &symbols, root).as_bytes(), packed.as_bytes());
    }

    #[test]
    fn every_structured_type_node_roundtrips_and_repacks_exactly() {
        let (source, symbols, root) = every_structured_type_owner();
        let variants = source
            .type_syntax()
            .nodes()
            .iter()
            .map(std::mem::discriminant)
            .collect::<ahash::AHashSet<_>>();
        assert_eq!(variants.len(), 13, "fixture must cover every type node");

        let packed = pack(&source, &symbols, root);
        let (decoded, metadata) = packed
            .try_decode_validated(
                PackedRirProjection {
                    symbol_count: symbols.len(),
                    file_id: FileId::DEFAULT,
                    declaration_start: 0,
                    source_length: u32::MAX,
                },
                || Ok::<_, ()>(()),
            )
            .unwrap();
        assert_eq!(metadata.declaration, root);
        assert_eq!(source.0.instructions, decoded.0.instructions);
        assert_eq!(source.0.extra, decoded.0.extra);
        let render = |rir: &ValidatedRir| {
            (0..rir.type_syntax().nodes().len())
                .map(|ordinal| {
                    rir.type_syntax()
                        .render_type_with(RirTypeSyntaxRef::from_u32(ordinal as u32), |symbol| {
                            symbols.resolve(symbol)
                        })
                        .unwrap()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(render(&source), render(&decoded));
        assert_eq!(pack(&decoded, &symbols, root).as_bytes(), packed.as_bytes());
    }

    /// ADR-0076 dense-space integrity.
    ///
    /// The packed envelope always speaks the body-private dense ordinal space.
    /// Decoding it through a body's dense remap into a *shared* equality space
    /// — one already carrying unrelated strings, so no handle equals its
    /// ordinal — produces the same program: the two decodes render identically,
    /// and the dense decode still repacks to the original bytes. The dense
    /// space is the encoding; the equality space the body analyzes in is not.
    #[test]
    fn a_shared_space_decode_renders_and_repacks_exactly_like_the_dense_one() {
        let (source, dense_symbols, root) = every_structured_type_owner();
        let packed = pack(&source, &dense_symbols, root);
        let projection = PackedRirProjection {
            symbol_count: packed.symbol_count(),
            file_id: FileId::DEFAULT,
            declaration_start: 0,
            source_length: u32::MAX,
        };

        let shared: ThreadedRodeo = ThreadedRodeo::new();
        for occupied in ["@revision", "@peer-body", "@another-body-name"] {
            shared.get_or_intern(occupied);
        }
        let dense_remap = packed
            .symbols()
            .map(|spelling| shared.get_or_intern(spelling))
            .collect::<Vec<_>>();
        assert_eq!(dense_remap.len(), packed.symbol_count());
        assert!(
            dense_remap
                .iter()
                .enumerate()
                .all(|(ordinal, symbol)| symbol.into_usize() != ordinal),
            "the fixture must exercise handles that are not their own ordinals"
        );

        let (dense_decoded, dense_metadata) = packed
            .try_decode_validated(projection, || Ok::<_, ()>(()))
            .unwrap();
        let (shared_decoded, shared_metadata) = packed
            .try_decode_validated_remapped(
                projection,
                false,
                || Ok::<_, ()>(()),
                |ordinal| dense_remap.get(ordinal as usize).copied(),
            )
            .unwrap();

        assert_eq!(dense_metadata.declaration, shared_metadata.declaration);
        assert_eq!(dense_metadata.declaration, root);
        assert_eq!(dense_decoded.0.extra, shared_decoded.0.extra);
        assert_eq!(
            RirPrinter::new(&dense_decoded, &dense_symbols).to_string(),
            RirPrinter::new(&shared_decoded, &shared).to_string(),
            "the equality space a body decodes into does not change the program"
        );
        assert_eq!(
            pack(&dense_decoded, &dense_symbols, root).as_bytes(),
            packed.as_bytes(),
            "the dense encoding space round-trips to the same bytes"
        );
    }

    /// An ordinal outside the body's dense remap fails the decode closed rather
    /// than silently naming another body's symbol in the shared space.
    #[test]
    fn a_dense_ordinal_outside_the_remap_fails_the_decode() {
        let (source, dense_symbols, root) = every_structured_type_owner();
        let packed = pack(&source, &dense_symbols, root);
        let error = packed
            .try_decode_validated_remapped(
                PackedRirProjection {
                    symbol_count: packed.symbol_count(),
                    file_id: FileId::DEFAULT,
                    declaration_start: 0,
                    source_length: u32::MAX,
                },
                false,
                || Ok::<_, ()>(()),
                |_| None,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            PackedRirAppendError::Decode(PackedRirDecodeError::CountOutOfBounds {
                family: "projected symbol ordinal"
            })
        ));
    }

    #[test]
    fn structured_type_corruption_fails_closed() {
        let (source, symbols, root) = parsed_signature_owner("fn types(value: [i32]) {}");
        let packed = pack(&source, &symbols, root);
        let header = Header::parse(packed.as_bytes()).unwrap();
        assert_eq!(header.types, 2);

        let mut bad_tag = packed.as_bytes().to_vec();
        bad_tag[header.types_offset] = 13;
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(bad_tag))),
            PackedRirDecodeError::InvalidTag {
                family: "type syntax node",
                tag: 13,
            }
        );

        let mut bad_symbol = packed.as_bytes().to_vec();
        bad_symbol[header.types_offset + 1] = u8::try_from(header.symbols).unwrap();
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(bad_symbol))),
            PackedRirDecodeError::SymbolOutOfBounds {
                symbol: header.symbols,
                symbols: header.symbols,
            }
        );

        let mut forward_child = packed.as_bytes().to_vec();
        forward_child[header.instructions_offset - 1] = 1;
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(forward_child))),
            PackedRirDecodeError::ForwardTypeReference {
                owner: 1,
                reference: 1,
            }
        );

        let mut excessive_count = packed.as_bytes().to_vec();
        set_header(&mut excessive_count, 16, 255);
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(excessive_count))),
            PackedRirDecodeError::CountOutOfBounds {
                family: "header counts",
            }
        );

        let mut truncated = packed.as_bytes().to_vec();
        truncated.remove(header.instructions_offset - 1);
        set_header(&mut truncated, 44, header.instructions_offset - 1);
        set_header(&mut truncated, 48, header.basis_offset - 1);
        set_header(&mut truncated, 52, header.end_offset - 1);
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(truncated))),
            PackedRirDecodeError::Truncated
        );

        let mut bad_instruction_ref = packed.as_bytes().to_vec();
        bad_instruction_ref[header.instructions_offset + 1] = 2;
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(bad_instruction_ref))),
            PackedRirDecodeError::TypeReferenceOutOfBounds {
                reference: 2,
                types: 2,
            }
        );
    }

    #[test]
    fn large_structured_type_pack_and_decode_are_cancellable_and_retryable() {
        let symbols = ThreadedRodeo::new();
        let name = symbols.get_or_intern("Repeated");
        let mut editor = RirEditor::new();
        let mut last = None;
        for _ in 0..2_000 {
            last = Some(named_type(&mut editor, name));
        }
        let value = editor.add_inst(Inst {
            data: InstData::TypeConst {
                type_name: last.unwrap(),
            },
            span: Span::new(0, 1),
        });
        let root = editor
            .add_const_decl(&[], false, name, None, value, Span::new(0, 1))
            .unwrap();
        let source = ValidatedRir::finish(
            editor,
            &RirValidationContext {
                symbol_count: symbols.len(),
                source_lengths: &[(FileId::DEFAULT, 1)],
            },
        )
        .unwrap();

        let mut encode_checks = 0usize;
        let canceled = source.try_pack_candidate(
            &symbols,
            PackedRirMetadata {
                declaration: root,
                method_owner: None,
            },
            || {
                encode_checks += 1;
                if encode_checks == 1_000 {
                    Err(())
                } else {
                    Ok(())
                }
            },
            |_slot, span| Ok((span.start, span.end)),
        );
        assert!(matches!(
            canceled,
            Err(PackedRirEncodeError::Checkpoint(()))
        ));

        let packed = pack(&source, &symbols, root);
        let projection = PackedRirProjection {
            symbol_count: symbols.len(),
            file_id: FileId::DEFAULT,
            declaration_start: 0,
            source_length: 1,
        };
        let mut decode_checks = 0usize;
        let canceled = packed.try_decode_validated(projection, || {
            decode_checks += 1;
            if decode_checks == 1_000 {
                Err(())
            } else {
                Ok(())
            }
        });
        assert!(matches!(
            canceled,
            Err(PackedRirAppendError::Checkpoint(()))
        ));

        let (decoded, metadata) = packed
            .try_decode_validated(projection, || Ok::<_, ()>(()))
            .unwrap();
        assert_eq!(metadata.declaration, root);
        assert!(source.exact_eq(&decoded));
    }

    #[test]
    fn direct_validated_decode_matches_the_generic_finish_boundary() {
        let (source, symbols, root) = validated_owner(false);
        let packed = pack(&source, &symbols, root);
        assert_eq!(packed.symbol_count(), symbols.len());
        let context = RirValidationContext {
            symbol_count: symbols.len(),
            source_lengths: &[(FileId::DEFAULT, 3)],
        };
        let (decoded, metadata) = packed
            .try_decode_validated(
                PackedRirProjection {
                    symbol_count: context.symbol_count,
                    file_id: FileId::DEFAULT,
                    declaration_start: 0,
                    source_length: 3,
                },
                || Ok::<_, ()>(()),
            )
            .unwrap();
        assert_eq!(metadata.declaration, root);
        assert!(source.exact_eq(&decoded));
        assert_eq!(pack(&decoded, &symbols, root).as_bytes(), packed.as_bytes());
    }

    #[test]
    fn direct_validated_decode_rejects_mapped_context_mismatches() {
        let (source, symbols, root) = validated_owner(false);
        let packed = pack(&source, &symbols, root);
        let invalid_symbol = packed.try_decode_validated(
            PackedRirProjection {
                symbol_count: symbols.len() + 1,
                file_id: FileId::DEFAULT,
                declaration_start: 0,
                source_length: 3,
            },
            || Ok::<_, ()>(()),
        );
        assert!(matches!(
            invalid_symbol,
            Err(PackedRirAppendError::Decode(
                PackedRirDecodeError::CountOutOfBounds {
                    family: "projected symbol universe"
                }
            ))
        ));

        let invalid_span = packed.try_decode_validated(
            PackedRirProjection {
                symbol_count: symbols.len(),
                file_id: FileId::new(7),
                declaration_start: 10,
                source_length: 3,
            },
            || Ok::<_, ()>(()),
        );
        assert!(matches!(
            invalid_span,
            Err(PackedRirAppendError::Decode(
                PackedRirDecodeError::CountOutOfBounds {
                    family: "projected span range"
                }
            ))
        ));
    }

    #[test]
    fn logical_payload_ignores_orphan_prefix_and_raw_range_start() {
        let (dense, dense_symbols, dense_root) = validated_owner(false);
        let (holey, holey_symbols, holey_root) = validated_owner(true);
        assert!(!dense.exact_eq(&holey));
        assert_eq!(
            pack(&dense, &dense_symbols, dense_root).as_bytes(),
            pack(&holey, &holey_symbols, holey_root).as_bytes()
        );
    }

    #[test]
    fn wrong_version_is_rejected_without_destination_mutation() {
        let (source, symbols, root) = validated_owner(false);
        let packed = pack(&source, &symbols, root);
        let mut bytes = packed.as_bytes().to_vec();
        bytes[4] = VERSION + 1;
        let corrupt = PackedValidatedRir(Arc::from(bytes));
        let mut destination = RirEditor::new();
        destination.add_inst(Inst {
            data: InstData::IntConst(9),
            span: Span::new(4, 5),
        });
        destination.rir.extra.push(17);
        destination.rir.instruction_limit_exceeded = true;
        let before_instructions = format!("{:?}", destination.rir.instructions);
        let before_extra = destination.rir.extra.clone();
        assert!(matches!(
            append(&corrupt, &mut destination),
            Err(PackedRirAppendError::Decode(
                PackedRirDecodeError::UnsupportedVersion(_)
            ))
        ));
        assert_eq!(
            format!("{:?}", destination.rir.instructions),
            before_instructions
        );
        assert_eq!(destination.rir.extra, before_extra);
        assert!(destination.rir.instruction_limit_exceeded);
    }

    #[test]
    fn fallible_intrinsic_set_uses_all_five_stable_header_bits() {
        let packed = pack_intrinsics(&[
            "parse_i32",
            "parse_i64",
            "parse_u32",
            "parse_u64",
            "read_line",
            "to_string",
        ]);
        assert_eq!(packed.as_bytes()[5], 0b1_1111);
        assert_eq!(
            packed.fallible_intrinsics().iter().collect::<Vec<_>>(),
            RirFallibleIntrinsic::ALL,
        );
    }

    #[test]
    fn intrinsic_name_changes_packed_identity_and_typed_set() {
        let i32 = pack_intrinsics(&["parse_i32"]);
        let i64 = pack_intrinsics(&["parse_i64"]);
        assert_ne!(i32.as_bytes(), i64.as_bytes());
        assert!(
            i32.fallible_intrinsics()
                .contains(RirFallibleIntrinsic::ParseI32)
        );
        assert!(
            !i32.fallible_intrinsics()
                .contains(RirFallibleIntrinsic::ParseI64)
        );
        assert!(
            i64.fallible_intrinsics()
                .contains(RirFallibleIntrinsic::ParseI64)
        );
        assert!(
            !i64.fallible_intrinsics()
                .contains(RirFallibleIntrinsic::ParseI32)
        );
    }

    #[test]
    fn reserved_fallible_intrinsic_header_bits_are_rejected() {
        let packed = pack_intrinsics(&["parse_i32"]);
        let mut bytes = packed.as_bytes().to_vec();
        bytes[5] |= 1 << 5;
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(bytes))),
            PackedRirDecodeError::InvalidTag {
                family: "fallible intrinsic set",
                tag: 0b10_0001,
            }
        );
    }

    #[test]
    fn cancellation_rolls_back_nonempty_destination_and_can_retry() {
        let (source, symbols, root) = validated_owner(false);
        let packed = pack(&source, &symbols, root);
        let mut destination = RirEditor::new();
        destination.add_inst(Inst {
            data: InstData::IntConst(9),
            span: Span::new(4, 5),
        });
        destination.rir.extra.push(17);
        let before_instructions = format!("{:?}", destination.rir.instructions);
        let before_extra = destination.rir.extra.clone();
        let mut checkpoints = 0;
        let result = packed.try_append_remapped(
            &mut destination,
            || {
                checkpoints += 1;
                if checkpoints == 5 { Err(()) } else { Ok(()) }
            },
            |ordinal| Ok(Spur::try_from_usize(ordinal as usize).unwrap()),
            |_slot, (start, end)| Ok(Span::new(start, end)),
        );
        assert!(matches!(result, Err(PackedRirAppendError::Checkpoint(()))));
        assert_eq!(
            format!("{:?}", destination.rir.instructions),
            before_instructions
        );
        assert_eq!(destination.rir.extra, before_extra);
        assert!(!destination.rir.instruction_limit_exceeded);

        let appended = append(&packed, &mut destination).unwrap();
        assert_eq!(appended.range.instructions, 1..4);
        assert_eq!(appended.metadata.declaration, InstRef::from_raw(3));
    }

    #[test]
    fn corrupt_instruction_encodings_fail_closed() {
        let (source, symbols, root) = validated_owner(false);
        let packed = pack(&source, &symbols, root);
        let header = Header::parse(packed.as_bytes()).unwrap();

        let mut bad_opcode = packed.as_bytes().to_vec();
        bad_opcode[header.instructions_offset] = 255;
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(bad_opcode))),
            PackedRirDecodeError::InvalidOpcode(255)
        );

        let mut forward_ref = packed.as_bytes().to_vec();
        forward_ref[header.instructions_offset + 3] = 1;
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(forward_ref))),
            PackedRirDecodeError::ForwardReference {
                instruction: 1,
                reference: 1,
            }
        );

        let mut out_of_bounds_ref = packed.as_bytes().to_vec();
        out_of_bounds_ref[header.instructions_offset + 3] = 127;
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(out_of_bounds_ref))),
            PackedRirDecodeError::ReferenceOutOfBounds {
                reference: 127,
                instructions: 3,
            }
        );

        let mut bad_count = packed.as_bytes().to_vec();
        bad_count[header.instructions_offset + 2] = 127;
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(bad_count))),
            PackedRirDecodeError::CountOutOfBounds {
                family: "block instructions",
            }
        );

        let mut nonminimal = packed.as_bytes().to_vec();
        nonminimal[header.instructions_offset + 2] = 0x81;
        nonminimal.insert(header.instructions_offset + 3, 0);
        set_header(&mut nonminimal, 48, header.basis_offset + 1);
        set_header(&mut nonminimal, 52, header.end_offset + 1);
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(nonminimal))),
            PackedRirDecodeError::NonMinimalVarint
        );

        let mut truncated = packed.as_bytes().to_vec();
        truncated.remove(header.basis_offset - 1);
        set_header(&mut truncated, 48, header.basis_offset - 1);
        set_header(&mut truncated, 52, header.end_offset - 1);
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(truncated))),
            PackedRirDecodeError::Truncated
        );
    }

    #[test]
    fn dense_symbol_corruption_and_scaling_are_bounded() {
        let (source, symbols, root) = validated_owner(false);
        let packed = pack(&source, &symbols, root);
        let header = Header::parse(packed.as_bytes()).unwrap();
        let spelling = packed.as_bytes()[header.symbols_offset..header.types_offset].to_vec();
        let mut duplicate = packed.as_bytes().to_vec();
        duplicate.splice(header.types_offset..header.types_offset, spelling.clone());
        set_header(&mut duplicate, 12, 2);
        set_header(&mut duplicate, 40, header.types_offset + spelling.len());
        set_header(
            &mut duplicate,
            44,
            header.instructions_offset + spelling.len(),
        );
        set_header(&mut duplicate, 48, header.basis_offset + spelling.len());
        set_header(&mut duplicate, 52, header.end_offset + spelling.len());
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(duplicate))),
            PackedRirDecodeError::DuplicateSymbol {
                first: 0,
                duplicate: 1,
            }
        );

        let many = ThreadedRodeo::new();
        for ordinal in 0..2_000 {
            many.get_or_intern(format!("symbol_{ordinal}"));
        }
        let packed = pack(&source, &many, root);
        let mut destination = RirEditor::new();
        let mut checkpoints = 0usize;
        packed
            .try_append_remapped(
                &mut destination,
                || {
                    checkpoints += 1;
                    Ok::<_, ()>(())
                },
                |ordinal| Ok(Spur::try_from_usize(ordinal as usize).unwrap()),
                |_slot, (start, end)| Ok(Span::new(start, end)),
            )
            .unwrap();
        assert!(
            checkpoints <= many.len() * 3 + 10,
            "symbol validation regressed beyond linear work: {checkpoints}"
        );
    }

    #[test]
    fn out_of_range_symbol_is_rejected() {
        let symbols = ThreadedRodeo::new();
        let name = symbols.get_or_intern("name");
        let mut editor = RirEditor::new();
        let type_name = named_type(&mut editor, name);
        let value = editor.add_inst(Inst {
            data: InstData::TypeConst { type_name },
            span: Span::new(0, 1),
        });
        let root = editor
            .add_const_decl(&[], false, name, None, value, Span::new(0, 1))
            .unwrap();
        let source = ValidatedRir::finish(
            editor,
            &RirValidationContext {
                symbol_count: symbols.len(),
                source_lengths: &[(FileId::DEFAULT, 1)],
            },
        )
        .unwrap();
        let packed = pack(&source, &symbols, root);
        let header = Header::parse(packed.as_bytes()).unwrap();
        let mut corrupt = packed.as_bytes().to_vec();
        let root_opcode = corrupt[header.instructions_offset..header.basis_offset]
            .iter()
            .rposition(|byte| *byte == 34)
            .expect("fixture has a final constant declaration");
        corrupt[header.instructions_offset + root_opcode + 3] = 1;
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(corrupt))),
            PackedRirDecodeError::SymbolOutOfBounds {
                symbol: 1,
                symbols: 1,
            }
        );
    }

    #[test]
    fn exhaustive_variant_and_payload_fixture_roundtrips_exactly() {
        let symbols = ThreadedRodeo::new();
        let a = symbols.get_or_intern("a");
        let b = symbols.get_or_intern("b");
        symbols.get_or_intern("");
        symbols.get_or_intern("unused");
        let span = Span::new(1, 2);
        let anchor = RirStructuralAnchor::new(vec![
            RirStructuralPathSegment::Body,
            RirStructuralPathSegment::ParameterType(1),
            RirStructuralPathSegment::ReturnType,
            RirStructuralPathSegment::Statement(2),
            RirStructuralPathSegment::Operand(3),
            RirStructuralPathSegment::Branch(4),
            RirStructuralPathSegment::MatchArm(5),
            RirStructuralPathSegment::FieldType(6),
            RirStructuralPathSegment::VariantPayload {
                variant: 7,
                payload: 8,
            },
            RirStructuralPathSegment::Method(9),
            RirStructuralPathSegment::AnonymousType(10),
            RirStructuralPathSegment::StringLiteral(11),
            RirStructuralPathSegment::ReadOnlyData(12),
        ]);
        let directive = RirDirective {
            name: a,
            args: vec![b],
            span,
        };
        let mut editor = RirEditor::new();
        let type_a = named_type(&mut editor, a);
        let type_b = named_type(&mut editor, b);
        let mut refs = Vec::new();
        macro_rules! add {
            ($data:expr) => {{
                let value = editor.add_inst(Inst { data: $data, span });
                refs.push(value);
                value
            }};
        }
        let unit = add!(InstData::UnitConst);
        let block = editor.add_block(&[unit], span).unwrap();
        refs.push(block);
        add!(InstData::IntConst(u64::MAX));
        add!(InstData::FloatConst { text: a });
        add!(InstData::BoolConst(true));
        add!(InstData::StringConst {
            content: b,
            anchor: anchor.clone()
        });
        macro_rules! binary {
            ($variant:ident) => {
                add!(InstData::$variant {
                    lhs: unit,
                    rhs: unit
                });
            };
        }
        binary!(Add);
        binary!(Sub);
        binary!(Mul);
        binary!(Div);
        binary!(Mod);
        binary!(Eq);
        binary!(Ne);
        binary!(Lt);
        binary!(Gt);
        binary!(Le);
        binary!(Ge);
        binary!(And);
        binary!(Or);
        binary!(BitAnd);
        binary!(BitOr);
        binary!(BitXor);
        binary!(Shl);
        binary!(Shr);
        add!(InstData::Neg { operand: unit });
        add!(InstData::Not { operand: unit });
        add!(InstData::BitNot { operand: unit });
        add!(InstData::Try { operand: unit });
        add!(InstData::Branch {
            cond: unit,
            then_block: block,
            else_block: Some(block)
        });
        add!(InstData::Loop {
            cond: unit,
            body: block
        });
        add!(InstData::InfiniteLoop {
            body: block,
            iter_borrow: Some(a)
        });
        let matched = editor
            .add_match(
                unit,
                &[(
                    RirPattern::Path {
                        module: Some(unit),
                        ctor_head: Some(unit),
                        type_name: a,
                        variant: b,
                        bindings: vec![a, b],
                        span,
                    },
                    block,
                )],
                span,
            )
            .unwrap();
        refs.push(matched);
        add!(InstData::Break { value: Some(unit) });
        add!(InstData::Continue);
        let function = editor
            .add_fn_decl(
                std::slice::from_ref(&directive),
                true,
                true,
                true,
                true,
                a,
                &[RirParam {
                    name: a,
                    ty: type_b,
                    mode: RirParamMode::Borrow,
                    is_comptime: true,
                    span,
                }],
                type_b,
                block,
                true,
                RirParamMode::Inout,
                true,
                true,
                span,
            )
            .unwrap();
        refs.push(function);
        let args = [RirCallArg {
            value: unit,
            mode: RirArgMode::Inout,
        }];
        refs.push(editor.add_call(a, &args, span).unwrap());
        refs.push(editor.add_intrinsic(a, &[unit], span).unwrap());
        refs.push(
            editor
                .add_internal_intrinsic(InternalIntrinsic::CharNextLossy, &[unit, unit], span)
                .unwrap(),
        );
        add!(InstData::TypeIntrinsic {
            name: a,
            type_arg: type_b
        });
        add!(InstData::OffsetOf {
            type_arg: type_a,
            field: b
        });
        add!(InstData::Ret(Some(unit)));
        add!(InstData::Yield(unit));
        refs.push(
            editor
                .add_alloc(
                    std::slice::from_ref(&directive),
                    Some(a),
                    true,
                    Some(type_b),
                    unit,
                    true,
                    span,
                )
                .unwrap(),
        );
        add!(InstData::VarRef {
            name: a,
            anchor: Some(anchor.clone())
        });
        add!(InstData::Assign {
            name: a,
            value: unit
        });
        refs.push(
            editor
                .add_struct_decl(
                    std::slice::from_ref(&directive),
                    true,
                    true,
                    a,
                    &[(a, type_b)],
                    &[function],
                    span,
                )
                .unwrap(),
        );
        refs.push(
            editor
                .add_struct_init(Some(unit), Some(unit), a, &[(a, unit)], Some(span), span)
                .unwrap(),
        );
        add!(InstData::FieldGet {
            base: unit,
            field: a
        });
        add!(InstData::FieldSet {
            base: unit,
            field: a,
            value: unit
        });
        refs.push(
            editor
                .add_enum_decl(true, false, a, &[a, b], &[vec![type_a], vec![]], span)
                .unwrap(),
        );
        add!(InstData::EnumVariant {
            module: Some(unit),
            type_name: a,
            variant: b
        });
        refs.push(editor.add_array_init(&[unit, block], span).unwrap());
        add!(InstData::ArrayRepeat {
            value: unit,
            count: RepeatCount::Literal(u64::MAX)
        });
        add!(InstData::IndexGet {
            base: unit,
            index: unit
        });
        add!(InstData::IndexSet {
            base: unit,
            index: unit,
            value: unit
        });
        refs.push(editor.add_method_call(unit, a, &args, span).unwrap());
        add!(InstData::DropFnDecl {
            type_name: a,
            body: block
        });
        add!(InstData::Comptime { expr: unit });
        add!(InstData::Checked { expr: unit });
        add!(InstData::TypeConst { type_name: type_a });
        refs.push(
            editor
                .add_anon_struct_type(&[(a, type_b)], &[function], anchor.clone(), span)
                .unwrap(),
        );
        refs.push(
            editor
                .add_anon_enum_type(&[a, b], &[vec![type_b], vec![]], anchor, span)
                .unwrap(),
        );
        let root = editor
            .add_const_decl(
                std::slice::from_ref(&directive),
                true,
                a,
                Some(type_b),
                block,
                span,
            )
            .unwrap();
        refs.push(root);
        assert_eq!(
            refs.len(),
            63,
            "fixture must contain exactly one of every InstData variant"
        );
        assert_eq!(editor.len(), 63);
        let variants = editor
            .iter()
            .map(|(_, instruction)| std::mem::discriminant(&instruction.data))
            .collect::<ahash::AHashSet<_>>();
        assert_eq!(variants.len(), 63, "fixture duplicated an InstData variant");
        let payload_stats = editor.payload_storage_stats();
        assert_eq!(RIR_PAYLOAD_FAMILY_NAMES.len(), 17);
        assert!(
            payload_stats
                .family_logical_bytes
                .iter()
                .all(|bytes| *bytes != 0),
            "fixture must populate every typed payload family: {:?}",
            payload_stats.family_logical_bytes,
        );

        let context = RirValidationContext {
            symbol_count: symbols.len(),
            source_lengths: &[(FileId::DEFAULT, 3)],
        };
        let source = ValidatedRir::finish(editor, &context).unwrap();
        let packed = pack(&source, &symbols, root);
        let mut destination = RirEditor::new();
        let appended = append(&packed, &mut destination).unwrap();
        assert_eq!(appended.metadata.declaration, root);
        let decoded = ValidatedRir::finish(destination, &context).unwrap();
        assert!(source.exact_eq(&decoded));
        assert_eq!(pack(&decoded, &symbols, root).as_bytes(), packed.as_bytes());

        let (direct, metadata) = packed
            .try_decode_validated(
                PackedRirProjection {
                    symbol_count: context.symbol_count,
                    file_id: FileId::DEFAULT,
                    declaration_start: 0,
                    source_length: 3,
                },
                || Ok::<_, ()>(()),
            )
            .unwrap();
        assert_eq!(metadata.declaration, root);
        assert!(source.exact_eq(&direct));
        assert_eq!(pack(&direct, &symbols, root).as_bytes(), packed.as_bytes());
    }

    #[test]
    fn primitive_tag_and_varint_decoders_reject_unknown_or_overflowing_values() {
        assert!(matches!(
            Reader::new(&[2]).boolean("test"),
            Err(PackedRirDecodeError::InvalidTag {
                family: "test",
                tag: 2
            })
        ));
        assert!(decode_param_mode(3).is_err());
        assert!(decode_arg_mode(3).is_err());
        assert!(decode_internal_intrinsic(5).is_err());
        for mode in [
            RirParamMode::Normal,
            RirParamMode::Inout,
            RirParamMode::Borrow,
        ] {
            assert_eq!(decode_param_mode(encode_param_mode(mode)).unwrap(), mode);
        }
        for mode in [RirArgMode::Normal, RirArgMode::Inout, RirArgMode::Borrow] {
            assert_eq!(decode_arg_mode(encode_arg_mode(mode)).unwrap(), mode);
        }
        for intrinsic in [
            InternalIntrinsic::IterLen,
            InternalIntrinsic::CharScalar,
            InternalIntrinsic::CharNext,
            InternalIntrinsic::CharScalarLossy,
            InternalIntrinsic::CharNextLossy,
        ] {
            assert_eq!(
                decode_internal_intrinsic(encode_internal_intrinsic(intrinsic)).unwrap(),
                intrinsic,
            );
        }
        let mut too_large_u32 = Vec::new();
        put_u64(&mut too_large_u32, u64::from(u32::MAX) + 1);
        assert_eq!(
            Reader::new(&too_large_u32).u32(),
            Err(PackedRirDecodeError::VarintOverflow)
        );
        let mut too_large_u64 = vec![0x80; 9];
        too_large_u64.push(2);
        assert_eq!(
            Reader::new(&too_large_u64).u64(),
            Err(PackedRirDecodeError::VarintOverflow)
        );
    }

    #[test]
    fn basis_corruption_and_projection_inversion_are_typed() {
        let (source, symbols, root) = validated_owner(false);
        let packed = pack(&source, &symbols, root);
        let header = Header::parse(packed.as_bytes()).unwrap();

        let mut inverted = packed.as_bytes().to_vec();
        inverted[header.basis_offset] = 2;
        inverted[header.basis_offset + 1] = 1;
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(inverted))),
            PackedRirDecodeError::InvalidBasisSpan { start: 2, end: 1 }
        );

        let mut extra = packed.as_bytes().to_vec();
        extra.push(0);
        set_header(&mut extra, 52, header.end_offset + 1);
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(extra))),
            PackedRirDecodeError::TrailingBytes
        );

        let mut fewer = packed.as_bytes().to_vec();
        set_header(&mut fewer, 56, header.spans as usize - 1);
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(fewer))),
            PackedRirDecodeError::TrailingBytes
        );

        let projection = source.try_pack_candidate(
            &symbols,
            PackedRirMetadata {
                declaration: root,
                method_owner: None,
            },
            || Ok::<_, ()>(()),
            |_slot, _span| Ok((9, 3)),
        );
        assert!(matches!(
            projection,
            Err(PackedRirEncodeError::InvalidProjectedSpan {
                start: 9,
                end: 3,
                ..
            })
        ));
    }

    #[test]
    fn anchor_tags_and_counts_are_validated() {
        let symbols = ThreadedRodeo::new();
        let name = symbols.get_or_intern("name");
        let mut editor = RirEditor::new();
        let value = editor.add_inst(Inst {
            data: InstData::StringConst {
                content: name,
                anchor: RirStructuralAnchor::new(vec![RirStructuralPathSegment::Body]),
            },
            span: Span::new(0, 1),
        });
        let root = editor
            .add_const_decl(&[], false, name, None, value, Span::new(0, 1))
            .unwrap();
        let source = ValidatedRir::finish(
            editor,
            &RirValidationContext {
                symbol_count: symbols.len(),
                source_lengths: &[(FileId::DEFAULT, 1)],
            },
        )
        .unwrap();
        let packed = pack(&source, &symbols, root);
        let header = Header::parse(packed.as_bytes()).unwrap();
        let mut bad_tag = packed.as_bytes().to_vec();
        bad_tag[header.instructions_offset + 3] = 13;
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(bad_tag))),
            PackedRirDecodeError::InvalidTag {
                family: "structural anchor segment",
                tag: 13
            }
        );
        let mut bad_count = packed.as_bytes().to_vec();
        bad_count[header.instructions_offset + 2] = 127;
        assert_eq!(
            decode_error(PackedValidatedRir(Arc::from(bad_count))),
            PackedRirDecodeError::CountOutOfBounds {
                family: "structural anchor"
            }
        );
    }

    #[test]
    fn method_owner_metadata_resolves_through_complete_dense_symbols() {
        let symbols = ThreadedRodeo::new();
        let empty = symbols.get_or_intern("");
        symbols.get_or_intern("unused");
        let name = symbols.get_or_intern("method");
        let ty = symbols.get_or_intern("Type");
        let mut editor = RirEditor::new();
        let return_type = named_type(&mut editor, ty);
        let unit = editor.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 1),
        });
        let block = editor.add_block(&[unit], Span::new(0, 1)).unwrap();
        let root = editor
            .add_fn_decl(
                &[],
                true,
                false,
                false,
                false,
                name,
                &[],
                return_type,
                block,
                true,
                RirParamMode::Normal,
                false,
                false,
                Span::new(0, 1),
            )
            .unwrap();
        let context = RirValidationContext {
            symbol_count: symbols.len(),
            source_lengths: &[(FileId::DEFAULT, 1)],
        };
        let source = ValidatedRir::finish(editor, &context).unwrap();
        let packed = source
            .try_pack_candidate(
                &symbols,
                PackedRirMetadata {
                    declaration: root,
                    method_owner: Some(PackedRirMethodOwner {
                        declaration: root,
                        name: empty,
                        is_public: true,
                        is_linear: true,
                    }),
                },
                || Ok::<_, ()>(()),
                |_slot, span| Ok((span.start, span.end)),
            )
            .unwrap();
        assert_eq!(
            packed.symbols().collect::<Vec<_>>(),
            ["", "unused", "method", "Type"]
        );
        let mut destination = RirEditor::new();
        let appended = append(&packed, &mut destination).unwrap();
        assert_eq!(appended.metadata.declaration, root);
        assert_eq!(
            appended.metadata.method_owner,
            Some(PackedRirMethodOwner {
                declaration: root,
                name: empty,
                is_public: true,
                is_linear: true,
            })
        );

        let (decoded, metadata) = packed
            .try_decode_validated_with_method_owner(
                PackedRirProjection {
                    symbol_count: context.symbol_count,
                    file_id: FileId::DEFAULT,
                    declaration_start: 0,
                    source_length: 3,
                },
                || Ok::<_, ()>(()),
            )
            .unwrap();
        assert_eq!(metadata, appended.metadata);
        assert_eq!(decoded.len(), source.len() + 1);
        let owner = InstRef::from_raw(decoded.len() as u32 - 1);
        let InstData::StructDecl {
            name: owner_name,
            methods,
            is_pub,
            is_linear,
            ..
        } = &decoded.get(owner).data
        else {
            panic!("validated method candidate must end in its synthetic owner shell")
        };
        assert_eq!(*owner_name, empty);
        assert!(*is_pub);
        assert!(*is_linear);
        assert_eq!(
            decoded
                .struct_methods(methods)
                .iter()
                .map(|method| *method)
                .collect::<Vec<_>>(),
            [root]
        );
    }

    fn finish_metadata_fixture(editor: RirEditor, symbols: &ThreadedRodeo) -> ValidatedRir {
        ValidatedRir::finish(
            editor,
            &RirValidationContext {
                symbol_count: symbols.len(),
                source_lengths: &[(FileId::DEFAULT, 4)],
            },
        )
        .unwrap()
    }

    #[test]
    fn declaration_root_and_owner_metadata_matrix_is_enforced() {
        let symbols = ThreadedRodeo::new();
        let a = symbols.get_or_intern("a");
        let b = symbols.get_or_intern("b");
        let metadata = |root| PackedRirMetadata {
            declaration: root,
            method_owner: None,
        };
        let pack_result = |rir: &ValidatedRir, value| {
            rir.try_pack_candidate(
                &symbols,
                value,
                || Ok::<_, ()>(()),
                |_slot, span| Ok((span.start, span.end)),
            )
        };

        let mut function = RirEditor::new();
        let function_return = named_type(&mut function, b);
        let unit = function.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 1),
        });
        let block = function.add_block(&[unit], Span::new(0, 1)).unwrap();
        let function_root = function
            .add_fn_decl(
                &[],
                false,
                false,
                false,
                false,
                a,
                &[],
                function_return,
                block,
                false,
                RirParamMode::Normal,
                false,
                false,
                Span::new(0, 1),
            )
            .unwrap();
        let function = finish_metadata_fixture(function, &symbols);
        assert!(pack_result(&function, metadata(function_root)).is_ok());
        assert!(
            pack_result(
                &function,
                PackedRirMetadata {
                    declaration: function_root,
                    method_owner: Some(PackedRirMethodOwner {
                        declaration: function_root,
                        name: a,
                        is_public: false,
                        is_linear: false,
                    }),
                }
            )
            .is_ok()
        );
        assert!(matches!(
            pack_result(
                &function,
                PackedRirMetadata {
                    declaration: function_root,
                    method_owner: Some(PackedRirMethodOwner {
                        declaration: unit,
                        name: a,
                        is_public: false,
                        is_linear: false,
                    }),
                }
            ),
            Err(PackedRirEncodeError::InvalidMetadata)
        ));
        let outside_name = Spur::try_from_usize(symbols.len() + 10).unwrap();
        assert!(matches!(
            pack_result(
                &function,
                PackedRirMetadata {
                    declaration: function_root,
                    method_owner: Some(PackedRirMethodOwner {
                        declaration: function_root,
                        name: outside_name,
                        is_public: false,
                        is_linear: false,
                    }),
                }
            ),
            Err(PackedRirEncodeError::InvalidMetadata)
        ));

        let mut constant = RirEditor::new();
        let unit = constant.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 1),
        });
        let constant_root = constant
            .add_const_decl(&[], false, a, None, unit, Span::new(0, 1))
            .unwrap();
        let constant = finish_metadata_fixture(constant, &symbols);
        assert!(pack_result(&constant, metadata(constant_root)).is_ok());
        assert!(matches!(
            pack_result(
                &constant,
                PackedRirMetadata {
                    declaration: constant_root,
                    method_owner: Some(PackedRirMethodOwner {
                        declaration: constant_root,
                        name: a,
                        is_public: false,
                        is_linear: false,
                    }),
                }
            ),
            Err(PackedRirEncodeError::InvalidMetadata)
        ));

        let mut structure = RirEditor::new();
        let structure_field = named_type(&mut structure, b);
        let structure_root = structure
            .add_struct_decl(
                &[],
                false,
                false,
                a,
                &[(a, structure_field)],
                &[],
                Span::new(0, 1),
            )
            .unwrap();
        let structure = finish_metadata_fixture(structure, &symbols);
        assert!(pack_result(&structure, metadata(structure_root)).is_ok());

        let mut enumeration = RirEditor::new();
        let enumeration_payload = named_type(&mut enumeration, b);
        let enum_root = enumeration
            .add_enum_decl(
                false,
                false,
                a,
                &[a],
                &[vec![enumeration_payload]],
                Span::new(0, 1),
            )
            .unwrap();
        let enumeration = finish_metadata_fixture(enumeration, &symbols);
        assert!(pack_result(&enumeration, metadata(enum_root)).is_ok());

        let mut drop_fn = RirEditor::new();
        let unit = drop_fn.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 1),
        });
        let drop_root = drop_fn.add_inst(Inst {
            data: InstData::DropFnDecl {
                type_name: a,
                body: unit,
            },
            span: Span::new(0, 1),
        });
        let drop_fn = finish_metadata_fixture(drop_fn, &symbols);
        assert!(pack_result(&drop_fn, metadata(drop_root)).is_ok());

        let mut expression = RirEditor::new();
        let expression_root = expression.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 1),
        });
        let expression = finish_metadata_fixture(expression, &symbols);
        assert!(matches!(
            pack_result(&expression, metadata(expression_root)),
            Err(PackedRirEncodeError::InvalidMetadata)
        ));

        let mut nonfinal = RirEditor::new();
        let unit = nonfinal.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 1),
        });
        let nonfinal_root = nonfinal
            .add_const_decl(&[], false, a, None, unit, Span::new(0, 1))
            .unwrap();
        nonfinal.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 1),
        });
        let nonfinal = finish_metadata_fixture(nonfinal, &symbols);
        assert!(matches!(
            pack_result(&nonfinal, metadata(nonfinal_root)),
            Err(PackedRirEncodeError::InvalidMetadata)
        ));

        let mut methodful = RirEditor::new();
        let method_return = named_type(&mut methodful, b);
        let unit = methodful.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 1),
        });
        let block = methodful.add_block(&[unit], Span::new(0, 1)).unwrap();
        let method = methodful
            .add_fn_decl(
                &[],
                false,
                false,
                false,
                false,
                a,
                &[],
                method_return,
                block,
                false,
                RirParamMode::Normal,
                false,
                false,
                Span::new(0, 1),
            )
            .unwrap();
        let methodful_root = methodful
            .add_struct_decl(&[], false, false, a, &[], &[method], Span::new(0, 1))
            .unwrap();
        let methodful = finish_metadata_fixture(methodful, &symbols);
        assert!(matches!(
            pack_result(&methodful, metadata(methodful_root)),
            Err(PackedRirEncodeError::InvalidMetadata)
        ));
    }

    #[test]
    fn direct_typed_payload_mutation_changes_packed_identity() {
        fn owner(elements: usize) -> (ValidatedRir, ThreadedRodeo, InstRef) {
            let symbols = ThreadedRodeo::new();
            let name = symbols.get_or_intern("name");
            let mut editor = RirEditor::new();
            let unit = editor.add_inst(Inst {
                data: InstData::UnitConst,
                span: Span::new(0, 1),
            });
            let block = editor
                .add_block(&vec![unit; elements], Span::new(0, 1))
                .unwrap();
            let root = editor
                .add_const_decl(&[], false, name, None, block, Span::new(0, 1))
                .unwrap();
            let rir = ValidatedRir::finish(
                editor,
                &RirValidationContext {
                    symbol_count: symbols.len(),
                    source_lengths: &[(FileId::DEFAULT, 1)],
                },
            )
            .unwrap();
            (rir, symbols, root)
        }
        let (one, one_symbols, one_root) = owner(1);
        let (two, two_symbols, two_root) = owner(2);
        assert_ne!(
            pack(&one, &one_symbols, one_root).as_bytes(),
            pack(&two, &two_symbols, two_root).as_bytes(),
        );
    }

    #[test]
    fn struct_root_method_override_is_typed_and_atomic() {
        let symbols = ThreadedRodeo::new();
        let name = symbols.get_or_intern("S");
        let ty = symbols.get_or_intern("T");
        let mut shell = RirEditor::new();
        let shell_field_type = named_type(&mut shell, ty);
        let shell_root = shell
            .add_struct_decl(
                &[],
                false,
                false,
                name,
                &[(name, shell_field_type)],
                &[],
                Span::new(0, 1),
            )
            .unwrap();
        let shell = ValidatedRir::finish(
            shell,
            &RirValidationContext {
                symbol_count: symbols.len(),
                source_lengths: &[(FileId::DEFAULT, 1)],
            },
        )
        .unwrap();
        let packed_shell = pack(&shell, &symbols, shell_root);

        let mut destination = RirEditor::new();
        let method_return = named_type(&mut destination, ty);
        let unit = destination.add_inst(Inst {
            data: InstData::UnitConst,
            span: Span::new(0, 1),
        });
        let block = destination.add_block(&[unit], Span::new(0, 1)).unwrap();
        let method = destination
            .add_fn_decl(
                &[],
                false,
                false,
                false,
                false,
                name,
                &[],
                method_return,
                block,
                false,
                RirParamMode::Normal,
                false,
                false,
                Span::new(0, 1),
            )
            .unwrap();
        let appended = packed_shell
            .try_append_remapped_with_root_methods(
                &mut destination,
                &[method],
                || Ok::<_, ()>(()),
                |ordinal| Ok(Spur::try_from_usize(ordinal as usize).unwrap()),
                |_slot, (start, end)| Ok(Span::new(start, end)),
            )
            .unwrap();
        let InstData::StructDecl { methods, .. } =
            &destination.get(appended.metadata.declaration).data
        else {
            panic!("override root must remain a struct");
        };
        assert_eq!(destination.struct_methods(methods).to_vec(), [method]);

        let before_cancel_instructions = destination.rir.instructions.len();
        let before_cancel_extra = destination.rir.extra.len();
        let before_cancel_latch = destination.rir.instruction_limit_exceeded;
        let many_methods = vec![method; 128];
        let mut checks = 0usize;
        let canceled = packed_shell.try_append_remapped_with_root_methods(
            &mut destination,
            &many_methods,
            || {
                checks += 1;
                if checks == 6 { Err(()) } else { Ok(()) }
            },
            |ordinal| Ok(Spur::try_from_usize(ordinal as usize).unwrap()),
            |_slot, (start, end)| Ok(Span::new(start, end)),
        );
        assert!(matches!(
            canceled,
            Err(PackedRirAppendError::Checkpoint(()))
        ));
        assert_eq!(
            destination.rir.instructions.len(),
            before_cancel_instructions
        );
        assert_eq!(destination.rir.extra.len(), before_cancel_extra);
        assert_eq!(
            destination.rir.instruction_limit_exceeded,
            before_cancel_latch
        );
        packed_shell
            .try_append_remapped_with_root_methods(
                &mut destination,
                &many_methods,
                || Ok::<_, ()>(()),
                |ordinal| Ok(Spur::try_from_usize(ordinal as usize).unwrap()),
                |_slot, (start, end)| Ok(Span::new(start, end)),
            )
            .unwrap();

        let instruction_len = destination.rir.instructions.len();
        let extra_len = destination.rir.extra.len();
        for invalid in [unit, InstRef::from_raw(u32::MAX - 1)] {
            let error = packed_shell.try_append_remapped_with_root_methods(
                &mut destination,
                &[invalid],
                || Ok::<_, ()>(()),
                |ordinal| Ok(Spur::try_from_usize(ordinal as usize).unwrap()),
                |_slot, (start, end)| Ok(Span::new(start, end)),
            );
            assert!(matches!(
                error,
                Err(PackedRirAppendError::Decode(
                    PackedRirDecodeError::InvalidTag {
                        family: "composed struct method",
                        ..
                    }
                ))
            ));
            assert_eq!(destination.rir.instructions.len(), instruction_len);
            assert_eq!(destination.rir.extra.len(), extra_len);
        }

        let (constant, constant_symbols, constant_root) = validated_owner(false);
        let packed_constant = pack(&constant, &constant_symbols, constant_root);
        let error = packed_constant.try_append_remapped_with_root_methods(
            &mut destination,
            &[],
            || Ok::<_, ()>(()),
            |ordinal| Ok(Spur::try_from_usize(ordinal as usize).unwrap()),
            |_slot, (start, end)| Ok(Span::new(start, end)),
        );
        assert!(matches!(
            error,
            Err(PackedRirAppendError::Decode(
                PackedRirDecodeError::InvalidTag {
                    family: "declaration root",
                    ..
                }
            ))
        ));
        assert_eq!(destination.rir.instructions.len(), instruction_len);
        assert_eq!(destination.rir.extra.len(), extra_len);
    }
}
