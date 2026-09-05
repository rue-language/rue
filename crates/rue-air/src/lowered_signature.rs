//! The one signature classifier and the lowered signature it produces
//! (ADR-0064 P2/P3/P4, ADR-0084).
//!
//! [`lower_c_signature`] is the single function that answers *where every value
//! of a `"C"` signature lives*: which register bank and roster index, which
//! byte of the outgoing argument area, or which pointer. Every C crossing site
//! consumes its answer — the `extern "C"` import call planner, the
//! `pub extern "C" fn` export thunk, and, when it exists, the callback
//! trampoline — which is ADR-0064's ratified acceptance criterion that the
//! by-value classifier agree across all four sites. There is no second placement
//! algorithm in the tree.
//!
//! ## Two entry points, one placement walk
//!
//! The native Rue convention is the compilation target's C convention plus a
//! wider return bank (ADR-0084), so it places arguments by exactly these rules
//! and differs only in its result. That is the shape of the two entry points:
//! [`lower_c_signature`] takes a C row and classifies both halves of the
//! signature, while [`lower_native_signature`] takes a
//! [`ConventionSpec`] and an already-decided [`LoweredReturn`], because the
//! native return bank is wider than any [`CConventionSpec`] describes. Both run
//! the same walk over the same [`Placer`], so a native argument and a C
//! argument of the same shape land in the same place by construction.
//!
//! ## Why this crate is the home
//!
//! The psABI *rules* need no type facts, so they are data in `rue-target`
//! ([`CConventionSpec`]). The classifier does need type facts — a size, an
//! alignment, a scalar's width and signedness — and `rue-air` is where type
//! facts first exist. So the description lives one crate below and this module
//! reads it.
//!
//! ## Two planes, one kernel
//!
//! The input is [`CAbiTypeFacts`], never a [`Type`](crate::Type): the live
//! classifier projects the facts from the request-scoped `FrozenTypeInternPool`
//! and the stable query plane projects them from its revision-stable type keys
//! and canonical layout values, exactly as they already share
//! [`NativeAbiTypeFacts`](crate::NativeAbiTypeFacts) for the native convention.
//! Both then call this one kernel, so the two planes cannot disagree about a
//! placement.
//!
//! ## Floats
//!
//! Classification is complete over floating-point values: an eightbyte whose
//! every leaf is a float classifies [`EightbyteClass::Sse`] and travels in
//! [`CRegisterClass::Fp`], and AAPCS64's homogeneous floating-point aggregates
//! travel in consecutive floating-point registers. No *C* signature reaches
//! that surface, because the C boundary rejects `f32`/`f64`
//! (`c_passable_by_value`); the native convention is what has floats to place,
//! and it places them by these same rules.

use rue_target::{
    AggregateClassificationRule, CConventionSpec, CRegisterClass, CallingConvention,
    ConventionSpec, SretRegisterKind, StackedArgumentPacking,
};

use crate::call_abi::{ArgConvention, CAbiScalarKind, ScalarAbiExtension};

/// Target-independent facts about one value type at a C boundary: the
/// projection each plane makes before this module classifies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CAbiTypeFacts {
    /// A zero-sized value. It occupies no register, no stack byte, and no
    /// pointer, in either direction.
    ZeroSized,
    /// A scalar or pointer: one register of `class`, with `kind` naming the
    /// width and signedness the extension table is keyed by.
    Scalar {
        /// The scalar's width-and-signedness class.
        kind: CAbiScalarKind,
        /// The register bank the scalar travels in. `Gp` for every scalar the
        /// C boundary currently admits.
        class: CRegisterClass,
    },
    /// An aggregate (struct, array, or enum) of `size` bytes at `align`
    /// alignment, with the scalar-leaf facts its eightbyte and
    /// homogeneous-float classification reads.
    Aggregate {
        /// The aggregate's `@size_of`.
        size: u64,
        /// The aggregate's `@align_of`.
        align: u64,
        /// What the aggregate's scalar leaves are, folded to what
        /// classification asks of them.
        leaves: AggregateLeaves,
    },
}

impl CAbiTypeFacts {
    /// The facts for one by-reference (`inout` / `borrow`) argument: a single
    /// register-width pointer, whatever it points at.
    pub const fn by_reference_pointer() -> Self {
        Self::Scalar {
            kind: CAbiScalarKind::RegisterWidth,
            class: CRegisterClass::Gp,
        }
    }

    /// The facts for an aggregate whose every leaf is an integer or a pointer:
    /// every eightbyte classifies INTEGER and no row's floating-point rule
    /// applies.
    pub const fn integer_aggregate(size: u64, align: u64) -> Self {
        Self::Aggregate {
            size,
            align,
            leaves: AggregateLeaves::all_integer(size),
        }
    }

    /// The facts an argument presents once its source-level mode is applied: a
    /// by-reference argument is one pointer regardless of its pointee.
    pub const fn as_argument(self, convention: ArgConvention) -> Self {
        match convention {
            ArgConvention::ByReference => Self::by_reference_pointer(),
            ArgConvention::ByValue => self,
        }
    }

    /// The extension this value carries at the boundary: a scalar's canonical
    /// 64-bit extension, and none for an aggregate or a zero-sized value.
    pub const fn extension(self) -> ScalarAbiExtension {
        match self {
            Self::Scalar { kind, .. } => kind.extension(),
            Self::ZeroSized | Self::Aggregate { .. } => ScalarAbiExtension::None,
        }
    }
}

/// The largest aggregate whose scalar leaves can still change where it
/// travels.
///
/// Every supported row places a larger aggregate through memory whatever its
/// leaves are: past
/// [`CConventionSpec::max_aggregate_register_bytes`] (16) an aggregate is
/// SysV MEMORY class or an AAPCS64 by-reference copy, AAPCS64's widest
/// homogeneous floating-point aggregate is four eight-byte members, and the
/// widest native return bank is eight eightbytes. So a projection may stop
/// walking an aggregate this large and report every eightbyte INTEGER without
/// changing any placement, which bounds the cost of projecting the leaves of a
/// large array.
pub const MAX_LEAF_CLASSIFIED_BYTES: u64 = 64;

/// The kind of one scalar leaf of an aggregate, as classification sees it.
///
/// The distinction is the one the psABIs draw and no finer: a leaf either
/// forces its eightbyte into an integer register or it is a float that may
/// leave the eightbyte in the floating-point bank, and the two float widths are
/// told apart because AAPCS64's homogeneous floating-point aggregate rule is
/// keyed by member width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CAbiLeafKind {
    /// An integer, `bool`, or pointer leaf.
    Integer,
    /// An `f32` leaf.
    F32,
    /// An `f64` leaf.
    F64,
}

impl CAbiLeafKind {
    /// The leaf's width in bytes when it is a float, `None` when it is not.
    pub const fn float_bytes(self) -> Option<u32> {
        match self {
            Self::Integer => None,
            Self::F32 => Some(4),
            Self::F64 => Some(8),
        }
    }
}

/// One scalar leaf of an aggregate: `width` bytes of `kind` at `offset` bytes
/// into the aggregate's memory image.
///
/// This is what a plane projects; [`AggregateLeaves`] is what classification
/// reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CAbiLeaf {
    /// Byte offset of the leaf within the aggregate.
    pub offset: u64,
    /// The leaf's byte width.
    pub width: u64,
    /// The leaf's classification kind.
    pub kind: CAbiLeafKind,
}

impl CAbiLeaf {
    /// One integer or pointer leaf.
    pub const fn integer(offset: u64, width: u64) -> Self {
        Self {
            offset,
            width,
            kind: CAbiLeafKind::Integer,
        }
    }

    /// One `f32` leaf.
    pub const fn f32(offset: u64) -> Self {
        Self {
            offset,
            width: 4,
            kind: CAbiLeafKind::F32,
        }
    }

    /// One `f64` leaf.
    pub const fn f64(offset: u64) -> Self {
        Self {
            offset,
            width: 8,
            kind: CAbiLeafKind::F64,
        }
    }
}

/// Whether an aggregate's leaves are all floats of one width — the AAPCS64
/// homogeneous floating-point aggregate question, answered before the
/// convention decides what to do about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FloatMembers {
    /// Some leaf is not a float, or two float leaves have different widths.
    Mixed,
    /// Every leaf folded so far is a float of `width` bytes, and there are
    /// `count` of them. `count == 0` is the fold's identity: an aggregate with
    /// no leaves at all.
    Homogeneous { width: u32, count: u32 },
}

