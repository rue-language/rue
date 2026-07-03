//! AIR instruction definitions.
//!
//! Like RIR, instructions are stored densely and referenced by index.
//!
//! # Place Expressions (ADR-0030 Phase 8)
//!
//! Memory locations are represented using [`AirPlace`], which consists of:
//! - A base ([`AirPlaceBase`]): either a local variable slot or parameter slot
//! - A list of projections ([`AirProjection`]): field accesses and array indices
//!
//! This design follows Rust MIR's proven approach and eliminates redundant
//! Load instructions for nested access patterns like `arr[i].field`.

use std::fmt;

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
use lasso::{Key, Spur};
use rue_span::Span;

// ============================================================================
// Place Expressions (ADR-0030 Phase 8)
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
/// - `x` → `AirPlace { base: Local(0), projections_start: 0, projections_len: 0 }`
/// - `arr[i]` → `AirPlace { base: Local(0), ... }` with `Index` projection
/// - `point.x` → `AirPlace { base: Local(0), ... }` with `Field` projection
/// - `arr[i].x` → `AirPlace { base: Local(0), ... }` with `Index` then `Field`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AirPlace {
    /// The base of the place - either a local slot or parameter slot
    pub base: AirPlaceBase,
    /// Start index into Air's projections array
    pub projections_start: u32,
    /// Number of projections
    pub projections_len: u32,
}

impl AirPlace {
    /// Create a place for a local variable with no projections.
    #[inline]
    pub const fn local(slot: u32) -> Self {
        Self {
            base: AirPlaceBase::Local(slot),
            projections_start: 0,
            projections_len: 0,
        }
    }

    /// Create a place for a parameter with no projections.
    #[inline]
    pub const fn param(slot: u32) -> Self {
        Self {
            base: AirPlaceBase::Param(slot),
            projections_start: 0,
            projections_len: 0,
        }
    }

    /// Returns true if this place has no projections (is just a variable).
    #[inline]
    pub const fn is_simple(&self) -> bool {
        self.projections_len == 0
    }

    /// Returns the local slot if this is a simple local place with no projections.
    #[inline]
    pub const fn as_local(&self) -> Option<u32> {
        if self.projections_len == 0 {
            match self.base {
                AirPlaceBase::Local(slot) => Some(slot),
                AirPlaceBase::Param(_) => None,
            }
        } else {
            None
        }
    }

    /// Returns the param slot if this is a simple param place with no projections.
    #[inline]
    pub const fn as_param(&self) -> Option<u32> {
        if self.projections_len == 0 {
            match self.base {
                AirPlaceBase::Param(slot) => Some(slot),
                AirPlaceBase::Local(_) => None,
            }
        } else {
            None
        }
    }
}

/// The base of a place - where the memory location starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AirPlaceBase {
    /// Local variable slot
    Local(u32),
    /// Parameter slot (for parameters, including inout)
    Param(u32),
}

/// A projection applied to a place to reach a nested location.
///
/// Projections are stored in `Air::projections` and referenced by
/// `AirPlace::projections_start` and `AirPlace::projections_len`.
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

impl AirArgMode {
    /// Convert to u32 for storage in extra array.
    #[inline]
    pub fn as_u32(self) -> u32 {
        match self {
            AirArgMode::Normal => 0,
            AirArgMode::Inout => 1,
            AirArgMode::Borrow => 2,
        }
    }

    /// Convert from u32 stored in extra array.
    #[inline]
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => AirArgMode::Normal,
            1 => AirArgMode::Inout,
            2 => AirArgMode::Borrow,
            _ => panic!("invalid AirArgMode value: {}", v),
        }
    }
}

/// An argument in a function call (AIR level).
#[derive(Debug, Clone)]
pub struct AirCallArg {
    /// The argument expression
    pub value: AirRef,
    /// The passing mode for this argument
    pub mode: AirArgMode,
}

