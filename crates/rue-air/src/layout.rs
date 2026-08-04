//! Canonical physical type layout (ADR-0052).
//!
//! One authority for the physical memory layout of every materializable type:
//! its [`size`](Layout::size), [`alignment`](Layout::alignment),
//! [`stride`](Layout::stride), and per-[`kind`](LayoutKind) structure — scalar,
//! array element stride, struct field offsets, and enum tag/payload placement.
//! Semantic analysis (`@size_of`, `@align_of`, `@offset_of`) and both
//! code-generation backends consume this query instead of rediscovering a
//! bytes-per-slot conversion. The slot decomposition that feeds it is the type
//! intern pool's `abi_slot_count`, so a layout's `size` is that slot count times
//! [`SLOT_BYTES`] by construction.
//!
//! The current physical model is the flattened eight-byte ABI slot: every
//! logical slot occupies [`SLOT_BYTES`] bytes at [`SLOT_BYTES`]-byte alignment,
//! `stride == size`, and a zero-sized type has size and stride zero with
//! one-byte alignment. The LP64 scalar table is identical across every supported
//! target (x86-64 and AArch64 agree), so the query carries no target
//! discriminator yet; the compact-representation phase introduces per-target
//! natural scalar widths through this same authority.

/// The byte width of one logical ABI/storage slot.
///
/// The single bytes-per-slot fact for the whole compiler: sema layout
/// intrinsics, code-generation size and offset math, and allocation scaling all
/// derive their byte quantities from this constant so they cannot drift apart.
pub const SLOT_BYTES: u64 = 8;

/// Call-boundary stack alignment shared by both supported targets.
pub const STACK_FRAME_ALIGNMENT: u64 = 16;

/// Largest aligned frame or transient call area representable by the signed
/// 32-bit displacements used throughout both machine backends.
///
/// Rounding the architectural `i32::MAX` bound down keeps the subsequent
/// alignment operation inside the same representable range. Published as the
/// cumulative function-storage ceiling in spec C.4:3; exceeding it is
/// diagnosed as E0907 rather than wrapping, per spec C.1:2.
pub const MAX_FUNCTION_FRAME_BYTES: u64 =
    (i32::MAX as u64 / STACK_FRAME_ALIGNMENT) * STACK_FRAME_ALIGNMENT;

/// [`MAX_FUNCTION_FRAME_BYTES`] expressed as slot-shaped frame cells.
pub const MAX_FUNCTION_FRAME_SLOTS: u64 = MAX_FUNCTION_FRAME_BYTES / SLOT_BYTES;

/// Add one slot-shaped region to a cumulative function-frame total.
///
/// This is the semantic-analysis boundary: it rejects cumulative locals or
/// parameters before their per-slot metadata is allocated.
#[inline]
pub fn checked_function_frame_slots(current: u32, additional: u32) -> Option<u32> {
    let total = u64::from(current).checked_add(u64::from(additional))?;
    (total <= MAX_FUNCTION_FRAME_SLOTS).then_some(total as u32)
}

/// The physical layout of a materializable type.
///
/// `size` counts every byte the value occupies including tail padding, `stride`
/// is the distance between consecutive elements of an array of this type (equal
/// to `size` under the current model), and `alignment` is the byte alignment a
/// pointer to the value must satisfy. `kind` carries the per-shape detail an
/// addressing or marshaling consumer needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// Total size in bytes, including tail padding.
    pub size: u64,
    /// Required byte alignment.
    pub alignment: u64,
    /// Distance in bytes between consecutive array elements of this type.
    pub stride: u64,
    /// Per-shape layout detail.
    pub kind: LayoutKind,
}

/// The shape-specific portion of a [`Layout`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutKind {
    /// A scalar occupying one storage cell: integers, `bool`, raw pointers, the
    /// error-recovery type, and the zero-sized `unit`/`never`-style types.
    Scalar,
    /// A contiguous array of `count` elements each laid out as `element`.
    Array {
        /// Layout of a single element; array indexing strides by
        /// `element.stride`.
        element: Box<Layout>,
        /// Number of elements.
        count: u64,
    },
    /// A struct laid out in declaration order.
    Struct {
        /// Byte offset of each field, parallel to the struct's declared fields.
        field_offsets: Vec<u64>,
        /// Byte ranges occupied by padding rather than a field. Empty under the
        /// current packed slot model.
        padding_ranges: Vec<PaddingRange>,
    },
    /// A tagged enum: a discriminant followed by the largest variant payload.
    Enum {
        /// Layout of the discriminant tag.
        tag: Box<Layout>,
        /// Byte offset at which every variant's payload begins.
        payload_offset: u64,
        /// Byte offsets of each variant's payload fields, indexed by variant in
        /// declaration order.
        variants: Vec<Vec<u64>>,
    },
}

/// A half-open `[start, end)` byte range occupied by padding rather than a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaddingRange {
    /// First padding byte, relative to the aggregate base.
    pub start: u64,
    /// One past the last padding byte.
    pub end: u64,
}