/// What classification needs to know about an aggregate's scalar leaves.
///
/// A plane projects the leaves themselves ([`CAbiLeaf`]) and this folds them
/// into the two questions every supported row asks: *which bank does each
/// eightbyte belong to* (SysV AMD64 section 3.2.3) and *is the whole aggregate
/// a homogeneous floating-point aggregate* (AAPCS64 section 6.8.2 rule C.3).
/// The fold is what keeps the facts a small `Copy` value, and it keeps the
/// psABI decisions themselves in the classifier: an eightbyte's bank is a fact
/// about the type, what a convention does with it is not.
///
/// Only the first `MAX_LEAF_CLASSIFIED_BYTES / 8` eightbytes are recorded,
/// because no row's answer for a larger aggregate depends on its leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AggregateLeaves {
    /// Bit *i* is set when eightbyte *i* holds at least one integer leaf.
    integer_eightbytes: u64,
    /// Bit *i* is set when eightbyte *i* holds at least one float leaf.
    float_eightbytes: u64,
    /// The homogeneous floating-point summary.
    floats: FloatMembers,
    /// Whether some leaf sits at an offset its own width does not divide, which
    /// SysV AMD64 section 3.2.3 makes MEMORY class outright.
    unaligned: bool,
}

impl AggregateLeaves {
    /// How many eightbytes the bitmaps hold.
    const RECORDED_EIGHTBYTES: u32 = (MAX_LEAF_CLASSIFIED_BYTES / 8) as u32;

    /// The leaves of an aggregate whose every leaf is an integer or a pointer.
    ///
    /// This is both the answer for an integer-only aggregate and the answer a
    /// plane projects when it cannot enumerate leaf kinds: every eightbyte
    /// INTEGER, no homogeneous float rule, nothing unaligned.
    pub const fn all_integer(size: u64) -> Self {
        Self {
            integer_eightbytes: eightbyte_mask(size),
            float_eightbytes: 0,
            floats: FloatMembers::Mixed,
            unaligned: false,
        }
    }

    /// Fold `leaves` — in any order — into the classification facts of a
    /// `size`-byte aggregate.
    pub fn from_leaves(size: u64, leaves: impl IntoIterator<Item = CAbiLeaf>) -> Self {
        let mut folded = Self {
            integer_eightbytes: 0,
            float_eightbytes: 0,
            floats: FloatMembers::Homogeneous { width: 0, count: 0 },
            unaligned: false,
        };
        let mut any = false;
        for leaf in leaves {
            any = true;
            folded.push(leaf);
        }
        if !any {
            // An aggregate of nothing but padding classifies as its size says,
            // in the integer bank: no leaf asked for the float one.
            return Self::all_integer(size);
        }
        folded
    }

    fn push(&mut self, leaf: CAbiLeaf) {
        if leaf.width != 0 && leaf.offset % leaf.width != 0 {
            self.unaligned = true;
        }
        let first = leaf.offset / 8;
        let last = leaf
            .offset
            .saturating_add(leaf.width.saturating_sub(1))
            .max(leaf.offset)
            / 8;
        for eightbyte in first..=last.min(u64::from(Self::RECORDED_EIGHTBYTES - 1)) {
            let bit = 1_u64 << eightbyte;
            match leaf.kind {
                CAbiLeafKind::Integer => self.integer_eightbytes |= bit,
                CAbiLeafKind::F32 | CAbiLeafKind::F64 => self.float_eightbytes |= bit,
            }
        }
        self.floats = match (self.floats, leaf.kind.float_bytes()) {
            (FloatMembers::Mixed, _) | (_, None) => FloatMembers::Mixed,
            (FloatMembers::Homogeneous { count: 0, .. }, Some(width)) => {
                FloatMembers::Homogeneous { width, count: 1 }
            }
            (FloatMembers::Homogeneous { width, count }, Some(leaf_width))
                if width == leaf_width =>
            {
                FloatMembers::Homogeneous {
                    width,
                    count: count.saturating_add(1),
                }
            }
            (FloatMembers::Homogeneous { .. }, Some(_)) => FloatMembers::Mixed,
        };
    }

    /// The class of eightbyte `index`, per SysV AMD64 section 3.2.3's merge: an
    /// eightbyte holding any integer or pointer leaf is INTEGER, one holding
    /// only floats is SSE. An eightbyte no leaf reaches — padding, or past what
    /// the projection recorded — is INTEGER, so a value never rides in a
    /// floating-point register on a fact no plane supplied.
    pub const fn eightbyte_class(&self, index: u32) -> EightbyteClass {
        if index >= Self::RECORDED_EIGHTBYTES {
            return EightbyteClass::Integer;
        }
        let bit = 1_u64 << index;
        if self.integer_eightbytes & bit != 0 {
            return EightbyteClass::Integer;
        }
        if self.float_eightbytes & bit != 0 {
            return EightbyteClass::Sse;
        }
        EightbyteClass::Integer
    }

    /// Whether some leaf sits at an offset its own width does not divide.
    pub const fn has_unaligned_leaf(&self) -> bool {
        self.unaligned
    }

    /// The `(member width, member count)` of the homogeneous floating-point
    /// aggregate this is, or `None` when its leaves are not all floats of one
    /// width. The member *count bound* is the convention's, not this fact's.
    pub const fn homogeneous_floats(&self) -> Option<(u32, u32)> {
        match self.floats {
            FloatMembers::Homogeneous { width, count } if count > 0 => Some((width, count)),
            FloatMembers::Homogeneous { .. } | FloatMembers::Mixed => None,
        }
    }
}

/// The bits of an eightbyte bitmap a `size`-byte aggregate occupies.
const fn eightbyte_mask(size: u64) -> u64 {
    let count = size.div_ceil(8);
    if count >= AggregateLeaves::RECORDED_EIGHTBYTES as u64 {
        return u64::MAX;
    }
    (1_u64 << count) - 1
}

/// The register class of one eightbyte of an aggregate.
///
/// SysV AMD64 section 3.2.3 classifies each eightbyte of an aggregate and
/// assigns it a register of the matching bank; AAPCS64's composite rule reaches
/// the same answer for the integer-only surface. Every eightbyte of every
/// currently admissible type is [`Integer`](Self::Integer): a field that would
/// classify [`Sse`](Self::Sse) is a float, which the boundary rejects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EightbyteClass {
    /// The eightbyte travels in a general-purpose integer register.
    Integer,
    /// The eightbyte travels in a floating-point/SIMD register.
    Sse,
}

impl EightbyteClass {
    /// The register bank this eightbyte class travels in.
    pub const fn register_class(self) -> CRegisterClass {
        match self {
            Self::Integer => CRegisterClass::Gp,
            Self::Sse => CRegisterClass::Fp,
        }
    }
}

/// The classes of an aggregate's eightbytes, in ascending memory order.
///
/// A register-passed *argument* spans at most
/// [`CConventionSpec::max_aggregate_register_bytes`] (16) bytes, so at most two
/// eightbytes. The capacity is the widest return bank instead, because the
/// native convention's bank is wider than any C row's (ADR-0084).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EightbyteClasses {
    classes: [EightbyteClass; Self::CAPACITY as usize],
    len: u32,
}

impl EightbyteClasses {
    /// The widest classification this type holds: the eight general-purpose
    /// registers of the native AArch64 return bank.
    pub const CAPACITY: u32 = 8;

    /// The classification of an aggregate whose every eightbyte is INTEGER.
    pub const fn all_integer(count: u32) -> Self {
        assert!(
            count >= 1 && count <= Self::CAPACITY,
            "a classified aggregate spans at least one eightbyte and at most \
             the widest return bank"
        );
        Self {
            classes: [EightbyteClass::Integer; Self::CAPACITY as usize],
            len: count,
        }
    }

    /// The classification of a `count`-eightbyte aggregate whose leaves are
    /// `leaves`, per SysV AMD64 section 3.2.3: an eightbyte is SSE when every
    /// leaf overlapping it is a float, and INTEGER as soon as one is not.
    pub fn classify(leaves: AggregateLeaves, count: u32) -> Self {
        let mut classified = Self::all_integer(count);
        for index in 0..count {
            classified.classes[index as usize] = leaves.eightbyte_class(index);
        }
        classified
    }

    /// How many registers of `class` this classification needs.
    pub fn registers_in(&self, class: CRegisterClass) -> u32 {
        self.as_slice()
            .iter()
            .filter(|eightbyte| eightbyte.register_class() == class)
            .count() as u32
    }

    /// How many eightbytes the aggregate spans.
    pub const fn len(&self) -> u32 {
        self.len
    }

    /// Whether the aggregate spans no eightbyte. Never true for a classified
    /// aggregate; present so `len` reads as a length.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The classes in ascending memory order.
    pub fn as_slice(&self) -> &[EightbyteClass] {
        &self.classes[..self.len as usize]
    }

    /// The single register bank every eightbyte travels in, or `None` when the
    /// aggregate is split across banks (unreachable until floats cross).
    pub fn uniform_register_class(&self) -> Option<CRegisterClass> {
        let first = self.as_slice().first()?.register_class();
        self.as_slice()
            .iter()
            .all(|class| class.register_class() == first)
            .then_some(first)
    }
}

/// One register a value occupies: roster `index` of bank `class`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisterPiece {
    /// The register bank.
    pub class: CRegisterClass,
    /// Roster index within that bank's argument registers.
    pub index: u32,
}