impl AirCallArg {
    /// Returns true if this argument is passed as inout.
    /// This is a convenience method for backwards compatibility.
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

/// Pattern type tags for extra array encoding.
const PATTERN_WILDCARD: u32 = 0;
const PATTERN_INT: u32 = 1;
const PATTERN_BOOL: u32 = 2;
const PATTERN_ENUM_VARIANT: u32 = 3;

impl AirPattern {
    /// Encode this pattern to the extra array, returning the number of u32s written.
    /// Format:
    /// - Wildcard: [tag, body_ref] = 2 words
    /// - Int: [tag, body_ref, lo, hi] = 4 words (i64 as two u32s)
    /// - Bool: [tag, body_ref, value] = 3 words
    /// - EnumVariant: [tag, body_ref, enum_id, variant_index] = 4 words
    pub fn encode(&self, body: AirRef, out: &mut Vec<u32>) {
        match self {
            AirPattern::Wildcard => {
                out.push(PATTERN_WILDCARD);
                out.push(body.as_u32());
            }
            AirPattern::Int(n) => {
                out.push(PATTERN_INT);
                out.push(body.as_u32());
                // Encode i64 as two u32s (low, high)
                out.push(*n as u32);
                out.push((*n >> 32) as u32);
            }
            AirPattern::Bool(b) => {
                out.push(PATTERN_BOOL);
                out.push(body.as_u32());
                out.push(if *b { 1 } else { 0 });
            }
            AirPattern::EnumVariant {
                enum_id,
                variant_index,
            } => {
                out.push(PATTERN_ENUM_VARIANT);
                out.push(body.as_u32());
                out.push(enum_id.0);
                out.push(*variant_index);
            }
        }
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

        let tag = self.data[0];
        let body = AirRef::from_raw(self.data[1]);

        let (pattern, advance) = match tag {
            PATTERN_WILDCARD => (AirPattern::Wildcard, 2),
            PATTERN_INT => {
                let lo = self.data[2] as i64;
                let hi = (self.data[3] as i64) << 32;
                (AirPattern::Int(lo | hi), 4)
            }
            PATTERN_BOOL => {
                let b = self.data[2] != 0;
                (AirPattern::Bool(b), 3)
            }
            PATTERN_ENUM_VARIANT => {
                let enum_id = crate::types::EnumId(self.data[2]);
                let variant_index = self.data[3];
                (
                    AirPattern::EnumVariant {
                        enum_id,
                        variant_index,
                    },
                    4,
                )
            }
            _ => panic!("invalid pattern tag: {}", tag),
        };

        self.data = &self.data[advance..];
        Some((pattern, body))
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
#[derive(Debug, Default, Clone)]
pub struct Air {
    instructions: Vec<AirInst>,
    /// Extra data for variable-length instruction payloads (args, elements, etc.)
    extra: Vec<u32>,
    /// The return type of this function
    return_type: Type,
    /// Storage for place projections (ADR-0030 Phase 8).
    /// AirPlace instructions store (start, len) indices into this array.
    projections: Vec<AirProjection>,
    /// Storage for places (ADR-0030 Phase 8).
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
}

impl Air {
    /// Create a new empty AIR.
    pub fn new(return_type: Type) -> Self {
        Self {
            instructions: Vec::new(),
            extra: Vec::new(),
            return_type,
            projections: Vec::new(),
            places: Vec::new(),
            param_drops: Vec::new(),
            borrow_slots: Vec::new(),
        }
    }

    /// Record a local slot as a non-owning borrow binding (see `borrow_slots`).
    pub fn add_borrow_slot(&mut self, slot: u32) {
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
    pub fn add_inst(&mut self, inst: AirInst) -> AirRef {
        let index = self.instructions.len() as u32;
        self.instructions.push(inst);
        AirRef::from_raw(index)
    }

    /// Get an instruction by reference.
    #[inline]
    pub fn get(&self, inst_ref: AirRef) -> &AirInst {
        &self.instructions[inst_ref.0 as usize]
    }

    /// Set the owned-parameter drop list: (ABI slot, type) for each
    /// pass-by-value parameter that the callee owns (and must drop at exit
    /// unless moved out).
    pub fn set_param_drops(&mut self, param_drops: Vec<(u32, Type)>) {
        self.param_drops = param_drops;
    }

    /// Clear the owned-parameter drop list. Used for destructors: the
    /// destructor consumes `self`, and the drop glue (not the destructor
    /// itself) is responsible for dropping the fields afterwards, so the
    /// destructor must not re-drop its own parameter.
    pub fn clear_param_drops(&mut self) {
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
    pub fn cancel_move_marker(&mut self, inst_ref: AirRef) {
        if let AirInstData::MarkMoved { value, .. } = self.instructions[inst_ref.0 as usize].data {
            self.instructions[inst_ref.0 as usize].data =
                self.instructions[value.0 as usize].data.clone();
        }
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

    /// Add extra data and return the start index.
    pub fn add_extra(&mut self, data: &[u32]) -> u32 {
        // Correctness guard (must run in release): `start` and the extra
        // indices are truncated to u32 below, so an overflow would silently
        // alias unrelated extra data. Plain `assert!` not `debug_assert!`
        // (RUE-45).
        assert!(
            self.extra.len() <= u32::MAX as usize,
            "AIR extra data overflow: {} entries exceeds u32::MAX",
            self.extra.len()
        );
        assert!(
            self.extra.len().saturating_add(data.len()) <= u32::MAX as usize,
            "AIR extra data would overflow: {} + {} exceeds u32::MAX",
            self.extra.len(),
            data.len()
        );

        let start = self.extra.len() as u32;
        self.extra.extend_from_slice(data);
        start
    }

    /// Get extra data slice by start index and length.
    #[inline]
    pub fn get_extra(&self, start: u32, len: u32) -> &[u32] {
        let start = start as usize;
        let end = start + len as usize;
        &self.extra[start..end]
    }

    // Helper methods for reading structured data from extra array

    /// Get AirRefs from extra array (for blocks, array elements, intrinsic args, etc.).
    #[inline]
    pub fn get_air_refs(&self, start: u32, len: u32) -> impl Iterator<Item = AirRef> + '_ {
        self.get_extra(start, len)
            .iter()
            .map(|&v| AirRef::from_raw(v))
    }

    /// Get call arguments from extra array.
    /// Each call arg is encoded as 2 u32s: (air_ref, mode).
    #[inline]
    pub fn get_call_args(&self, start: u32, len: u32) -> impl Iterator<Item = AirCallArg> + '_ {
        let data = self.get_extra(start, len * 2);
        data.chunks_exact(2).map(|chunk| AirCallArg {
            value: AirRef::from_raw(chunk[0]),
            mode: AirArgMode::from_u32(chunk[1]),
        })
    }

    /// Get match arms from extra array.
    /// Each match arm is encoded based on pattern type plus the body AirRef.
    #[inline]
    pub fn get_match_arms(
        &self,
        start: u32,
        len: u32,
    ) -> impl Iterator<Item = (AirPattern, AirRef)> + '_ {
        MatchArmIterator {
            data: &self.extra[start as usize..],
            remaining: len as usize,
        }
    }

    /// Get struct init data from extra array.
    /// Returns (field_refs_iterator, source_order_iterator).
    #[inline]
    pub fn get_struct_init(
        &self,
        fields_start: u32,
        fields_len: u32,
        source_order_start: u32,
    ) -> (
        impl Iterator<Item = AirRef> + '_,
        impl Iterator<Item = usize> + '_,
    ) {
        let fields = self.get_air_refs(fields_start, fields_len);
        let source_order = self
            .get_extra(source_order_start, fields_len)
            .iter()
            .map(|&v| v as usize);
        (fields, source_order)
    }

    /// Remap string constant IDs using the provided mapping function.
    ///
    /// This is used after parallel function analysis to convert local string IDs
    /// (per-function) to global string IDs (across all functions). The mapping
    /// function takes a local string ID and returns the global string ID.
    pub fn remap_string_ids<F>(&mut self, map_fn: F)
    where
        F: Fn(u32) -> u32,
    {
        for inst in &mut self.instructions {
            if let AirInstData::StringConst(ref mut id) = inst.data {
                *id = map_fn(*id);
            }
        }
    }

    /// Get a reference to all instructions.
    #[inline]
    pub fn instructions(&self) -> &[AirInst] {
        &self.instructions
    }

    /// Rewrite the data of an instruction at a given index.
    ///
    /// This is used by the specialization pass to rewrite `CallGeneric` to `Call`.
    /// The type and span are preserved.
    pub fn rewrite_inst_data(&mut self, index: usize, new_data: AirInstData) {
        self.instructions[index].data = new_data;
    }

    // ========================================================================
    // Place operations (ADR-0030 Phase 8)
    // ========================================================================

    /// Add projections to the projections array and return (start, len).
    ///
    /// Used for PlaceRead and PlaceWrite instructions.
    pub fn push_projections(
        &mut self,
        projs: impl IntoIterator<Item = AirProjection>,
    ) -> (u32, u32) {
        let start = self.projections.len() as u32;
        self.projections.extend(projs);
        let len = self.projections.len() as u32 - start;
        (start, len)
    }

    /// Get a slice from the projections array.
    #[inline]
    pub fn get_projections(&self, start: u32, len: u32) -> &[AirProjection] {
        &self.projections[start as usize..(start + len) as usize]
    }

    /// Get projections for a place.
    #[inline]
    pub fn get_place_projections(&self, place: &AirPlace) -> &[AirProjection] {
        self.get_projections(place.projections_start, place.projections_len)
    }

    /// Create a place with the given base and projections.
    ///
    /// This adds the projections to the projections array and returns a PlaceRef
    /// that references the place.
    pub fn make_place(
        &mut self,
        base: AirPlaceBase,
        projs: impl IntoIterator<Item = AirProjection>,
    ) -> AirPlaceRef {
        let (projections_start, projections_len) = self.push_projections(projs);
        let place = AirPlace {
            base,
            projections_start,
            projections_len,
        };
        let index = self.places.len() as u32;
        self.places.push(place);
        AirPlaceRef::from_raw(index)
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

    /// Get all projections.
    #[inline]
    pub fn projections(&self) -> &[AirProjection] {
        &self.projections
    }
}

/// A single AIR instruction.
#[derive(Debug, Clone)]
pub struct AirInst {
    pub data: AirInstData,
    pub ty: Type,
    pub span: Span,
}

/// AIR instruction data - fully typed operations.
#[derive(Debug, Clone)]
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
        arms_start: u32,
        /// Number of match arms
        arms_len: u32,
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
        /// Function name (interned symbol)
        name: Spur,
        /// Start index into extra array for arguments
        args_start: u32,
        /// Number of arguments
        args_len: u32,
    },

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
        type_args_start: u32,
        /// Number of type arguments
        type_args_len: u32,
        /// Start index into extra array for encoded comptime value arguments
        value_args_start: u32,
        /// Number of words (not values) in the encoded value-argument stream
        value_args_len: u32,
        /// Start index into extra array for runtime arguments
        args_start: u32,
        /// Number of runtime arguments
        args_len: u32,
    },

