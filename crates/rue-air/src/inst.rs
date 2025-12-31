//! AIR instruction definitions.
//!
//! Like RIR, instructions are stored densely and referenced by index.

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

use crate::types::{ArrayTypeId, StructId, Type};
use lasso::{Key, Spur};
use rue_span::Span;

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
#[derive(Debug, Default)]
pub struct Air {
    instructions: Vec<AirInst>,
    /// Extra data for variable-length instruction payloads (args, elements, etc.)
    extra: Vec<u32>,
    /// The return type of this function
    return_type: Type,
}

impl Air {
    /// Create a new empty AIR.
    pub fn new(return_type: Type) -> Self {
        Self {
            instructions: Vec::new(),
            extra: Vec::new(),
            return_type,
        }
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
        // Debug assertions for u32 overflow
        debug_assert!(
            self.extra.len() <= u32::MAX as usize,
            "AIR extra data overflow: {} entries exceeds u32::MAX",
            self.extra.len()
        );
        debug_assert!(
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
    /// Create a new array with initialized elements
    ArrayInit {
        /// The array type
        array_type_id: ArrayTypeId,
        /// Start index into extra array for element refs
        elems_start: u32,
        /// Number of elements
        elems_len: u32,
    },

    /// Load an element from an array
    IndexGet {
        /// The array value
        base: AirRef,
        /// The array type
        array_type_id: ArrayTypeId,
        /// Index expression
        index: AirRef,
    },

    /// Store a value to an array element
    IndexSet {
        /// The array variable slot
        slot: u32,
        /// The array type
        array_type_id: ArrayTypeId,
        /// Index expression
        index: AirRef,
        /// Value to store
        value: AirRef,
    },

    /// Store a value to an array element of an inout parameter
    ParamIndexSet {
        /// The parameter's ABI slot (relative to params, not locals)
        param_slot: u32,
        /// The array type
        array_type_id: ArrayTypeId,
        /// Index expression
        index: AirRef,
        /// Value to store
        value: AirRef,
    },

    // Enum operations
    /// Create an enum variant value
    EnumVariant {
        /// The enum type ID
        enum_id: crate::types::EnumId,
        /// The variant index (0-based)
        variant_index: u32,
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
                    array_type_id,
                    elems_start,
                    elems_len,
                } => {
                    write!(f, "array_init @{} [", array_type_id.0)?;
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
                    array_type_id,
                    index,
                } => {
                    writeln!(f, "index_get {}(@{})[{}]", base, array_type_id.0, index)?;
                }
                AirInstData::IndexSet {
                    slot,
                    array_type_id,
                    index,
                    value,
                } => {
                    writeln!(
                        f,
                        "index_set ${}(@{})[{}] = {}",
                        slot, array_type_id.0, index, value
                    )?;
                }
                AirInstData::ParamIndexSet {
                    param_slot,
                    array_type_id,
                    index,
                    value,
                } => {
                    writeln!(
                        f,
                        "param_index_set param{}(@{})[{}] = {}",
                        param_slot, array_type_id.0, index, value
                    )?;
                }
                AirInstData::EnumVariant {
                    enum_id,
                    variant_index,
                } => {
                    writeln!(f, "enum_variant #{}::{}", enum_id.0, variant_index)?;
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
            }
        }
        writeln!(f, "}}")
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