/// The registers one value occupies, one piece per eightbyte or per
/// homogeneous floating-point member, in ascending memory order.
///
/// A value is a list of pieces rather than a run of one bank because SysV
/// AMD64 classifies each eightbyte independently: a `{f64, i64}` struct travels
/// with its first eightbyte in an SSE register and its second in an integer
/// one. [`consecutive`](Self::consecutive) is the common case where every piece
/// is in one bank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegisterPieces {
    pieces: [RegisterPiece; Self::CAPACITY],
    len: u32,
}

impl RegisterPieces {
    /// The widest value this holds: AAPCS64's four-member homogeneous
    /// floating-point aggregate is the widest register-passed argument, and the
    /// capacity matches [`EightbyteClasses::CAPACITY`] so a classification and
    /// its placement cannot disagree about how much they can carry.
    pub const CAPACITY: usize = EightbyteClasses::CAPACITY as usize;

    const EMPTY_PIECE: RegisterPiece = RegisterPiece {
        class: CRegisterClass::Gp,
        index: 0,
    };

    /// `count` consecutive registers of one bank, starting at `first_index`.
    pub fn consecutive(class: CRegisterClass, first_index: u32, count: u32) -> Self {
        let mut pieces = Self {
            pieces: [Self::EMPTY_PIECE; Self::CAPACITY],
            len: 0,
        };
        for offset in 0..count {
            pieces.push(RegisterPiece {
                class,
                index: first_index.saturating_add(offset),
            });
        }
        pieces
    }

    /// One register of `class` at roster index `index`.
    pub fn one(class: CRegisterClass, index: u32) -> Self {
        Self::consecutive(class, index, 1)
    }

    fn push(&mut self, piece: RegisterPiece) {
        assert!(
            (self.len as usize) < Self::CAPACITY,
            "a register-passed value spans at most the widest return bank"
        );
        self.pieces[self.len as usize] = piece;
        self.len += 1;
    }

    /// How many registers the value occupies.
    pub const fn len(&self) -> u32 {
        self.len
    }

    /// Whether the value occupies no register. Never true for a placed value;
    /// present so `len` reads as a length.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The pieces in ascending memory order.
    pub fn as_slice(&self) -> &[RegisterPiece] {
        &self.pieces[..self.len as usize]
    }

    /// The single bank every piece travels in, or `None` when the value is
    /// split across banks.
    pub fn uniform_class(&self) -> Option<CRegisterClass> {
        let first = self.as_slice().first()?.class;
        self.as_slice()
            .iter()
            .all(|piece| piece.class == first)
            .then_some(first)
    }

    /// The roster index of the first register, or `None` for an empty run.
    pub fn first_index(&self) -> Option<u32> {
        self.as_slice().first().map(|piece| piece.index)
    }
}

/// Where the pointer of an indirectly-passed argument lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerLocation {
    /// In general-purpose argument register `index`.
    Register {
        /// Roster index within the general-purpose argument registers.
        index: u32,
    },
    /// At `offset` bytes into the outgoing argument area, because the integer
    /// argument registers were exhausted.
    Stack {
        /// Byte offset from the base of the outgoing argument area.
        offset: u32,
    },
}

/// Where one argument of a lowered C signature lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgLocation {
    /// A zero-sized value: no register, no stack byte, no pointer.
    Omitted,
    /// In registers: one piece per eightbyte, or per homogeneous
    /// floating-point member, in ascending memory order — that is, C field
    /// order. A scalar is one piece.
    Registers {
        /// The registers the value occupies.
        pieces: RegisterPieces,
    },
    /// By value in the outgoing argument area: `size` bytes at `offset`, whose
    /// alignment the placement already satisfied.
    Stack {
        /// Byte offset from the base of the outgoing argument area.
        offset: u32,
        /// Footprint in bytes, per the convention's stacked-argument packing.
        size: u32,
        /// Alignment the offset satisfies.
        align: u32,
    },
    /// By pointer to a caller-owned copy of `size` bytes at `align` alignment
    /// (AAPCS64 section 6.8.2 B.4 / C.12).
    Indirect {
        /// Where the pointer itself travels.
        pointer: PointerLocation,
        /// The copy's byte size.
        size: u32,
        /// The copy's alignment.
        align: u32,
    },
}

/// One lowered argument: where it lives and what extension its value carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoweredArgument {
    /// Where the argument lives at the boundary.
    pub location: ArgLocation,
    /// The narrow-integer extension the value carries. Rue's canonical 64-bit
    /// form already satisfies it on the way out; an export thunk applies it to
    /// an incoming register, whose high bits no C caller is required to define.
    pub extension: ScalarAbiExtension,
}

/// How a lowered C signature's result crosses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoweredReturn {
    /// No value crosses back.
    Void,
    /// `count` result registers of `class`, low eightbyte first (C field order
    /// for an aggregate). `extension` is the operation that restores Rue's
    /// canonical 64-bit form to a narrow scalar whose high bits the callee left
    /// unspecified; it is [`ScalarAbiExtension::None`] for an aggregate.
    Registers {
        /// The register bank.
        class: CRegisterClass,
        /// How many consecutive result registers the value occupies.
        count: u32,
        /// The extension a returning scalar needs.
        extension: ScalarAbiExtension,
    },
    /// Through caller-provided storage whose address crosses as a hidden
    /// argument (sret).
    Sret {
        /// Where the hidden pointer travels.
        register: SretRegisterKind,
        /// Whether the callee echoes the pointer in the primary result
        /// register.
        echoed: bool,
        /// Byte size of the caller storage.
        size: u32,
        /// Alignment of the caller storage.
        align: u32,
    },
}

impl LoweredReturn {
    /// Whether the result crosses through caller storage.
    pub const fn uses_sret(self) -> bool {
        matches!(self, Self::Sret { .. })
    }
}

/// A complete C signature, lowered to placements.
///
/// Produced by [`lower_c_signature`] and consumed identically by the import
/// call planner (caller direction: write these places, then call) and the export
/// thunk (callee direction: read these places, then adapt to the native body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoweredSignature {
    convention: ConventionSpec,
    arguments: Vec<LoweredArgument>,
    ret: LoweredReturn,
    stack_bytes: u32,
}

impl LoweredSignature {
    /// The convention that produced this lowering.
    pub fn convention(&self) -> CallingConvention {
        self.convention.convention()
    }

    /// The description this lowering placed by. For a C row it is that row's
    /// own psABI entry; for the native convention it is the target's C entry
    /// with the native amendments (`ConventionSpec::native`).
    pub fn spec(&self) -> CConventionSpec {
        self.convention.spec()
    }

    /// The convention and description this lowering placed by, as one value.
    pub fn convention_spec(&self) -> ConventionSpec {
        self.convention
    }

    /// Every argument's placement, in source order.
    pub fn arguments(&self) -> &[LoweredArgument] {
        &self.arguments
    }

    /// The result's placement.
    pub fn ret(&self) -> LoweredReturn {
        self.ret
    }

    /// Bytes the caller reserves for the outgoing argument area, already
    /// rounded to the convention's call-boundary alignment. Includes the
    /// convention's shadow space.
    pub fn stack_bytes(&self) -> u32 {
        self.stack_bytes
    }

    /// Whether the hidden indirect-result pointer consumes the first ordinary
    /// integer argument register (SysV) rather than a dedicated one (AAPCS64).
    /// False when the result does not use sret at all.
    pub fn sret_in_argument_register(&self) -> bool {
        matches!(
            self.ret,
            LoweredReturn::Sret {
                register: SretRegisterKind::ArgumentRegister,
                ..
            }
        )
    }

    /// Total bytes of caller-owned copies the convention's by-reference
    /// aggregate rule requires, each already rounded to the call-boundary
    /// allocation granule by the caller that reserves it.
    pub fn indirect_copy_sizes(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.arguments
            .iter()
            .filter_map(|argument| match argument.location {
                ArgLocation::Indirect { size, align, .. } => Some((size, align)),
                _ => None,
            })
    }
}

/// Round `value` up to a multiple of the power-of-two `align`, saturating
/// rather than wrapping.
///
/// The mask form is exact only for a power of two, and every alignment reaching
/// here is one: [`Placer::stack_extent`] produces 1, 2, 4, or 8, and the final
/// rounding uses the convention's `call_stack_alignment`, which is 16 on every
/// row.
fn align_up(value: u64, align: u64) -> u64 {
    value.saturating_add(align - 1) & !(align - 1)
}

/// Saturating byte-count projection for the `u32` fields of the placements.
/// Semantic analysis rejects layouts anywhere near this bound before they reach
/// a boundary; saturating keeps the classifier total instead of panicking on a
/// value no program can construct.
fn saturate_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Number of eightbytes a `size`-byte value occupies.
fn eightbytes(size: u64) -> u32 {
    saturate_u32(size.div_ceil(8))
}