    /// Intrinsic call (e.g., @dbg)
    Intrinsic {
        /// Intrinsic name (without @, interned)
        name: Spur,
        /// Start index into extra array for arguments
        args_start: u32,
        /// Number of arguments
        args_len: u32,
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
        stmts_start: u32,
        /// Number of statements
        stmts_len: u32,
        /// The block's resulting value
        value: AirRef,
    },

    // Struct operations
    /// Create a new struct instance with initialized fields
    StructInit {
        /// The struct type being created
        struct_id: StructId,
        /// Start index into extra array for field refs (in declaration order)
        fields_start: u32,
        /// Number of fields
        fields_len: u32,
        /// Start index into extra array for source order indices
        /// Each entry is an index into fields, specifying evaluation order
        source_order_start: u32,
    },

    /// Load a field from a struct value
    FieldGet {
        /// The struct value
        base: AirRef,
        /// The struct type
        struct_id: StructId,
        /// Field index (0-based, in declaration order)
        field_index: u32,
    },

    /// Store a value to a struct field (for local variables)
    FieldSet {
        /// The struct variable slot
        slot: u32,
        /// The struct type
        struct_id: StructId,
        /// Field index (0-based, in declaration order)
        field_index: u32,
        /// Value to store
        value: AirRef,
    },