/// Where one value sits in the outgoing argument area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StackedPlacement {
    /// Byte offset from the base of the outgoing argument area.
    pub offset: u32,
    /// Footprint in bytes, per the convention's stacked-argument packing.
    pub size: u32,
    /// Alignment the offset satisfies.
    pub align: u32,
}

/// One call's outgoing argument area: the single rule for where a stacked
/// value lands under a convention's packing, and how many bytes the caller
/// reserves for it.
///
/// Every stacked placement in the tree claims through this cursor — the C
/// signature lowering below, and the native call planner in `rue-codegen` —
/// so the two cannot pack an argument area differently.
#[derive(Debug, Clone, Copy)]
pub struct ArgumentArea {
    spec: CConventionSpec,
    offset: u64,
}

impl ArgumentArea {
    /// An empty area under `spec`, positioned past any callee shadow space the
    /// row reserves at its base.
    pub fn new(spec: CConventionSpec) -> Self {
        Self {
            spec,
            offset: u64::from(spec.shadow_space_bytes),
        }
    }

    /// Claim the next `natural_size` bytes for a stacked scalar.
    ///
    /// Under [`StackedArgumentPacking::EightByteSlots`] the scalar starts at
    /// the next 8-byte boundary and occupies a whole eightbyte. Under Apple's
    /// [`NaturalSize`](StackedArgumentPacking::NaturalSize) amendment it
    /// occupies exactly its own width at its own alignment, so a stacked `i8`
    /// takes one byte and an `i16` starts at the next even offset.
    pub fn claim_scalar(&mut self, natural_size: u64) -> StackedPlacement {
        let (size, align) = match self.spec.stacked_argument_packing {
            StackedArgumentPacking::NaturalSize => {
                let bytes = natural_size.clamp(1, 8);
                (bytes, bytes)
            }
            StackedArgumentPacking::EightByteSlots => {
                (u64::from(eightbytes(natural_size)).saturating_mul(8), 8)
            }
        };
        self.claim(size, align)
    }

    /// Claim the next `size` bytes for a stacked aggregate.
    ///
    /// A composite keeps whole eightbytes at 8-byte alignment under every row:
    /// it crosses through its eightbyte image, and a byte-exact copy needs
    /// marshaling this path does not have. That one open Apple amendment is
    /// recorded in `docs/notes/ffi-abi-conformance-audit.md`.
    pub fn claim_aggregate(&mut self, size: u64) -> StackedPlacement {
        self.claim(u64::from(eightbytes(size)).saturating_mul(8), 8)
    }

    fn claim(&mut self, size: u64, align: u64) -> StackedPlacement {
        let offset = align_up(self.offset, align);
        self.offset = offset.saturating_add(size);
        StackedPlacement {
            offset: saturate_u32(offset),
            size: saturate_u32(size),
            align: saturate_u32(align),
        }
    }

    /// Bytes the caller reserves for the area, rounded to the convention's
    /// call-boundary alignment.
    pub fn reserved_bytes(&self) -> u32 {
        saturate_u32(align_up(
            self.offset,
            u64::from(self.spec.call_stack_alignment),
        ))
    }
}

/// Running state of the placement walk: the register rosters consumed so far
/// and the outgoing argument area's cursor.
struct Placer {
    spec: CConventionSpec,
    gp: u32,
    fp: u32,
    area: ArgumentArea,
}

impl Placer {
    fn new(spec: CConventionSpec) -> Self {
        Self {
            spec,
            gp: 0,
            fp: 0,
            area: ArgumentArea::new(spec),
        }
    }

    fn used(&self, class: CRegisterClass) -> u32 {
        match class {
            CRegisterClass::Gp => self.gp,
            CRegisterClass::Fp => self.fp,
        }
    }

    fn consume(&mut self, class: CRegisterClass, count: u32) {
        match class {
            CRegisterClass::Gp => self.gp = self.gp.saturating_add(count),
            CRegisterClass::Fp => self.fp = self.fp.saturating_add(count),
        }
    }

    /// Mark every remaining register of `class` used, so no later argument
    /// takes one (AAPCS64 rule C.11 after a composite fails to fit).
    fn exhaust(&mut self, class: CRegisterClass) {
        let all = self.spec.argument_registers(class);
        match class {
            CRegisterClass::Gp => self.gp = all,
            CRegisterClass::Fp => self.fp = all,
        }
    }

    /// Claim `count` consecutive registers of `class`, or `None` when the
    /// roster cannot hold the whole value. Register assignment is
    /// all-or-nothing: a two-eightbyte aggregate with one register left goes to
    /// memory entire, it does not split.
    fn claim_registers(&mut self, class: CRegisterClass, count: u32) -> Option<u32> {
        let first = self.used(class);
        if first.saturating_add(count) > self.spec.argument_registers(class) {
            return None;
        }
        self.consume(class, count);
        Some(first)
    }

    /// Claim one register per eightbyte of `classes`, each from its own bank,
    /// or `None` when either roster cannot hold its share.
    ///
    /// All-or-nothing spans both banks: a `{f64, i64}` argument with an SSE
    /// register but no integer register left goes to memory entire and spends
    /// neither.
    fn claim_classified_registers(&mut self, classes: &EightbyteClasses) -> Option<RegisterPieces> {
        for class in [CRegisterClass::Gp, CRegisterClass::Fp] {
            let needed = classes.registers_in(class);
            if self.used(class).saturating_add(needed) > self.spec.argument_registers(class) {
                return None;
            }
        }
        let mut pieces = RegisterPieces::consecutive(CRegisterClass::Gp, 0, 0);
        for eightbyte in classes.as_slice() {
            let class = eightbyte.register_class();
            let index = self.used(class);
            self.consume(class, 1);
            pieces.push(RegisterPiece { class, index });
        }
        Some(pieces)
    }
}

impl ArgLocation {
    /// A value in the outgoing argument area at `placement`.
    fn stacked(placement: StackedPlacement) -> Self {
        Self::Stack {
            offset: placement.offset,
            size: placement.size,
            align: placement.align,
        }
    }
}

/// Classify one whole `"C"` signature into placements.
///
/// `parameters` are the source parameters in declaration order, each already
/// reduced to the facts its plane projected and the mode it is passed under;
/// `result` is the return type's facts. `convention` must be a C row: the
/// pairing [`ConventionSpec::c`] is what asserts it, because a C boundary is
/// exactly a convention that describes itself.
pub fn lower_c_signature(
    convention: CallingConvention,
    parameters: &[(CAbiTypeFacts, ArgConvention)],
    result: CAbiTypeFacts,
) -> LoweredSignature {
    let pairing = ConventionSpec::c(convention);
    let ret = lower_return(&pairing.spec(), result);
    place_signature(pairing, parameters, ret)
}

/// Classify one whole *native* signature's arguments into placements, against
/// an already-decided return.
///
/// Arguments are placed by exactly the rules `pairing` describes, which for
/// [`ConventionSpec::native`] are the compilation target's own C rules
/// (ADR-0084). The return is an input rather than a decision because the native
/// return bank is wider than any C row's and no [`CConventionSpec`] describes
/// it: the caller classifies the return and hands it in, and this function
/// honors the one consequence a return has for argument placement — a native
/// sret pointer is the hidden first ordinary argument, so it shifts every user
/// argument one general-purpose register right.
pub fn lower_native_signature(
    pairing: ConventionSpec,
    parameters: &[(CAbiTypeFacts, ArgConvention)],
    result: LoweredReturn,
) -> LoweredSignature {
    place_signature(pairing, parameters, result)
}

/// Place every argument of a signature whose result crosses as `ret`, under
/// `pairing`.
///
/// The one placement walk both entry points run: the sret shift, the argument
/// classification in declaration order, and the outgoing argument area's final
/// size. What differs between a C boundary and a native one is only where `ret`
/// came from.
fn place_signature(
    pairing: ConventionSpec,
    parameters: &[(CAbiTypeFacts, ArgConvention)],
    ret: LoweredReturn,
) -> LoweredSignature {
    let spec = pairing.spec();
    let mut placer = Placer::new(spec);
    if ret.uses_sret() && spec.sret_pointer_in_argument_register() {
        // The hidden pointer is the hidden *first* argument, so it shifts every
        // user argument one general-purpose register right.
        placer.consume(CRegisterClass::Gp, 1);
    }

    let arguments = parameters
        .iter()
        .map(|(facts, mode)| {
            let facts = facts.as_argument(*mode);
            LoweredArgument {
                location: lower_argument(&mut placer, facts),
                extension: facts.extension(),
            }
        })
        .collect();

    LoweredSignature {
        convention: pairing,
        arguments,
        ret,
        stack_bytes: placer.area.reserved_bytes(),
    }
}

fn lower_argument(placer: &mut Placer, facts: CAbiTypeFacts) -> ArgLocation {
    match facts {
        CAbiTypeFacts::ZeroSized => ArgLocation::Omitted,
        CAbiTypeFacts::Scalar { kind, class } => {
            if let Some(index) = placer.claim_registers(class, 1) {
                return ArgLocation::Registers {
                    pieces: RegisterPieces::one(class, index),
                };
            }
            let natural = u64::from(kind.extension().natural_bytes());
            ArgLocation::stacked(placer.area.claim_scalar(natural))
        }
        CAbiTypeFacts::Aggregate {
            size,
            align,
            leaves,
        } => lower_aggregate_argument(placer, size, align, leaves),
    }
}

/// The homogeneous floating-point aggregate `size` bytes of `leaves` form, as a
/// member count, or `None` when they do not form one.
///
/// AAPCS64 section 6.8.2 admits one to four members of one fundamental
/// floating-point type. Requiring the members to fill the aggregate keeps a
/// padded composite on the ordinary composite rule, where the psABI's "the
/// same fundamental data type" reading is not in doubt.
fn homogeneous_float_members(size: u64, leaves: AggregateLeaves) -> Option<u32> {
    let (width, count) = leaves.homogeneous_floats()?;
    (count <= 4 && u64::from(width) * u64::from(count) == size).then_some(count)
}

fn lower_aggregate_argument(
    placer: &mut Placer,
    size: u64,
    align: u64,
    leaves: AggregateLeaves,
) -> ArgLocation {
    if size == 0 {
        return ArgLocation::Omitted;
    }
    let aapcs = placer.spec.aggregate_rule == AggregateClassificationRule::Aapcs64Composite;
    if aapcs && let Some(members) = homogeneous_float_members(size, leaves) {
        // AAPCS64 rule C.3: a homogeneous floating-point aggregate travels in
        // consecutive floating-point registers whatever its size, and when the
        // roster cannot hold it the whole aggregate is stacked and the roster
        // is exhausted, so no later argument takes a register it left free.
        if let Some(first) = placer.claim_registers(CRegisterClass::Fp, members) {
            return ArgLocation::Registers {
                pieces: RegisterPieces::consecutive(CRegisterClass::Fp, first, members),
            };
        }
        placer.exhaust(CRegisterClass::Fp);
        return ArgLocation::stacked(placer.area.claim_aggregate(size));
    }

    // SysV AMD64 section 3.2.3 makes an aggregate with an unaligned field
    // MEMORY class outright, whatever its size.
    let memory_by_alignment = !aapcs && leaves.has_unaligned_leaf();
    if !memory_by_alignment && size <= placer.spec.max_aggregate_register_bytes {
        let count = eightbytes(size);
        // AAPCS64 passes a composite that is not a homogeneous floating-point
        // aggregate in consecutive *integer* registers whatever its fields are
        // (section 6.8.2 rules C.13 and C.14); only SysV classifies an
        // aggregate's eightbytes by their leaves.
        let classes = if aapcs {
            EightbyteClasses::all_integer(count)
        } else {
            EightbyteClasses::classify(leaves, count)
        };
        if let Some(pieces) = placer.claim_classified_registers(&classes) {
            return ArgLocation::Registers { pieces };
        }
        // Registers exhausted: the whole aggregate goes to memory, keeping its
        // eightbyte image contiguous. AAPCS64 rule C.11 additionally sets the
        // next general-purpose register number to 8, so every later argument
        // is stacked too even though a register may remain; SysV AMD64 has no
        // such rule and a later scalar still takes the register the aggregate
        // could not fit in.
        if aapcs {
            placer.exhaust(CRegisterClass::Gp);
        }
        return ArgLocation::stacked(placer.area.claim_aggregate(size));
    }

    match placer.spec.aggregate_rule {
        AggregateClassificationRule::SysVEightbyte => {
            // MEMORY class: by value in the outgoing argument area, consuming
            // no register.
            let _ = align;
            ArgLocation::stacked(placer.area.claim_aggregate(size))
        }
        AggregateClassificationRule::Aapcs64Composite => {
            // One pointer to a caller-owned copy; the pointer itself is an
            // ordinary register-width argument and spills like one.
            let pointer = match placer.claim_registers(CRegisterClass::Gp, 1) {
                Some(index) => PointerLocation::Register { index },
                None => PointerLocation::Stack {
                    offset: placer.area.claim_scalar(8).offset,
                },
            };
            ArgLocation::Indirect {
                pointer,
                size: saturate_u32(size),
                align: saturate_u32(align),
            }
        }
    }
}