    /// Store a value to a struct field (for parameters, including inout)
    ParamFieldSet {
        /// The parameter's ABI slot (relative to params, not locals)
        param_slot: u32,
        /// Offset within the struct for nested field access (e.g., p.inner.x)
        inner_offset: u32,
        /// The struct type containing the field being set
        struct_id: StructId,
        /// Field index (0-based, in declaration order)
        field_index: u32,
        /// Value to store
        value: AirRef,
    },

    // Array operations
    /// Create a new array with initialized elements.
    /// The array type is stored in `AirInst.ty` as `Type::new_array(...)`.
    ArrayInit {
        /// Start index into extra array for element refs
        elems_start: u32,
        /// Number of elements
        elems_len: u32,
    },

    /// Load an element from an array.
    /// The array type is stored in `AirInst.ty`.
    IndexGet {
        /// The array value
        base: AirRef,
        /// The array type (for bounds checking and element size)
        array_type: Type,
        /// Index expression
        index: AirRef,
    },

    /// Store a value to an array element.
    /// The array type is stored in `AirInst.ty`.
    IndexSet {
        /// The array variable slot
        slot: u32,
        /// The array type (for bounds checking and element size)
        array_type: Type,
        /// Index expression
        index: AirRef,
        /// Value to store
        value: AirRef,
    },