fn lower_return(spec: &CConventionSpec, result: CAbiTypeFacts) -> LoweredReturn {
    match result {
        CAbiTypeFacts::ZeroSized => LoweredReturn::Void,
        CAbiTypeFacts::Scalar { kind, class } => LoweredReturn::Registers {
            class,
            count: 1,
            extension: kind.extension(),
        },
        CAbiTypeFacts::Aggregate {
            size,
            align,
            leaves,
        } => {
            if size == 0 {
                return LoweredReturn::Void;
            }
            let aapcs = spec.aggregate_rule == AggregateClassificationRule::Aapcs64Composite;
            // AAPCS64 returns a homogeneous floating-point aggregate in
            // consecutive floating-point result registers, one per member.
            if aapcs
                && let Some(members) = homogeneous_float_members(size, leaves)
                && members <= spec.return_registers(CRegisterClass::Fp)
            {
                return LoweredReturn::Registers {
                    class: CRegisterClass::Fp,
                    count: members,
                    extension: ScalarAbiExtension::None,
                };
            }
            let memory_by_alignment = !aapcs && leaves.has_unaligned_leaf();
            if !memory_by_alignment && size <= spec.max_aggregate_register_bytes {
                let count = eightbytes(size);
                let classes = if aapcs {
                    EightbyteClasses::all_integer(count)
                } else {
                    EightbyteClasses::classify(leaves, count)
                };
                return LoweredReturn::Registers {
                    class: classes.uniform_register_class().expect(
                        "a result split across register banks needs the per-piece \
                         return locations RUE-2038 gives it",
                    ),
                    count: classes.len(),
                    extension: ScalarAbiExtension::None,
                };
            }
            LoweredReturn::Sret {
                register: spec.sret_register,
                echoed: spec.sret_pointer_echoed_in_result_register,
                size: saturate_u32(size),
                align: saturate_u32(align),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_target::Target;

    const SYSV: CallingConvention = CallingConvention::X86_64SysV;
    const AAPCS: CallingConvention = CallingConvention::Aarch64Aapcs;
    const DARWIN: CallingConvention = CallingConvention::Aarch64AapcsDarwin;

    fn scalar(kind: CAbiScalarKind) -> (CAbiTypeFacts, ArgConvention) {
        (
            CAbiTypeFacts::Scalar {
                kind,
                class: CRegisterClass::Gp,
            },
            ArgConvention::ByValue,
        )
    }

    fn word() -> (CAbiTypeFacts, ArgConvention) {
        scalar(CAbiScalarKind::RegisterWidth)
    }

    fn aggregate(size: u64, align: u64) -> (CAbiTypeFacts, ArgConvention) {
        (
            CAbiTypeFacts::integer_aggregate(size, align),
            ArgConvention::ByValue,
        )
    }

    /// An aggregate whose leaves are exactly `leaves`, sized to cover them.
    fn leafy(leaves: &[CAbiLeaf]) -> (CAbiTypeFacts, ArgConvention) {
        (leafy_facts(leaves), ArgConvention::ByValue)
    }

    fn leafy_facts(leaves: &[CAbiLeaf]) -> CAbiTypeFacts {
        let size = leaves
            .iter()
            .map(|leaf| leaf.offset + leaf.width)
            .max()
            .unwrap_or(0);
        let align = leaves.iter().map(|leaf| leaf.width).max().unwrap_or(1);
        CAbiTypeFacts::Aggregate {
            size,
            align,
            leaves: AggregateLeaves::from_leaves(size, leaves.iter().copied()),
        }
    }

    fn registers(first_index: u32, count: u32) -> ArgLocation {
        ArgLocation::Registers {
            pieces: RegisterPieces::consecutive(CRegisterClass::Gp, first_index, count),
        }
    }

    fn fp_registers(first_index: u32, count: u32) -> ArgLocation {
        ArgLocation::Registers {
            pieces: RegisterPieces::consecutive(CRegisterClass::Fp, first_index, count),
        }
    }

    fn pieces(location: ArgLocation) -> Vec<(CRegisterClass, u32)> {
        match location {
            ArgLocation::Registers { pieces } => pieces
                .as_slice()
                .iter()
                .map(|piece| (piece.class, piece.index))
                .collect(),
            other => panic!("expected registers, got {other:?}"),
        }
    }

    #[test]
    fn scalars_fill_the_roster_then_spill_to_the_argument_area() {
        let params = vec![word(); 9];
        let sysv = lower_c_signature(SYSV, &params, CAbiTypeFacts::ZeroSized);
        for (index, argument) in sysv.arguments().iter().take(6).enumerate() {
            assert_eq!(argument.location, registers(index as u32, 1));
        }
        assert_eq!(
            sysv.arguments()[6].location,
            ArgLocation::Stack {
                offset: 0,
                size: 8,
                align: 8
            }
        );
        assert_eq!(
            sysv.arguments()[8].location,
            ArgLocation::Stack {
                offset: 16,
                size: 8,
                align: 8
            }
        );
        assert_eq!(sysv.stack_bytes(), 32);

        let aapcs = lower_c_signature(AAPCS, &params, CAbiTypeFacts::ZeroSized);
        assert_eq!(aapcs.arguments()[7].location, registers(7, 1));
        assert_eq!(
            aapcs.arguments()[8].location,
            ArgLocation::Stack {
                offset: 0,
                size: 8,
                align: 8
            }
        );
        assert_eq!(aapcs.stack_bytes(), 16);
    }

    #[test]
    fn the_apple_row_packs_stacked_scalars_at_natural_size() {
        let mut params = vec![word(); 8];
        params.extend([
            scalar(CAbiScalarKind::I8),
            scalar(CAbiScalarKind::I16),
            scalar(CAbiScalarKind::I32),
            word(),
        ]);
        let linux = lower_c_signature(AAPCS, &params, CAbiTypeFacts::ZeroSized);
        let darwin = lower_c_signature(DARWIN, &params, CAbiTypeFacts::ZeroSized);
        let offsets = |signature: &LoweredSignature| {
            signature.arguments()[8..]
                .iter()
                .map(|argument| match argument.location {
                    ArgLocation::Stack {
                        offset,
                        size,
                        align,
                    } => (offset, size, align),
                    other => panic!("expected a stacked argument, got {other:?}"),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            offsets(&linux),
            vec![(0, 8, 8), (8, 8, 8), (16, 8, 8), (24, 8, 8)]
        );
        assert_eq!(
            offsets(&darwin),
            vec![(0, 1, 1), (2, 2, 2), (4, 4, 4), (8, 8, 8)]
        );
        assert_eq!(linux.stack_bytes(), 32);
        assert_eq!(darwin.stack_bytes(), 16);
    }

    #[test]
    fn aggregates_pack_into_registers_up_to_two_eightbytes() {
        for convention in [SYSV, AAPCS, DARWIN] {
            let signature = lower_c_signature(
                convention,
                &[aggregate(8, 4), aggregate(16, 8), aggregate(12, 4)],
                CAbiTypeFacts::ZeroSized,
            );
            assert_eq!(signature.arguments()[0].location, registers(0, 1));
            assert_eq!(signature.arguments()[1].location, registers(1, 2));
            // A non-multiple-of-8 size rounds up to whole eightbytes.
            assert_eq!(signature.arguments()[2].location, registers(3, 2));
            assert_eq!(signature.stack_bytes(), 0);
        }
    }

    #[test]
    fn an_oversized_aggregate_follows_its_conventions_rule() {
        let sysv = lower_c_signature(SYSV, &[aggregate(24, 8)], CAbiTypeFacts::ZeroSized);
        assert_eq!(
            sysv.arguments()[0].location,
            ArgLocation::Stack {
                offset: 0,
                size: 24,
                align: 8
            }
        );
        assert_eq!(sysv.stack_bytes(), 32);
        for convention in [AAPCS, DARWIN] {
            let aapcs =
                lower_c_signature(convention, &[aggregate(24, 8)], CAbiTypeFacts::ZeroSized);
            assert_eq!(
                aapcs.arguments()[0].location,
                ArgLocation::Indirect {
                    pointer: PointerLocation::Register { index: 0 },
                    size: 24,
                    align: 8
                }
            );
            assert_eq!(aapcs.stack_bytes(), 0);
        }
    }

    #[test]
    fn a_sysv_memory_argument_consumes_no_register() {
        // SysV MEMORY class is byval-on-stack and leaves the integer roster
        // untouched, so the scalar after it still lands in `rsi`.
        let signature = lower_c_signature(
            SYSV,
            &[word(), aggregate(24, 8), word()],
            CAbiTypeFacts::ZeroSized,
        );
        assert_eq!(signature.arguments()[0].location, registers(0, 1));
        assert_eq!(signature.arguments()[2].location, registers(1, 1));
    }

    #[test]
    fn register_assignment_of_an_aggregate_is_all_or_nothing() {
        // Five words leave one integer register; a two-eightbyte aggregate does
        // not split across the boundary, it goes to memory entire, and the word
        // after it still takes the last register.
        let signature = lower_c_signature(
            SYSV,
            &[
                word(),
                word(),
                word(),
                word(),
                word(),
                aggregate(16, 8),
                word(),
            ],
            CAbiTypeFacts::ZeroSized,
        );
        assert_eq!(
            signature.arguments()[5].location,
            ArgLocation::Stack {
                offset: 0,
                size: 16,
                align: 8
            }
        );
        assert_eq!(signature.arguments()[6].location, registers(5, 1));
    }

    #[test]
    fn an_aapcs64_composite_that_does_not_fit_exhausts_the_register_roster() {
        // Seven words leave `x7`; a two-eightbyte composite cannot fit, so it
        // goes to the stack entire and, per AAPCS64 rule C.11, the next
        // general-purpose register number becomes 8: the word after it is
        // stacked behind the composite rather than taking the free `x7`.
        for convention in [AAPCS, DARWIN] {
            let signature = lower_c_signature(
                convention,
                &[
                    word(),
                    word(),
                    word(),
                    word(),
                    word(),
                    word(),
                    word(),
                    aggregate(16, 8),
                    word(),
                ],
                CAbiTypeFacts::ZeroSized,
            );
            assert_eq!(
                signature.arguments()[7].location,
                ArgLocation::Stack {
                    offset: 0,
                    size: 16,
                    align: 8
                }
            );
            assert_eq!(
                signature.arguments()[8].location,
                ArgLocation::Stack {
                    offset: 16,
                    size: 8,
                    align: 8
                },
                "{convention}: the register the composite could not use stays unused"
            );
            assert_eq!(signature.stack_bytes(), 32);
        }
    }

    #[test]
    fn an_aapcs64_by_reference_pointer_spills_like_any_pointer() {
        let mut params = vec![word(); 8];
        params.push(aggregate(24, 8));
        let signature = lower_c_signature(AAPCS, &params, CAbiTypeFacts::ZeroSized);
        assert_eq!(
            signature.arguments()[8].location,
            ArgLocation::Indirect {
                pointer: PointerLocation::Stack { offset: 0 },
                size: 24,
                align: 8
            }
        );
        assert_eq!(signature.stack_bytes(), 16);
    }

    #[test]
    fn a_sysv_sret_pointer_shifts_user_arguments_and_aapcs64_does_not() {
        let big = CAbiTypeFacts::integer_aggregate(24, 8);
        let sysv = lower_c_signature(SYSV, &[word(), word()], big);
        assert_eq!(
            sysv.ret(),
            LoweredReturn::Sret {
                register: SretRegisterKind::ArgumentRegister,
                echoed: true,
                size: 24,
                align: 8
            }
        );
        assert!(sysv.sret_in_argument_register());
        assert_eq!(sysv.arguments()[0].location, registers(1, 1));
        assert_eq!(sysv.arguments()[1].location, registers(2, 1));

        let aapcs = lower_c_signature(AAPCS, &[word(), word()], big);
        assert_eq!(
            aapcs.ret(),
            LoweredReturn::Sret {
                register: SretRegisterKind::DedicatedRegister,
                echoed: false,
                size: 24,
                align: 8
            }
        );
        assert!(!aapcs.sret_in_argument_register());
        assert_eq!(aapcs.arguments()[0].location, registers(0, 1));
    }

    #[test]
    fn returns_follow_the_scalar_extension_and_eightbyte_tables() {
        let signature = lower_c_signature(
            SYSV,
            &[],
            CAbiTypeFacts::Scalar {
                kind: CAbiScalarKind::I16,
                class: CRegisterClass::Gp,
            },
        );
        assert_eq!(
            signature.ret(),
            LoweredReturn::Registers {
                class: CRegisterClass::Gp,
                count: 1,
                extension: ScalarAbiExtension::Signed { from_bits: 16 }
            }
        );
        for (size, count) in [(1_u64, 1_u32), (8, 1), (9, 2), (16, 2)] {
            let signature = lower_c_signature(SYSV, &[], CAbiTypeFacts::integer_aggregate(size, 8));
            assert_eq!(
                signature.ret(),
                LoweredReturn::Registers {
                    class: CRegisterClass::Gp,
                    count,
                    extension: ScalarAbiExtension::None
                }
            );
        }
        assert_eq!(
            lower_c_signature(SYSV, &[], CAbiTypeFacts::ZeroSized).ret(),
            LoweredReturn::Void
        );
    }

    #[test]
    fn a_by_reference_argument_is_one_pointer_whatever_it_points_at() {
        let signature = lower_c_signature(
            SYSV,
            &[(
                CAbiTypeFacts::integer_aggregate(4096, 8),
                ArgConvention::ByReference,
            )],
            CAbiTypeFacts::ZeroSized,
        );
        assert_eq!(signature.arguments()[0].location, registers(0, 1));
        assert_eq!(signature.stack_bytes(), 0);
    }

    #[test]
    fn every_eightbyte_of_the_integer_only_surface_is_the_integer_class() {
        let classes = EightbyteClasses::all_integer(2);
        assert_eq!(classes.len(), 2);
        assert!(!classes.is_empty());
        assert_eq!(
            classes.as_slice(),
            &[EightbyteClass::Integer, EightbyteClass::Integer]
        );
        assert_eq!(
            classes.uniform_register_class(),
            Some(CRegisterClass::Gp),
            "an integer-only classification names the general-purpose bank"
        );
        assert_eq!(classes.registers_in(CRegisterClass::Gp), 2);
        assert_eq!(classes.registers_in(CRegisterClass::Fp), 0);
        assert_eq!(EightbyteClass::Sse.register_class(), CRegisterClass::Fp);
    }

    #[test]
    fn an_eightbyte_is_sse_only_when_every_leaf_over_it_is_a_float() {
        let mixed = AggregateLeaves::from_leaves(16, [CAbiLeaf::f64(0), CAbiLeaf::integer(8, 8)]);
        assert_eq!(mixed.eightbyte_class(0), EightbyteClass::Sse);
        assert_eq!(mixed.eightbyte_class(1), EightbyteClass::Integer);
        assert_eq!(mixed.homogeneous_floats(), None);

        // Two `f32`s share one eightbyte and leave it SSE; a narrow integer
        // beside one of them pulls that eightbyte into the integer bank.
        let packed = AggregateLeaves::from_leaves(8, [CAbiLeaf::f32(0), CAbiLeaf::f32(4)]);
        assert_eq!(packed.eightbyte_class(0), EightbyteClass::Sse);
        assert_eq!(packed.homogeneous_floats(), Some((4, 2)));
        let contaminated =
            AggregateLeaves::from_leaves(8, [CAbiLeaf::f32(0), CAbiLeaf::integer(4, 4)]);
        assert_eq!(contaminated.eightbyte_class(0), EightbyteClass::Integer);
        assert_eq!(contaminated.homogeneous_floats(), None);

        // An eightbyte no leaf reaches stays in the integer bank, and so does
        // every eightbyte of an aggregate whose leaves a plane did not
        // enumerate.
        assert_eq!(
            AggregateLeaves::all_integer(16).eightbyte_class(1),
            EightbyteClass::Integer
        );
        assert_eq!(
            AggregateLeaves::from_leaves(16, []).eightbyte_class(0),
            EightbyteClass::Integer
        );
        assert!(!AggregateLeaves::all_integer(16).has_unaligned_leaf());
        assert!(
            AggregateLeaves::from_leaves(16, [CAbiLeaf::integer(3, 4)]).has_unaligned_leaf(),
            "a leaf its own width does not divide is unaligned"
        );
    }

    #[test]
    fn a_two_eightbyte_struct_splits_across_banks_on_sysv_and_not_on_aapcs64() {
        for leaves in [
            [CAbiLeaf::f64(0), CAbiLeaf::integer(8, 8)],
            [CAbiLeaf::integer(0, 8), CAbiLeaf::f64(8)],
        ] {
            let argument = leafy(&leaves);
            let sysv = lower_c_signature(SYSV, &[argument], CAbiTypeFacts::ZeroSized);
            let expected = leaves
                .iter()
                .map(|leaf| match leaf.kind {
                    CAbiLeafKind::Integer => (CRegisterClass::Gp, 0),
                    _ => (CRegisterClass::Fp, 0),
                })
                .collect::<Vec<_>>();
            assert_eq!(
                pieces(sysv.arguments()[0].location),
                expected,
                "SysV classifies each eightbyte by its own leaves"
            );
            for convention in [AAPCS, DARWIN] {
                let aapcs = lower_c_signature(convention, &[argument], CAbiTypeFacts::ZeroSized);
                assert_eq!(
                    aapcs.arguments()[0].location,
                    registers(0, 2),
                    "{convention}: a composite that is not an HFA travels in \
                     integer registers"
                );
            }
        }
    }

    #[test]
    fn a_float_pair_is_one_sse_eightbyte_on_sysv_and_a_two_member_hfa_on_aapcs64() {
        let argument = leafy(&[CAbiLeaf::f32(0), CAbiLeaf::f32(4)]);
        let sysv = lower_c_signature(SYSV, &[argument], CAbiTypeFacts::ZeroSized);
        assert_eq!(sysv.arguments()[0].location, fp_registers(0, 1));
        for convention in [AAPCS, DARWIN] {
            let aapcs = lower_c_signature(convention, &[argument], CAbiTypeFacts::ZeroSized);
            assert_eq!(aapcs.arguments()[0].location, fp_registers(0, 2));
        }
    }

    #[test]
    fn three_doubles_are_an_aapcs64_hfa_and_sysv_memory() {
        let argument = leafy(&[CAbiLeaf::f64(0), CAbiLeaf::f64(8), CAbiLeaf::f64(16)]);
        let sysv = lower_c_signature(SYSV, &[argument], CAbiTypeFacts::ZeroSized);
        assert_eq!(
            sysv.arguments()[0].location,
            ArgLocation::Stack {
                offset: 0,
                size: 24,
                align: 8
            },
            "24 bytes exceeds the two-eightbyte register budget: MEMORY class"
        );
        for convention in [AAPCS, DARWIN] {
            let aapcs = lower_c_signature(convention, &[argument], CAbiTypeFacts::ZeroSized);
            assert_eq!(
                aapcs.arguments()[0].location,
                fp_registers(0, 3),
                "{convention}: rule C.3 takes an HFA in registers past 16 bytes"
            );
        }
        // Five members are not an HFA: over four, the ordinary composite rule
        // applies and 40 bytes is a by-reference copy.
        let five = leafy(&[
            CAbiLeaf::f64(0),
            CAbiLeaf::f64(8),
            CAbiLeaf::f64(16),
            CAbiLeaf::f64(24),
            CAbiLeaf::f64(32),
        ]);
        assert_eq!(
            lower_c_signature(AAPCS, &[five], CAbiTypeFacts::ZeroSized).arguments()[0].location,
            ArgLocation::Indirect {
                pointer: PointerLocation::Register { index: 0 },
                size: 40,
                align: 8
            }
        );
    }

    #[test]
    fn an_hfa_that_does_not_fit_exhausts_the_floating_point_roster() {
        // Six single-`f64` arguments leave two floating-point registers; a
        // three-member HFA cannot fit, so it stacks entire and rule C.3
        // exhausts the roster: the single float after it stacks too, rather
        // than taking a register the HFA left free.
        let one = leafy(&[CAbiLeaf::f64(0)]);
        let three = leafy(&[CAbiLeaf::f64(0), CAbiLeaf::f64(8), CAbiLeaf::f64(16)]);
        let mut params = vec![one; 6];
        params.push(three);
        params.push(one);
        let signature = lower_c_signature(AAPCS, &params, CAbiTypeFacts::ZeroSized);
        assert_eq!(signature.arguments()[5].location, fp_registers(5, 1));
        assert_eq!(
            signature.arguments()[6].location,
            ArgLocation::Stack {
                offset: 0,
                size: 24,
                align: 8
            }
        );
        assert_eq!(
            signature.arguments()[7].location,
            ArgLocation::Stack {
                offset: 24,
                size: 8,
                align: 8
            }
        );
        assert_eq!(signature.stack_bytes(), 32);
    }

    #[test]
    fn a_split_aggregate_takes_both_banks_or_neither() {
        // SysV: five integer words and eight doubles leave one integer
        // register and no SSE register, so a `{f64, i64}` argument goes to
        // memory entire and spends neither bank; the integer word after it
        // still takes the register it left.
        let split = leafy(&[CAbiLeaf::f64(0), CAbiLeaf::integer(8, 8)]);
        let one = leafy(&[CAbiLeaf::f64(0)]);
        let mut params = vec![word(); 5];
        params.extend(vec![one; 8]);
        params.push(split);
        params.push(word());
        let signature = lower_c_signature(SYSV, &params, CAbiTypeFacts::ZeroSized);
        assert_eq!(
            signature.arguments()[13].location,
            ArgLocation::Stack {
                offset: 0,
                size: 16,
                align: 8
            }
        );
        assert_eq!(signature.arguments()[14].location, registers(5, 1));
    }

    #[test]
    fn a_native_signature_places_floats_by_its_targets_row() {
        // The native convention is the one caller that has floats to place, and
        // it places them by exactly the rules above.
        let pair = leafy(&[CAbiLeaf::f32(0), CAbiLeaf::f32(4)]);
        let sysv = lower_native_signature(
            ConventionSpec::native(Target::X86_64Linux),
            &[pair],
            LoweredReturn::Void,
        );
        assert_eq!(sysv.arguments()[0].location, fp_registers(0, 1));
        for target in [Target::Aarch64Linux, Target::Aarch64Macos] {
            let native = lower_native_signature(
                ConventionSpec::native(target),
                &[pair],
                LoweredReturn::Void,
            );
            assert_eq!(native.arguments()[0].location, fp_registers(0, 2));
        }
    }

    #[test]
    fn a_returned_aggregate_classifies_by_the_same_leaves() {
        // The return path reads the same facts: an SSE eightbyte returns in the
        // floating-point bank, and AAPCS64 returns an HFA one register per
        // member.
        let pair = leafy_facts(&[CAbiLeaf::f32(0), CAbiLeaf::f32(4)]);
        assert_eq!(
            lower_c_signature(SYSV, &[], pair).ret(),
            LoweredReturn::Registers {
                class: CRegisterClass::Fp,
                count: 1,
                extension: ScalarAbiExtension::None
            }
        );
        assert_eq!(
            lower_c_signature(AAPCS, &[], pair).ret(),
            LoweredReturn::Registers {
                class: CRegisterClass::Fp,
                count: 2,
                extension: ScalarAbiExtension::None
            }
        );
        let three = leafy_facts(&[CAbiLeaf::f64(0), CAbiLeaf::f64(8), CAbiLeaf::f64(16)]);
        assert_eq!(
            lower_c_signature(AAPCS, &[], three).ret(),
            LoweredReturn::Registers {
                class: CRegisterClass::Fp,
                count: 3,
                extension: ScalarAbiExtension::None
            }
        );
        assert!(
            lower_c_signature(SYSV, &[], three).ret().uses_sret(),
            "24 bytes exceeds SysV's two-eightbyte result budget"
        );
    }

    #[test]
    fn the_argument_area_packs_by_its_rows_rule_and_agrees_on_whole_eightbytes() {
        // The Apple row packs a stacked scalar at its own width; the other rows
        // give it a whole eightbyte. The rows therefore differ only for a
        // value narrower than eight bytes, which is why a native ABI slot —
        // canonically 64-bit-extended, so eight bytes wide — lands in the same
        // place on every row.
        let narrow = |target: Target| {
            let mut area = ArgumentArea::new(ConventionSpec::native(target).spec());
            let first = area.claim_scalar(1);
            let second = area.claim_scalar(2);
            (first, second, area.reserved_bytes())
        };
        let whole = |target: Target| {
            let mut area = ArgumentArea::new(ConventionSpec::native(target).spec());
            let first = area.claim_scalar(8);
            let second = area.claim_scalar(8);
            (first, second, area.reserved_bytes())
        };
        let slot = |offset: u32| StackedPlacement {
            offset,
            size: 8,
            align: 8,
        };
        for target in [Target::X86_64Linux, Target::Aarch64Linux] {
            assert_eq!(narrow(target), (slot(0), slot(8), 16));
        }
        assert_eq!(
            narrow(Target::Aarch64Macos),
            (
                StackedPlacement {
                    offset: 0,
                    size: 1,
                    align: 1
                },
                StackedPlacement {
                    offset: 2,
                    size: 2,
                    align: 2
                },
                16
            )
        );
        for target in Target::all() {
            assert_eq!(
                whole(*target),
                (slot(0), slot(8), 16),
                "{target:?}: an eight-byte value is packed the same by every row"
            );
        }
    }

    #[test]
    fn oversized_byte_counts_saturate_rather_than_wrapping() {
        // Semantic analysis rejects layouts anywhere near this bound before they
        // reach a boundary; the classifier stays total rather than panicking on
        // a value no program can construct.
        let huge = u64::from(u32::MAX) + 9;
        let signature = lower_c_signature(
            SYSV,
            &[aggregate(huge, 8)],
            CAbiTypeFacts::integer_aggregate(huge, 8),
        );
        assert_eq!(
            signature.arguments()[0].location,
            ArgLocation::Stack {
                offset: 0,
                size: u32::MAX,
                align: 8
            }
        );
        assert_eq!(
            signature.ret(),
            LoweredReturn::Sret {
                register: SretRegisterKind::ArgumentRegister,
                echoed: true,
                size: u32::MAX,
                align: 8
            }
        );
        assert_eq!(signature.stack_bytes(), u32::MAX);
    }

    #[test]
    #[should_panic(expected = "a C pairing names a psABI row")]
    fn the_native_convention_is_not_a_c_boundary() {
        let _ = lower_c_signature(CallingConvention::Rue, &[], CAbiTypeFacts::ZeroSized);
    }

    #[test]
    fn a_void_native_signature_places_arguments_exactly_as_its_targets_c_row() {
        // ADR-0084's argument rule, stated as an equality: with nothing forced
        // by the return, every native placement is the target C placement.
        let params = vec![
            word(),
            aggregate(16, 8),
            aggregate(24, 8),
            word(),
            word(),
            word(),
            word(),
            word(),
            word(),
            scalar(CAbiScalarKind::I8),
        ];
        for target in Target::all() {
            let c = lower_c_signature(
                target.c_calling_convention(),
                &params,
                CAbiTypeFacts::ZeroSized,
            );
            let native = lower_native_signature(
                ConventionSpec::native(*target),
                &params,
                LoweredReturn::Void,
            );
            assert_eq!(
                native.arguments(),
                c.arguments(),
                "{target:?}: the native convention places arguments by its C row"
            );
            assert_eq!(native.stack_bytes(), c.stack_bytes());
            assert_eq!(native.convention(), CallingConvention::Rue);
            assert_eq!(native.ret(), LoweredReturn::Void);
            assert_eq!(
                native.spec().gp_argument_registers,
                c.spec().gp_argument_registers
            );
        }
    }

    #[test]
    fn a_native_sret_shifts_user_arguments_on_every_target() {
        // The native hidden indirect-result pointer is an ordinary first
        // argument on both architectures, so it costs the first
        // general-purpose argument register everywhere — unlike AAPCS64's
        // dedicated `x8`, which leaves user arguments at roster index 0.
        let sret = LoweredReturn::Sret {
            register: SretRegisterKind::ArgumentRegister,
            echoed: false,
            size: 64,
            align: 8,
        };
        for target in Target::all() {
            let native =
                lower_native_signature(ConventionSpec::native(*target), &[word(), word()], sret);
            assert!(native.sret_in_argument_register());
            assert_eq!(native.arguments()[0].location, registers(1, 1));
            assert_eq!(native.arguments()[1].location, registers(2, 1));
        }
        // The same shape at the AAPCS64 C boundary keeps `x0` for the user's
        // first argument, which is the difference the pairing carries.
        let c = lower_c_signature(
            CallingConvention::Aarch64Aapcs,
            &[word(), word()],
            CAbiTypeFacts::integer_aggregate(64, 8),
        );
        assert!(!c.sret_in_argument_register());
        assert_eq!(c.arguments()[0].location, registers(0, 1));
    }

    #[test]
    fn a_native_signature_fills_its_targets_roster_before_the_argument_area() {
        // Nine words: SysV stacks the last three, AAPCS64 the last one, and the
        // Apple row packs a narrow stacked scalar at its natural size just as
        // it does at its C boundary.
        let params = vec![word(); 9];
        let native = |target: Target| {
            lower_native_signature(ConventionSpec::native(target), &params, LoweredReturn::Void)
        };
        let sysv = native(Target::X86_64Linux);
        assert_eq!(sysv.arguments()[5].location, registers(5, 1));
        assert_eq!(
            sysv.arguments()[6].location,
            ArgLocation::Stack {
                offset: 0,
                size: 8,
                align: 8
            }
        );
        assert_eq!(sysv.stack_bytes(), 32);
        let aapcs = native(Target::Aarch64Linux);
        assert_eq!(aapcs.arguments()[7].location, registers(7, 1));
        assert_eq!(
            aapcs.arguments()[8].location,
            ArgLocation::Stack {
                offset: 0,
                size: 8,
                align: 8
            }
        );

        let mut narrow = vec![word(); 8];
        narrow.push(scalar(CAbiScalarKind::I8));
        let darwin = lower_native_signature(
            ConventionSpec::native(Target::Aarch64Macos),
            &narrow,
            LoweredReturn::Void,
        );
        assert_eq!(
            darwin.arguments()[8].location,
            ArgLocation::Stack {
                offset: 0,
                size: 1,
                align: 1
            }
        );
    }

    #[test]
    fn a_native_register_return_costs_no_argument_register() {
        // Only an sret return shifts arguments; a return that fits the native
        // bank leaves the whole roster to the user's arguments.
        for target in Target::all() {
            let native = lower_native_signature(
                ConventionSpec::native(*target),
                &[word()],
                LoweredReturn::Registers {
                    class: CRegisterClass::Gp,
                    count: 6,
                    extension: ScalarAbiExtension::None,
                },
            );
            assert_eq!(native.arguments()[0].location, registers(0, 1));
        }
    }
}