    /// Store a value to an array element of an inout parameter.
    /// The array type is stored in `AirInst.ty`.
    ParamIndexSet {
        /// The parameter's ABI slot (relative to params, not locals)
        param_slot: u32,
        /// The array type (for bounds checking and element size)
        array_type: Type,
        /// Index expression
        index: AirRef,
        /// Value to store
        value: AirRef,
    },

    // Place operations (ADR-0030 Phase 8)
    /// Read a value from a memory location.
    ///
    /// This unifies Load, IndexGet, and FieldGet into a single instruction
    /// that can handle arbitrarily nested access patterns like `arr[i].field`.
    /// Eventually, the separate FieldGet/IndexGet instructions will be removed.
    PlaceRead {
        /// Reference to the place to read from
        place: AirPlaceRef,
    },

    /// Write a value to a memory location.
    ///
    /// This unifies Store, IndexSet, ParamIndexSet, FieldSet, and ParamFieldSet
    /// into a single instruction that can handle nested writes.
    /// Eventually, the separate *Set instructions will be removed.
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
        payload_start: u32,
        /// Number of payload operands.
        payload_len: u32,
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
        /// `Field` projection chain of any depth (`o.a`, `o.a.b`), or a
        /// single CONSTANT-index projection (`xs[K]`, RUE-186, whose index
        /// operand is a dedicated `Const` instruction); drop elaboration
        /// then drops the struct/array path-granularly, skipping the moved
        /// path. Paths THROUGH an array index (`arr[i].a`) are NOT exported
        /// (no marker), keeping the whole-slot drop — which re-drops the
        /// moved element (a known gap: index-interior paths aren't tracked
        /// by drop elaboration).
        place: Option<AirPlaceRef>,
    },
}

impl fmt::Display for AirRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "%{}", self.0)
    }
}

impl fmt::Display for Air {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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
                AirInstData::Match {
                    scrutinee,
                    arms_start,
                    arms_len,
                } => {
                    write!(f, "match {} {{ ", scrutinee)?;
                    for (i, (pat, body)) in self.get_match_arms(*arms_start, *arms_len).enumerate()
                    {
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
                            } => format!("enum#{}::{}", enum_id.0, variant_index),
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
                    name,
                    args_start,
                    args_len,
                } => {
                    write!(f, "call @{}(", name.into_usize())?;
                    for (i, arg) in self.get_call_args(*args_start, *args_len).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", arg)?;
                    }
                    writeln!(f, ")")?;
                }
                AirInstData::CallGeneric {
                    name,
                    type_args_start,
                    type_args_len,
                    value_args_start,
                    value_args_len,
                    args_start,
                    args_len,
                } => {
                    write!(f, "call_generic @{}<", name.into_usize())?;
                    // Show type arguments
                    for i in 0..*type_args_len {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        let type_val = self.extra[(*type_args_start + i) as usize];
                        write!(f, "type#{}", type_val)?;
                    }
                    // Show comptime value arguments (decoded from the tagged
                    // word stream, see specialize::encode_const_values)
                    for (i, value) in crate::specialize::decode_const_values(
                        self.get_extra(*value_args_start, *value_args_len),
                    )
                    .iter()
                    .enumerate()
                    {
                        if i > 0 || *type_args_len > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "value {:?}", value)?;
                    }
                    write!(f, ">(")?;
                    // Show runtime arguments
                    for (i, arg) in self.get_call_args(*args_start, *args_len).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", arg)?;
                    }
                    writeln!(f, ")")?;
                }
                AirInstData::Intrinsic {
                    name,
                    args_start,
                    args_len,
                } => {
                    write!(f, "intrinsic @sym:{}(", name.into_usize())?;
                    for (i, arg) in self.get_air_refs(*args_start, *args_len).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", arg)?;
                    }
                    writeln!(f, ")")?;
                }
                AirInstData::Param { index } => writeln!(f, "param {}", index)?,
                AirInstData::Block {
                    stmts_start,
                    stmts_len,
                    value,
                } => {
                    write!(f, "block [")?;
                    for (i, s) in self.get_air_refs(*stmts_start, *stmts_len).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", s)?;
                    }
                    writeln!(f, "], {}", value)?;
                }
                AirInstData::StructInit {
                    struct_id,
                    fields_start,
                    fields_len,
                    source_order_start,
                } => {
                    write!(f, "struct_init #{} {{", struct_id.0)?;
                    let (fields, source_order) =
                        self.get_struct_init(*fields_start, *fields_len, *source_order_start);
                    for (i, field) in fields.enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", field)?;
                    }
                    write!(f, "}} eval_order=[")?;
                    for (i, idx) in source_order.enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", idx)?;
                    }
                    writeln!(f, "]")?;
                }
                AirInstData::FieldGet {
                    base,
                    struct_id,
                    field_index,
                } => {
                    writeln!(f, "field_get {}.#{}.{}", base, struct_id.0, field_index)?;
                }
                AirInstData::FieldSet {
                    slot,
                    struct_id,
                    field_index,
                    value,
                } => {
                    writeln!(
                        f,
                        "field_set ${}.#{}.{} = {}",
                        slot, struct_id.0, field_index, value
                    )?;
                }
                AirInstData::ParamFieldSet {
                    param_slot,
                    inner_offset,
                    struct_id,
                    field_index,
                    value,
                } => {
                    writeln!(
                        f,
                        "param_field_set %{}+{}.#{}.{} = {}",
                        param_slot, inner_offset, struct_id.0, field_index, value
                    )?;
                }
                AirInstData::ArrayInit {
                    elems_start,
                    elems_len,
                } => {
                    write!(f, "array_init [")?;
                    for (i, elem) in self.get_air_refs(*elems_start, *elems_len).enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", elem)?;
                    }
                    writeln!(f, "]")?;
                }
                AirInstData::IndexGet {
                    base,
                    array_type,
                    index,
                } => {
                    writeln!(f, "index_get {}({})[{}]", base, array_type.name(), index)?;
                }
                AirInstData::IndexSet {
                    slot,
                    array_type,
                    index,
                    value,
                } => {
                    writeln!(
                        f,
                        "index_set ${}({})[{}] = {}",
                        slot,
                        array_type.name(),
                        index,
                        value
                    )?;
                }
                AirInstData::ParamIndexSet {
                    param_slot,
                    array_type,
                    index,
                    value,
                } => {
                    writeln!(
                        f,
                        "param_index_set param{}({})[{}] = {}",
                        param_slot,
                        array_type.name(),
                        index,
                        value
                    )?;
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
                    payload_start,
                    payload_len,
                } => {
                    if *payload_len == 0 {
                        writeln!(f, "enum_variant #{}::{}", enum_id.0, variant_index)?;
                    } else {
                        let payload: Vec<String> = self
                            .get_air_refs(*payload_start, *payload_len)
                            .map(|r| r.to_string())
                            .collect();
                        writeln!(
                            f,
                            "enum_variant #{}::{}({})",
                            enum_id.0,
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
                        "enum_payload_get {} #{}::{}.{}",
                        base, enum_id.0, variant_index, field_index
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

impl Air {
    /// Format a place for display, showing the base and projections.
    fn fmt_place(&self, f: &mut fmt::Formatter<'_>, place_ref: AirPlaceRef) -> fmt::Result {
        let place = self.get_place(place_ref);

        // Write the base
        match place.base {
            AirPlaceBase::Local(slot) => write!(f, "${}", slot)?,
            AirPlaceBase::Param(slot) => write!(f, "param%{}", slot)?,
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
mod tests {
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
}
