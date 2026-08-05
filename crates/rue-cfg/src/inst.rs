//! CFG instruction definitions.
//!
//! Unlike AIR, the CFG has explicit basic blocks and terminators.
//! Control flow only happens at block boundaries via terminators.
//!
//! # Place Expressions
//!
//! Memory locations are represented using [`Place`], which consists of:
//! - A base ([`PlaceBase`]): either a local variable slot or parameter slot
//! - A list of projections ([`Projection`]): field accesses and array indices
//!
//! This design follows Rust MIR's proven approach and eliminates redundant
//! Load instructions for nested access patterns like `arr[i].field`.

use std::fmt;

// Compile-time size assertions to prevent silent size growth during refactoring.
// These limits are set slightly above current sizes to allow minor changes,
// but will catch significant size regressions.
//
// Current sizes (as of 2025-12):
// - CfgInst: 40 bytes (CfgInstData + Type + Span)
// - CfgInstData: 24 bytes
const _: () = assert!(std::mem::size_of::<CfgInst>() <= 48);
const _: () = assert!(std::mem::size_of::<CfgInstData>() <= 32);

use lasso::{Key, Spur, ThreadedRodeo};
use rue_air::{EnumId, ParamSlotModes, StructId, Type};
use rue_span::Span;

use crate::payload::{
    self, CfgArrayElements, CfgCallArgs, CfgElseArgs, CfgEnumPayload, CfgGotoArgs,
    CfgIntrinsicArgs, CfgProjections, CfgStructFields, CfgSwitchCases, CfgThenArgs,
};

mod payload_metrics;

// ============================================================================
// Place Expressions
// ============================================================================

/// A memory location that can be read from or written to.
///
/// A place represents a path to a memory location, consisting of a base
/// (local variable or parameter) and zero or more projections (field access,
/// array indexing).
///
/// # Examples
///
/// - `x` → `Place { base: Local(0), base_type: T, ... }`
/// - `arr[i]` → `Place { base: Local(0), base_type: Array, ... }` with `Index` projection
/// - `point.x` → `Place { base: Local(0), base_type: Point, ... }` with `Field` projection
/// - `arr[i].x` → the same `Array` base type with `Index` then `Field`
#[derive(Debug, PartialEq, Eq)]
pub struct Place {
    /// The base of the place - either a local slot or parameter slot
    pub base: PlaceBase,
    /// The logical type stored at the base before applying projections.
    ///
    /// This is explicit because a physical ABI slot does not uniquely identify
    /// its logical value type (aggregates are flattened and zero-sized values
    /// can share an index). It also lets the verifier type-check the first
    /// projection instead of trusting its self-described container.
    pub base_type: Type,
    /// Start index into Cfg's projections array
    pub(crate) projections: CfgProjections,
}

impl Place {
    pub(crate) fn duplicate_with_owner(&self) -> Self {
        Self {
            base: self.base,
            base_type: self.base_type,
            projections: self.projections.duplicate(),
        }
    }

    /// Create a place for a local variable with no projections.
    #[inline]
    pub const fn local(slot: u32, base_type: Type) -> Self {
        Self {
            base: PlaceBase::Local(slot),
            base_type,
            projections: CfgProjections::EMPTY,
        }
    }

    /// Create a place for a parameter with no projections.
    #[inline]
    pub const fn param(slot: u32, base_type: Type) -> Self {
        Self {
            base: PlaceBase::Param(slot),
            base_type,
            projections: CfgProjections::EMPTY,
        }
    }

    /// Return this place with a different base, preserving its opaque
    /// projection payload.
    #[inline]
    pub const fn with_base(self, base: PlaceBase) -> Self {
        Self { base, ..self }
    }

    /// Return this place with a different root type, preserving its opaque
    /// projection payload.
    #[inline]
    pub const fn with_base_type(self, base_type: Type) -> Self {
        Self { base_type, ..self }
    }

    /// Returns the local slot if this is a simple local place with no projections.
    #[inline]
    pub const fn as_local(&self) -> Option<u32> {
        if self.projections.is_empty() {
            match self.base {
                PlaceBase::Local(slot) => Some(slot),
                PlaceBase::Param(_) | PlaceBase::Accessor(_) => None,
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
                PlaceBase::Param(slot) => Some(slot),
                PlaceBase::Local(_) | PlaceBase::Accessor(_) => None,
            }
        } else {
            None
        }
    }
}

impl CfgInstData {
    pub(crate) fn duplicate_with_owner(&self) -> Self {
        match self {
            Self::Const(v) => Self::Const(*v),
            Self::BoolConst(v) => Self::BoolConst(*v),
            Self::StringConst(v) => Self::StringConst(*v),
            Self::Param { index } => Self::Param { index: *index },
            Self::BlockParam { index } => Self::BlockParam { index: *index },
            Self::Add(a, b) => Self::Add(*a, *b),
            Self::Sub(a, b) => Self::Sub(*a, *b),
            Self::Mul(a, b) => Self::Mul(*a, *b),
            Self::WrappingAdd(a, b) => Self::WrappingAdd(*a, *b),
            Self::WrappingSub(a, b) => Self::WrappingSub(*a, *b),
            Self::WrappingMul(a, b) => Self::WrappingMul(*a, *b),
            Self::Div(a, b) => Self::Div(*a, *b),
            Self::Mod(a, b) => Self::Mod(*a, *b),
            Self::Eq(a, b) => Self::Eq(*a, *b),
            Self::Ne(a, b) => Self::Ne(*a, *b),
            Self::Lt(a, b) => Self::Lt(*a, *b),
            Self::Gt(a, b) => Self::Gt(*a, *b),
            Self::Le(a, b) => Self::Le(*a, *b),
            Self::Ge(a, b) => Self::Ge(*a, *b),
            Self::BitAnd(a, b) => Self::BitAnd(*a, *b),
            Self::BitOr(a, b) => Self::BitOr(*a, *b),
            Self::BitXor(a, b) => Self::BitXor(*a, *b),
            Self::Shl(a, b) => Self::Shl(*a, *b),
            Self::Shr(a, b) => Self::Shr(*a, *b),
            Self::Neg(v) => Self::Neg(*v),
            Self::Not(v) => Self::Not(*v),
            Self::BitNot(v) => Self::BitNot(*v),
            Self::Alloc { slot, init } => Self::Alloc {
                slot: *slot,
                init: *init,
            },
            Self::Load { slot } => Self::Load { slot: *slot },
            Self::Store { slot, value } => Self::Store {
                slot: *slot,
                value: *value,
            },
            Self::ParamStore { param_slot, value } => Self::ParamStore {
                param_slot: *param_slot,
                value: *value,
            },
            Self::PlaceRead { place } => Self::PlaceRead {
                place: place.duplicate_with_owner(),
            },
            Self::PlaceWrite { place, value } => Self::PlaceWrite {
                place: place.duplicate_with_owner(),
                value: *value,
            },
            Self::Call {
                runtime,
                name,
                args,
            } => Self::Call {
                runtime: *runtime,
                name: *name,
                args: args.duplicate(),
            },
            Self::AccessorCall { name, args } => Self::AccessorCall {
                name: *name,
                args: args.duplicate(),
            },
            Self::Intrinsic {
                runtime,
                name,
                args,
            } => Self::Intrinsic {
                runtime: *runtime,
                name: *name,
                args: args.duplicate(),
            },
            Self::StructInit { struct_id, fields } => Self::StructInit {
                struct_id: *struct_id,
                fields: fields.duplicate(),
            },
            Self::ArrayInit { elements } => Self::ArrayInit {
                elements: elements.duplicate(),
            },
            Self::EnumVariant {
                enum_id,
                variant_index,
                payload,
            } => Self::EnumVariant {
                enum_id: *enum_id,
                variant_index: *variant_index,
                payload: payload.duplicate(),
            },
            Self::EnumPayloadGet {
                base,
                enum_id,
                variant_index,
                field_index,
            } => Self::EnumPayloadGet {
                base: *base,
                enum_id: *enum_id,
                variant_index: *variant_index,
                field_index: *field_index,
            },
            Self::IntCast { value, from_ty } => Self::IntCast {
                value: *value,
                from_ty: *from_ty,
            },
            Self::Drop { value } => Self::Drop { value: *value },
            Self::StorageLive { slot, local_ty } => Self::StorageLive {
                slot: *slot,
                local_ty: *local_ty,
            },
            Self::StorageDead { slot, local_ty } => Self::StorageDead {
                slot: *slot,
                local_ty: *local_ty,
            },
        }
    }
}

impl fmt::Display for Place {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.base {
            PlaceBase::Local(slot) => write!(f, "${}", slot)?,
            PlaceBase::Param(slot) => write!(f, "%{}", slot)?,
            PlaceBase::Accessor(value) => write!(f, "accessor%{}", value.as_u32())?,
        }
        if !self.projections.is_empty() {
            write!(f, "[{} projections]", self.projections.extent())?;
        }
        Ok(())
    }
}

/// The base of a place - where the memory location starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceBase {
    /// Local variable slot
    Local(u32),
    /// Parameter slot (for parameters, including inout)
    Param(u32),
    /// Mandatory-inline accessor call whose place result has not yet been
    /// substituted by the inter-procedural CFG splice.
    Accessor(CfgValue),
}

/// A projection applied to a place to reach a nested location.
///
/// Projections are stored in CFG's typed projection store and referenced by
/// the owning place's opaque projection range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Projection {
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
    /// The index is a CfgValue that will be evaluated at runtime.
    Index { array_type: Type, index: CfgValue },
}

/// A basic block identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub(crate) u32);

impl BlockId {
    /// Create a new block ID from a raw index.
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

impl fmt::Display for BlockId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bb{}", self.0)
    }
}

/// A reference to a value (instruction result) in the CFG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CfgValue(u32);

impl CfgValue {
    /// Create a new value reference from a raw index.
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

impl fmt::Display for CfgValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// A single CFG instruction with its metadata.
#[derive(Debug)]
pub struct CfgInst {
    pub data: CfgInstData,
    pub ty: Type,
    pub span: Span,
}

/// Argument passing mode in CFG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CfgArgMode {
    /// Normal pass-by-value argument
    #[default]
    Normal,
    /// Inout argument - mutated in place
    Inout,
    /// Borrow argument - immutable borrow
    Borrow,
}

/// An argument in a function call.
#[derive(Debug, Clone, Copy)]
pub struct CfgCallArg {
    /// The argument value
    pub value: CfgValue,
    /// The passing mode for this argument
    pub mode: CfgArgMode,
}

impl CfgCallArg {
    /// Returns true if this argument is passed as inout (mutable by reference).
    pub fn is_inout(&self) -> bool {
        self.mode == CfgArgMode::Inout
    }

    /// Returns true if this argument is passed as borrow (immutable by reference).
    pub fn is_borrow(&self) -> bool {
        self.mode == CfgArgMode::Borrow
    }

    /// Returns true if this argument is passed by reference (either inout or borrow).
    pub fn is_by_ref(&self) -> bool {
        matches!(self.mode, CfgArgMode::Inout | CfgArgMode::Borrow)
    }
}

/// CFG instruction data.
///
/// Unlike AIR, there are NO control flow instructions here.
/// Control flow is handled entirely by terminators.
#[allow(private_interfaces)]
#[derive(Debug)]
pub enum CfgInstData {
    /// Integer constant (typed)
    Const(u64),

    /// Boolean constant
    BoolConst(bool),

    /// String constant (index into string table)
    StringConst(u32),

    /// Reference to a function parameter
    Param {
        index: u32,
    },

    /// Block parameter (like phi, but explicit)
    /// Only valid at the start of a block
    BlockParam {
        index: u32,
    },

    // Binary arithmetic operations
    Add(CfgValue, CfgValue),
    Sub(CfgValue, CfgValue),
    Mul(CfgValue, CfgValue),
    /// Wrapping arithmetic (two's-complement mod 2^N, never traps) — RUE-647.
    WrappingAdd(CfgValue, CfgValue),
    WrappingSub(CfgValue, CfgValue),
    WrappingMul(CfgValue, CfgValue),
    Div(CfgValue, CfgValue),
    Mod(CfgValue, CfgValue),

    // Comparison operations (return bool)
    Eq(CfgValue, CfgValue),
    Ne(CfgValue, CfgValue),
    Lt(CfgValue, CfgValue),
    Gt(CfgValue, CfgValue),
    Le(CfgValue, CfgValue),
    Ge(CfgValue, CfgValue),

    // Bitwise operations
    BitAnd(CfgValue, CfgValue),
    BitOr(CfgValue, CfgValue),
    BitXor(CfgValue, CfgValue),
    Shl(CfgValue, CfgValue),
    Shr(CfgValue, CfgValue),

    // Unary operations
    Neg(CfgValue),
    Not(CfgValue),
    BitNot(CfgValue),

    // Variable operations
    /// Allocate local variable with initial value
    Alloc {
        slot: u32,
        init: CfgValue,
    },
    /// Load value from local variable
    Load {
        slot: u32,
    },
    /// Store value to local variable
    Store {
        slot: u32,
        value: CfgValue,
    },
    /// Store value to a parameter (for inout params)
    ParamStore {
        param_slot: u32,
        value: CfgValue,
    },

    // Place operations
    /// Read a value from a memory location.
    ///
    /// Projected memory reads are represented canonically by this instruction,
    /// including arbitrarily nested access patterns like `arr[i].field`.
    PlaceRead {
        place: Place,
    },

    /// Write a value to a memory location.
    ///
    /// Projected memory writes are represented canonically by this instruction,
    /// including nested local and parameter writes.
    PlaceWrite {
        place: Place,
        value: CfgValue,
    },

    // Function calls
    /// Function call. Arguments are stored in CFG's typed call-argument store.
    Call {
        /// Typed runtime identity, absent for source-language calls.
        runtime: Option<rue_air::RuntimeCallKind>,
        /// Function name (interned symbol)
        name: Spur,
        /// Start index into Cfg's call_args array
        args: CfgCallArgs,
    },

    /// Mandatory-inline place-producing accessor call.
    AccessorCall {
        name: Spur,
        args: CfgCallArgs,
    },

    /// Intrinsic call (e.g., @dbg). Arguments use the intrinsic-value family.
    Intrinsic {
        /// Runtime helper/adaptation selected by semantic analysis.
        runtime: Option<rue_air::RuntimeCallKind>,
        /// Intrinsic name (interned symbol)
        name: Spur,
        /// Start index into Cfg's extra array
        args: CfgIntrinsicArgs,
    },

    // Struct operations
    /// Struct initialization. Field values use the struct-field family.
    StructInit {
        struct_id: StructId,
        /// Start index into Cfg's extra array
        fields: CfgStructFields,
    },
    // Array operations
    /// Array initialization. Element values use the array-element family.
    /// The array type is stored in `CfgInst.ty`.
    ArrayInit {
        /// Start index into Cfg's extra array
        elements: CfgArrayElements,
    },
    // Enum operations
    /// Create an enum variant value: the discriminant plus (for tuple
    /// variants, RUE-221) the payload operands, stored in the Cfg's extra
    /// array. Payload operands use the enum-payload family (empty for a
    /// discriminant-only variant).
    EnumVariant {
        enum_id: EnumId,
        variant_index: u32,
        /// Start index into the Cfg's extra array for payload values.
        payload: CfgEnumPayload,
    },
    /// Read payload field `field_index` of a tuple variant from an enum value
    /// (RUE-221). The result is the payload field, read from the tagged-union
    /// slots of `base` (slot 0 is the discriminant).
    EnumPayloadGet {
        base: CfgValue,
        enum_id: EnumId,
        variant_index: u32,
        field_index: u32,
    },

    // Type conversion operations
    /// Integer cast: convert between integer types with runtime range check.
    /// Panics if the value cannot be represented in the target type.
    /// The target type is stored in CfgInst.ty.
    IntCast {
        /// The value to cast
        value: CfgValue,
        /// The source type (for determining signedness and size)
        from_ty: Type,
    },

    // Drop/destructor operations
    /// Drop a value, running its destructor if the type has one.
    /// For trivially droppable types, this is a no-op that will be elided.
    Drop {
        value: CfgValue,
    },

    // Storage liveness operations (for drop elaboration and stack allocation)
    /// Marks that a local slot becomes live (storage allocated).
    /// The slot is now valid to write to.
    StorageLive {
        slot: u32,
        local_ty: Type,
    },

    /// Marks that a local slot becomes dead (storage can be deallocated).
    /// The slot is now invalid to read from.
    /// Drop elaboration inserts Drop before this if the type needs drop.
    StorageDead {
        slot: u32,
        local_ty: Type,
    },
}

/// Block terminator - how control leaves a basic block.
///
/// Terminators are the ONLY place where control flow happens in the CFG.
///
/// Block arguments are stored in the CFG's `extra` array for efficiency.
/// Use `Cfg::get_goto_args()`, `Cfg::get_branch_then_args()`, and
/// `Cfg::get_branch_else_args()` to retrieve the arguments.
#[allow(private_interfaces)]
#[derive(Debug)]
pub enum Terminator {
    /// Unconditional jump to another block.
    /// Arguments are stored in Cfg's extra array.
    Goto {
        target: BlockId,
        /// Start index into Cfg's extra array
        args: CfgGotoArgs,
    },

    /// Conditional branch.
    /// Arguments for each branch are stored in Cfg's extra array.
    Branch {
        cond: CfgValue,
        then_block: BlockId,
        /// Start index into Cfg's extra array for then branch args
        then_args: CfgThenArgs,
        else_block: BlockId,
        /// Start index into Cfg's extra array for else branch args
        else_args: CfgElseArgs,
    },

    /// Multi-way branch (switch/match).
    /// Cases are stored in Cfg's switch_cases array.
    Switch {
        /// The value to switch on
        scrutinee: CfgValue,
        /// Start index into Cfg's switch_cases array
        cases: CfgSwitchCases,
        /// Default block (for wildcard pattern)
        default: BlockId,
    },

    /// Return from function (None for unit-returning functions).
    Return { value: Option<CfgValue> },

    /// Unreachable - control never reaches here.
    /// Used after diverging expressions.
    Unreachable,

    /// Placeholder for blocks under construction.
    /// Should not exist in a valid CFG.
    None,
}

impl Terminator {
    fn duplicate_with_owner(&self) -> Self {
        match self {
            Self::Goto { target, args } => Self::Goto {
                target: *target,
                args: args.duplicate(),
            },
            Self::Branch {
                cond,
                then_block,
                then_args,
                else_block,
                else_args,
            } => Self::Branch {
                cond: *cond,
                then_block: *then_block,
                then_args: then_args.duplicate(),
                else_block: *else_block,
                else_args: else_args.duplicate(),
            },
            Self::Switch {
                scrutinee,
                cases,
                default,
            } => Self::Switch {
                scrutinee: *scrutinee,
                cases: cases.duplicate(),
                default: *default,
            },
            Self::Return { value } => Self::Return { value: *value },
            Self::Unreachable => Self::Unreachable,
            Self::None => Self::None,
        }
    }
}

/// A basic block in the CFG.
#[derive(Debug)]
pub struct BasicBlock {
    /// Block identifier
    pub id: BlockId,
    /// Block parameters (receive values from predecessors)
    pub params: Vec<(CfgValue, Type)>,
    /// Instructions in this block (straight-line, no control flow)
    pub insts: Vec<CfgValue>,
    /// How this block exits
    pub terminator: Terminator,
}

impl BasicBlock {
    /// Create a new empty basic block.
    pub fn new(id: BlockId) -> Self {
        Self {
            id,
            params: Vec::new(),
            insts: Vec::new(),
            terminator: Terminator::None,
        }
    }
}

/// The complete CFG for a function.
#[derive(Debug)]
pub struct Cfg {
    /// All basic blocks
    blocks: Vec<BasicBlock>,
    /// Entry block
    pub entry: BlockId,
    /// Return type
    return_type: Type,
    /// All instructions (values) - blocks reference these by CfgValue
    values: Vec<CfgInst>,
    /// Extra storage for variable-length CfgValue data (struct fields, array elements, intrinsic args,
    /// and terminator block arguments). Instructions and terminators store (start, len) indices into this array.
    extra: payload::Values,
    /// Extra storage for call arguments (CfgCallArg).
    /// Call instructions store (start, len) indices into this array.
    call_args: payload::CallArgs,
    /// Extra storage for switch cases (value, target block pairs).
    /// Switch terminators store (start, len) indices into this array.
    switch_cases: payload::SwitchCases,
    /// Extra storage for place projections.
    /// Place instructions store (start, len) indices into this array.
    projections: payload::Projections,
    /// Number of local variable slots
    num_locals: u32,
    /// Number of parameter slots
    num_params: u32,
    /// Function name
    fn_name: String,
    /// Physical by-reference and logical writability facts for every
    /// parameter ABI slot.
    param_modes: ParamSlotModes,
    /// Local slots whose ADDRESS escapes through an address-taking intrinsic
    /// (`@raw` / `@raw_mut` / `@field_ptr`), recorded at construction by
    /// CfgBuilder (which has the interner to recognize the names — opt passes
    /// don't). Codegen lowers those intrinsics by taking the operand place's
    /// address, so its `Load`/`PlaceRead` must survive optimization: constant
    /// propagation
    /// consults this to disqualify the slot, otherwise the Load becomes a
    /// `Const` and the constant is dereferenced as an address — the verified
    /// RUE-521 O1+ segfault. By-ref call arguments are the analogous
    /// per-instruction escape, handled directly in constopt's scan.
    address_taken_slots: std::collections::HashSet<u32>,
    /// Parameter ABI slots whose ADDRESS escapes through an address-taking
    /// intrinsic (`@raw` / `@raw_mut` / `@field_ptr` applied to a parameter),
    /// recorded at construction like `address_taken_slots`. A by-value
    /// parameter normally cannot change between reads, which is what lets
    /// CSE key repeated `Param` reads (RUE-914) — but a raw pointer obtained
    /// from the parameter's storage can mutate it (`@ptr_write`), so reads
    /// on either side of such a write are NOT interchangeable. Passes that
    /// treat parameter reads as pure must consult this set.
    address_taken_params: std::collections::HashSet<u32>,
    /// Grouped per-source-parameter ABI descriptors (ADR-0052 phase 5.8,
    /// RUE-1005), derived at construction from the AIR parameter types and the
    /// per-slot by-reference vector. Empty for a directly constructed CFG
    /// (synthetic tests), in which case code generation homes one incoming
    /// register per parameter slot (the historical prologue). Deriving it from
    /// the AIR keeps a durable/imported body — which rebuilds its CFG from the
    /// same AIR — identical to its freshly analyzed counterpart.
    source_param_abi: Vec<rue_air::SourceParamAbi>,
    /// The family ("basic blocks" / "values") whose per-function ceiling
    /// ([`MAX_CFG_ENTITIES_PER_FUNCTION`], spec Appendix C.6:1) a
    /// [`Cfg::new_block`] or [`Cfg::add_inst`] call ran past, if any.
    ///
    /// Both owner-index allocators are infallible and are called from hundreds
    /// of construction and optimization sites, so the ceiling is recorded here
    /// and reported once at the construction and optimization boundaries rather
    /// than wrapping `len() as u32` onto an existing identity. Spec C.1:2
    /// requires a diagnostic (`E1401`), not a wrapped index.
    capacity_exceeded: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CfgPayloadStorageStats {
    pub family_logical_bytes: [usize; 10],
    pub value_store_logical_bytes: usize,
    pub value_store_capacity_bytes: usize,
    pub call_store_logical_bytes: usize,
    pub call_store_capacity_bytes: usize,
    pub switch_store_logical_bytes: usize,
    pub switch_store_capacity_bytes: usize,
    pub projection_store_logical_bytes: usize,
    pub projection_store_capacity_bytes: usize,
    pub nonempty_variable_envelopes: usize,
    pub peak_staging_bytes: usize,
}

/// Mutable CFG construction and transformation state.
///
/// Only an editor can mutate owner-bound payload stores. Consumers receive a
/// [`ValidatedCfg`] after verification has consumed the editor.
pub type CfgEditor = Cfg;

/// Recoverable failure from an owner-level CFG edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CfgEditError {
    /// A block, value, target, or instruction kind was invalid before mutation.
    InvalidBuilderInput {
        operation: &'static str,
        detail: &'static str,
    },
    /// A payload cannot be represented by the compact `u32` range format.
    ResourceLimitExceeded { family: &'static str },
    /// One function's block or value arena ran past the published per-function
    /// ceiling, so its next `u32` identity is not representable.
    OwnerLimitExceeded { family: &'static str },
    /// Reserving storage failed without changing the CFG.
    CapacityFailure { family: &'static str },
}

/// The published per-program ceiling on the CFG payload word store: payload
/// ranges are `(start: u32, extent: u32)` into one shared store, so a program
/// holds at most this many CFG payload words (spec Appendix C.6:1).
pub const MAX_CFG_PAYLOAD_WORDS_PER_PROGRAM: u32 = u32::MAX;

/// The published per-function ceiling on CFG basic blocks and CFG values
/// (spec Appendix C.6:1).
///
/// A [`BlockId`] and a [`CfgValue`] are `u32` indices into the arenas of **one**
/// function's graph, and both arena lengths are narrowed to `u32` by graph
/// traversal and domain projection, so a function holds at most this many of
/// each. Exceeding either is a diagnosable compile-time failure (spec C.1:2),
/// surfaced as `E1401` at the CFG construction and optimization boundaries.
///
/// This ceiling is *checked*, not argued unreachable: CFG entities are not a
/// small constant multiple of the RIR instructions that produce them. Drop
/// elaboration re-emits one drop (and, for a path-dependent move, a flag-guard
/// block) per live slot at **every** exit, so a body with `N` droppable
/// bindings and `M` `return` statements lowers to on the order of `N * M`
/// values and blocks — quadratic, from a body that is linear in its source. See
/// spec C.6:5 for the arithmetic that disproves the unreachability hypothesis.
pub const MAX_CFG_ENTITIES_PER_FUNCTION: u32 = u32::MAX;

/// The `u32` identity the next block or value in a per-function arena of
/// `length` entries will take, or `None` once the published per-function
/// ceiling leaves no representable identity.
fn checked_owner_index(length: usize) -> Option<u32> {
    let index = u32::try_from(length).ok()?;
    (index < MAX_CFG_ENTITIES_PER_FUNCTION).then_some(index)
}

impl fmt::Display for CfgEditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBuilderInput { operation, detail } => {
                write!(f, "invalid CFG {operation} builder input: {detail}")
            }
            Self::ResourceLimitExceeded { family } => write!(
                f,
                "CFG {family} exceeded the implementation limit of \
                 {MAX_CFG_PAYLOAD_WORDS_PER_PROGRAM} payload words per program \
                 (spec Appendix C.6:1)"
            ),
            Self::OwnerLimitExceeded { family } => write!(
                f,
                "this function's CFG has more {family} than the implementation limit of \
                 {MAX_CFG_ENTITIES_PER_FUNCTION} per function (spec Appendix C.6:1)"
            ),
            Self::CapacityFailure { family } => {
                write!(f, "could not reserve storage for CFG {family} payload")
            }
        }
    }
}

impl std::error::Error for CfgEditError {}

impl CfgEditError {
    /// Classify this edit failure for the user.
    ///
    /// Spec C.1:2 makes exceeding a published implementation limit a
    /// diagnosable compile-time failure rather than an internal compiler
    /// error, so a payload range that no longer fits the compact `u32`
    /// representation is reported as `E1401` naming the limit. A failed
    /// reservation for an otherwise representable request is the environmental
    /// `E1402`; only a malformed builder request remains an ICE.
    pub fn error_kind(&self, context: &str) -> rue_error::ErrorKind {
        match self {
            Self::ResourceLimitExceeded { .. } | Self::OwnerLimitExceeded { .. } => {
                rue_error::ErrorKind::CompilerResourceLimit(self.to_string())
            }
            Self::CapacityFailure { .. } => {
                rue_error::ErrorKind::CompilerResourceExhaustion(self.to_string())
            }
            Self::InvalidBuilderInput { .. } => {
                rue_error::ErrorKind::InternalError(format!("{context}: {self}"))
            }
        }
    }
}

/// Failure from a validated owner-level edit transaction.
#[derive(Debug)]
pub enum CfgEditTransactionError {
    /// The requested edit was rejected before publication.
    Edit(CfgEditError),
    /// The edited owner did not pass complete structural and type validation.
    Verification(crate::CfgVerificationError),
}

/// Failure while remapping an entire CFG owner into new request-local domains.
#[derive(Debug)]
pub enum CfgRemapError<E> {
    /// A caller-supplied domain mapping rejected a value.
    Domain(E),
    /// Rebuilding an owner-bound payload failed without publishing a CFG.
    Edit(CfgEditError),
}

impl fmt::Display for CfgEditTransactionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Edit(error) => write!(f, "CFG edit was rejected: {error:?}"),
            Self::Verification(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for CfgEditTransactionError {}

impl From<payload::PayloadBuildError> for CfgEditError {
    fn from(error: payload::PayloadBuildError) -> Self {
        match error {
            payload::PayloadBuildError::ResourceLimitExceeded { family } => {
                Self::ResourceLimitExceeded { family }
            }
            payload::PayloadBuildError::CapacityFailure { family } => {
                Self::CapacityFailure { family }
            }
        }
    }
}

/// An immutable CFG whose complete owner and payload stores passed structural
/// verification together.
#[derive(Debug)]
pub struct ValidatedCfg(pub(crate) Cfg);

impl std::ops::Deref for ValidatedCfg {
    type Target = Cfg;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Clone for ValidatedCfg {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl ValidatedCfg {
    pub(crate) fn into_editor(self) -> CfgEditor {
        self.0
    }

    /// Apply a mutation transaction to a private editor and publish it only if
    /// structural verification still succeeds.
    pub fn try_edit<R>(
        &mut self,
        type_pool: &rue_air::FrozenTypeInternPool,
        edit: impl FnOnce(&mut CfgEditor) -> Result<R, CfgEditError>,
    ) -> Result<R, CfgEditTransactionError> {
        let mut editor = self.0.clone();
        let result = edit(&mut editor).map_err(CfgEditTransactionError::Edit)?;
        editor
            .verify_with_type_pool(type_pool)
            .map_err(CfgEditTransactionError::Verification)?;
        self.0 = editor;
        Ok(result)
    }
}

impl Clone for Cfg {
    /// Clone the complete owner so every opaque payload range remains paired
    /// with the physical store whose positions it addresses.
    fn clone(&self) -> Self {
        Self {
            blocks: self
                .blocks
                .iter()
                .map(|block| BasicBlock {
                    id: block.id,
                    params: block.params.clone(),
                    insts: block.insts.clone(),
                    terminator: block.terminator.duplicate_with_owner(),
                })
                .collect(),
            entry: self.entry,
            return_type: self.return_type,
            values: self
                .values
                .iter()
                .map(|inst| CfgInst {
                    data: inst.data.duplicate_with_owner(),
                    ty: inst.ty,
                    span: inst.span,
                })
                .collect(),
            extra: self.extra.clone(),
            call_args: self.call_args.clone(),
            switch_cases: self.switch_cases.clone(),
            projections: self.projections.clone(),
            num_locals: self.num_locals,
            num_params: self.num_params,
            fn_name: self.fn_name.clone(),
            param_modes: self.param_modes.clone(),
            address_taken_slots: self.address_taken_slots.clone(),
            address_taken_params: self.address_taken_params.clone(),
            source_param_abi: self.source_param_abi.clone(),
            capacity_exceeded: self.capacity_exceeded,
        }
    }
}

impl Cfg {
    /// Clone and atomically remap every request-local payload domain embedded
    /// in this CFG. Block and value numbers are structural within the graph;
    /// types, nominal IDs, symbols, strings, and spans are supplied by the
    /// caller's stable projection/import boundary.
    pub fn try_remap_domains<E>(
        &self,
        mut ty: impl FnMut(Type) -> Result<Type, E>,
        mut strukt: impl FnMut(StructId) -> Result<StructId, E>,
        mut enm: impl FnMut(EnumId) -> Result<EnumId, E>,
        mut symbol: impl FnMut(Spur) -> Result<Spur, E>,
        mut string: impl FnMut(u32) -> Result<u32, E>,
        mut span: impl FnMut(Span) -> Result<Span, E>,
    ) -> Result<Self, CfgRemapError<E>> {
        use CfgInstData::*;
        let mut cfg = Self::new(
            ty(self.return_type).map_err(CfgRemapError::Domain)?,
            self.num_locals,
            self.num_params,
            self.fn_name.clone(),
            self.param_modes.clone(),
        );
        cfg.entry = self.entry;
        cfg.address_taken_slots = self.address_taken_slots.clone();
        cfg.address_taken_params = self.address_taken_params.clone();
        // Descriptors carry a type only for a by-value indirect aggregate
        // parameter, remapped across pools like any other CFG type (RUE-1005).
        cfg.source_param_abi = self
            .source_param_abi
            .iter()
            .map(|param| {
                Ok(rue_air::SourceParamAbi {
                    start_slot: param.start_slot,
                    slot_count: param.slot_count,
                    crossing_regs: param.crossing_regs,
                    ty: match param.ty {
                        Some(t) => Some(ty(t).map_err(CfgRemapError::Domain)?),
                        None => None,
                    },
                })
            })
            .collect::<Result<Vec<_>, CfgRemapError<E>>>()?;

        macro_rules! binary {
            ($variant:ident, $a:expr, $b:expr) => {
                $variant(*$a, *$b)
            };
        }
        macro_rules! domain {
            ($value:expr) => {
                $value.map_err(CfgRemapError::Domain)?
            };
        }
        for source in &self.values {
            let data = match &source.data {
                Const(value) => Const(*value),
                BoolConst(value) => BoolConst(*value),
                StringConst(index) => StringConst(domain!(string(*index))),
                Param { index } => Param { index: *index },
                BlockParam { index } => BlockParam { index: *index },
                Add(a, b) => binary!(Add, a, b),
                Sub(a, b) => binary!(Sub, a, b),
                Mul(a, b) => binary!(Mul, a, b),
                WrappingAdd(a, b) => binary!(WrappingAdd, a, b),
                WrappingSub(a, b) => binary!(WrappingSub, a, b),
                WrappingMul(a, b) => binary!(WrappingMul, a, b),
                Div(a, b) => binary!(Div, a, b),
                Mod(a, b) => binary!(Mod, a, b),
                Eq(a, b) => binary!(Eq, a, b),
                Ne(a, b) => binary!(Ne, a, b),
                Lt(a, b) => binary!(Lt, a, b),
                Gt(a, b) => binary!(Gt, a, b),
                Le(a, b) => binary!(Le, a, b),
                Ge(a, b) => binary!(Ge, a, b),
                BitAnd(a, b) => binary!(BitAnd, a, b),
                BitOr(a, b) => binary!(BitOr, a, b),
                BitXor(a, b) => binary!(BitXor, a, b),
                Shl(a, b) => binary!(Shl, a, b),
                Shr(a, b) => binary!(Shr, a, b),
                Neg(value) => Neg(*value),
                Not(value) => Not(*value),
                BitNot(value) => BitNot(*value),
                Alloc { slot, init } => Alloc {
                    slot: *slot,
                    init: *init,
                },
                Load { slot } => Load { slot: *slot },
                Store { slot, value } => Store {
                    slot: *slot,
                    value: *value,
                },
                ParamStore { param_slot, value } => ParamStore {
                    param_slot: *param_slot,
                    value: *value,
                },
                PlaceRead { place } => PlaceRead {
                    place: cfg
                        .make_place(
                            place.base,
                            domain!(ty(place.base_type)),
                            self.get_place_projections(place)
                                .iter()
                                .map(|projection| match projection {
                                    Projection::Field {
                                        struct_id,
                                        field_index,
                                    } => Ok(Projection::Field {
                                        struct_id: domain!(strukt(*struct_id)),
                                        field_index: *field_index,
                                    }),
                                    Projection::Index { array_type, index } => {
                                        Ok(Projection::Index {
                                            array_type: domain!(ty(*array_type)),
                                            index: *index,
                                        })
                                    }
                                })
                                .collect::<Result<Vec<_>, CfgRemapError<E>>>()?,
                        )
                        .map_err(CfgRemapError::Edit)?,
                },
                PlaceWrite { place, value } => PlaceWrite {
                    place: cfg
                        .make_place(
                            place.base,
                            domain!(ty(place.base_type)),
                            self.get_place_projections(place)
                                .iter()
                                .map(|projection| match projection {
                                    Projection::Field {
                                        struct_id,
                                        field_index,
                                    } => Ok(Projection::Field {
                                        struct_id: domain!(strukt(*struct_id)),
                                        field_index: *field_index,
                                    }),
                                    Projection::Index { array_type, index } => {
                                        Ok(Projection::Index {
                                            array_type: domain!(ty(*array_type)),
                                            index: *index,
                                        })
                                    }
                                })
                                .collect::<Result<Vec<_>, CfgRemapError<E>>>()?,
                        )
                        .map_err(CfgRemapError::Edit)?,
                    value: *value,
                },
                Call {
                    runtime,
                    name,
                    args,
                } => Call {
                    runtime: *runtime,
                    name: domain!(symbol(*name)),
                    args: cfg
                        .push_call_args(self.call_args(args).iter().copied())
                        .map_err(CfgRemapError::Edit)?,
                },
                AccessorCall { name, args } => AccessorCall {
                    name: domain!(symbol(*name)),
                    args: cfg
                        .push_call_args(self.call_args(args).iter().copied())
                        .map_err(CfgRemapError::Edit)?,
                },
                Intrinsic {
                    runtime,
                    name,
                    args,
                } => Intrinsic {
                    runtime: *runtime,
                    name: domain!(symbol(*name)),
                    args: cfg
                        .push_intrinsic_args(self.intrinsic_args(args).iter().copied())
                        .map_err(CfgRemapError::Edit)?,
                },
                StructInit { struct_id, fields } => StructInit {
                    struct_id: domain!(strukt(*struct_id)),
                    fields: cfg
                        .push_struct_fields(self.struct_fields(fields).iter().copied())
                        .map_err(CfgRemapError::Edit)?,
                },
                ArrayInit { elements } => ArrayInit {
                    elements: cfg
                        .push_array_elements(self.array_elements(elements).iter().copied())
                        .map_err(CfgRemapError::Edit)?,
                },
                EnumVariant {
                    enum_id,
                    variant_index,
                    payload,
                } => EnumVariant {
                    enum_id: domain!(enm(*enum_id)),
                    variant_index: *variant_index,
                    payload: cfg
                        .push_enum_payload(self.enum_payload(payload).iter().copied())
                        .map_err(CfgRemapError::Edit)?,
                },
                EnumPayloadGet {
                    base,
                    enum_id,
                    variant_index,
                    field_index,
                } => EnumPayloadGet {
                    base: *base,
                    enum_id: domain!(enm(*enum_id)),
                    variant_index: *variant_index,
                    field_index: *field_index,
                },
                IntCast { value, from_ty } => IntCast {
                    value: *value,
                    from_ty: domain!(ty(*from_ty)),
                },
                Drop { value } => Drop { value: *value },
                StorageLive { slot, local_ty } => StorageLive {
                    slot: *slot,
                    local_ty: domain!(ty(*local_ty)),
                },
                StorageDead { slot, local_ty } => StorageDead {
                    slot: *slot,
                    local_ty: domain!(ty(*local_ty)),
                },
            };
            cfg.add_inst(CfgInst {
                data,
                ty: domain!(ty(source.ty)),
                span: domain!(span(source.span)),
            });
        }

        for source in &self.blocks {
            let terminator = match &source.terminator {
                Terminator::Goto { target, args } => Terminator::Goto {
                    target: *target,
                    args: cfg
                        .push_goto_args(self.goto_args(args).iter().copied())
                        .map_err(CfgRemapError::Edit)?,
                },
                Terminator::Branch {
                    cond,
                    then_block,
                    then_args,
                    else_block,
                    else_args,
                } => Terminator::Branch {
                    cond: *cond,
                    then_block: *then_block,
                    then_args: cfg
                        .push_then_args(self.then_args(then_args).iter().copied())
                        .map_err(CfgRemapError::Edit)?,
                    else_block: *else_block,
                    else_args: cfg
                        .push_else_args(self.else_args(else_args).iter().copied())
                        .map_err(CfgRemapError::Edit)?,
                },
                Terminator::Switch {
                    scrutinee,
                    cases,
                    default,
                } => Terminator::Switch {
                    scrutinee: *scrutinee,
                    cases: cfg
                        .push_switch_cases(self.switch_cases(cases).iter().copied())
                        .map_err(CfgRemapError::Edit)?,
                    default: *default,
                },
                Terminator::Return { value } => Terminator::Return { value: *value },
                Terminator::Unreachable => Terminator::Unreachable,
                Terminator::None => Terminator::None,
            };
            cfg.blocks.push(BasicBlock {
                id: source.id,
                params: source
                    .params
                    .iter()
                    .map(|(value, value_ty)| Ok((*value, domain!(ty(*value_ty)))))
                    .collect::<Result<Vec<_>, CfgRemapError<E>>>()?,
                insts: source.insts.clone(),
                terminator,
            });
        }
        Ok(cfg)
    }

    /// Create a new CFG.
    pub fn new(
        return_type: Type,
        num_locals: u32,
        num_params: u32,
        fn_name: String,
        param_modes: impl Into<ParamSlotModes>,
    ) -> Self {
        Self {
            blocks: Vec::new(),
            entry: BlockId(0),
            return_type,
            values: Vec::new(),
            extra: payload::Values::new(),
            call_args: payload::CallArgs::new(),
            switch_cases: payload::SwitchCases::new(),
            projections: payload::Projections::new(),
            num_locals,
            num_params,
            fn_name,
            param_modes: param_modes.into(),
            address_taken_slots: std::collections::HashSet::new(),
            address_taken_params: std::collections::HashSet::new(),
            source_param_abi: Vec::new(),
            capacity_exceeded: None,
        }
    }

    /// Install the grouped per-source-parameter ABI descriptors derived by the
    /// CFG builder from the AIR (RUE-1005). See the `source_param_abi` field.
    #[inline]
    pub fn set_source_param_abi(&mut self, descriptors: Vec<rue_air::SourceParamAbi>) {
        self.source_param_abi = descriptors;
    }

    /// Record that `slot`'s address escapes through an address-taking
    /// intrinsic (`@raw` / `@raw_mut` / `@field_ptr`), pinning its loads as
    /// places for the optimizer (RUE-521).
    #[inline]
    pub fn mark_address_taken(&mut self, slot: u32) {
        self.address_taken_slots.insert(slot);
    }

    /// Whether `slot`'s address escapes through an address-taking intrinsic
    /// (see [`Cfg::mark_address_taken`]).
    #[inline]
    pub fn is_address_taken(&self, slot: u32) -> bool {
        self.address_taken_slots.contains(&slot)
    }

    /// Record that parameter ABI slot `slot`'s address escapes through an
    /// address-taking intrinsic. See the `address_taken_params` field docs:
    /// repeated reads of such a parameter are not interchangeable, because a
    /// raw pointer into its storage may write between them.
    #[inline]
    pub fn mark_param_address_taken(&mut self, slot: u32) {
        self.address_taken_params.insert(slot);
    }

    /// Whether parameter ABI slot `slot`'s address escapes through an
    /// address-taking intrinsic (see [`Cfg::mark_param_address_taken`]).
    #[inline]
    pub fn is_param_address_taken(&self, slot: u32) -> bool {
        self.address_taken_params.contains(&slot)
    }

    /// Get the return type.
    #[inline]
    pub fn return_type(&self) -> Type {
        self.return_type
    }

    /// Get the number of local variable slots.
    #[inline]
    pub fn num_locals(&self) -> u32 {
        self.num_locals
    }

    /// Allocate a new temporary local slot for spilling computed values.
    ///
    /// This is used during CFG construction when a computed value (e.g., method
    /// call result) needs to be accessed via a place expression. The value is
    /// spilled to this temporary slot.
    ///
    /// Returns the slot number for the new local.
    #[inline]
    pub fn alloc_temp_local(&mut self) -> u32 {
        self.alloc_temp_local_slots(1)
    }

    /// Allocate a contiguous temporary frame region and return its base slot.
    ///
    /// Multi-slot aggregate spills must reserve their complete ABI width;
    /// reserving only the first slot lets codegen overwrite the following
    /// local or parameter area. Callers use at least one slot for zero-sized
    /// roots that still need a concrete address.
    #[inline]
    pub fn alloc_temp_local_slots(&mut self, slots: u32) -> u32 {
        assert!(
            slots > 0,
            "temporary frame regions must reserve at least one slot"
        );
        let slot = self.num_locals;
        self.num_locals = self
            .num_locals
            .checked_add(slots)
            .expect("temporary frame slot count overflow");
        slot
    }

    /// Get the number of parameter slots.
    #[inline]
    pub fn num_params(&self) -> u32 {
        self.num_params
    }

    /// Get the function name.
    #[inline]
    pub fn fn_name(&self) -> &str {
        &self.fn_name
    }

    /// Get whether a parameter slot uses the physical by-reference ABI.
    ///
    /// This is true for both logical `borrow` and logical `inout`. Physical
    /// transport is independent of mutation permission; use
    /// [`Cfg::is_param_writable`] to ask whether the callee may mutate it.
    #[inline]
    pub fn is_param_by_ref(&self, slot: u32) -> bool {
        self.param_modes
            .by_ref()
            .get(slot as usize)
            .copied()
            .unwrap_or(false)
    }

    /// Get whether a parameter ABI slot is logically writable: `inout`
    /// (by-ref, written back to the caller) or a `mut self` receiver slot
    /// (by-value, callee-local mutation only). Combine with
    /// [`Cfg::is_param_by_ref`] to distinguish the two.
    #[inline]
    pub fn is_param_writable(&self, slot: u32) -> bool {
        self.param_modes
            .writable()
            .get(slot as usize)
            .copied()
            .unwrap_or(false)
    }

    /// Get the parameter modes slice.
    #[inline]
    pub fn param_modes(&self) -> &[bool] {
        self.param_modes.by_ref()
    }

    /// Get the grouped per-source-parameter ABI descriptors (RUE-1005), or an
    /// empty slice for a directly constructed CFG (synthetic tests), in which
    /// case code generation homes one incoming register per parameter slot.
    #[inline]
    pub fn source_param_abi(&self) -> &[rue_air::SourceParamAbi] {
        &self.source_param_abi
    }

    /// Create a new basic block and return its ID.
    ///
    /// A `BlockId` is a `u32` index into this function's block arena, so one
    /// function holds at most [`MAX_CFG_ENTITIES_PER_FUNCTION`] blocks. Past
    /// that, `len() as u32` would wrap block 2^32 onto the entry block and
    /// silently reroute control flow. This allocator has no fallible callers,
    /// so it latches [`Cfg::capacity_exceeded`] and hands back an existing
    /// block; the construction and optimization boundaries turn the latch into
    /// an `E1401` diagnostic before the graph is published (spec C.1:2).
    pub fn new_block(&mut self) -> BlockId {
        let Some(index) = checked_owner_index(self.blocks.len()) else {
            self.capacity_exceeded.get_or_insert("basic blocks");
            // The arena is full, so the entry block always exists.
            return BlockId(0);
        };
        let id = BlockId(index);
        self.blocks.push(BasicBlock::new(id));
        id
    }

    /// The implementation-limit rejection latched by an infallible
    /// [`Self::new_block`] or [`Self::add_inst`] that ran past the published
    /// per-function ceiling, if any (spec Appendix C.6:1).
    pub(crate) fn latched_capacity_error(&self) -> Option<CfgEditError> {
        self.capacity_exceeded
            .map(|family| CfgEditError::OwnerLimitExceeded { family })
    }

    /// Get a block by ID.
    #[inline]
    pub fn get_block(&self, id: BlockId) -> &BasicBlock {
        &self.blocks[id.0 as usize]
    }

    /// Get a block mutably by ID.
    #[inline]
    pub(crate) fn get_block_mut(&mut self, id: BlockId) -> &mut BasicBlock {
        &mut self.blocks[id.0 as usize]
    }

    /// Add an instruction and return its value reference.
    ///
    /// A `CfgValue` is a `u32` index into this function's value arena, so one
    /// function holds at most [`MAX_CFG_ENTITIES_PER_FUNCTION`] values. See
    /// [`Self::new_block`] for why the ceiling is latched rather than returned.
    pub(crate) fn add_inst(&mut self, inst: CfgInst) -> CfgValue {
        let Some(index) = checked_owner_index(self.values.len()) else {
            self.capacity_exceeded.get_or_insert("values");
            // The arena is full, so value 0 always exists.
            return CfgValue::from_raw(0);
        };
        let value = CfgValue::from_raw(index);
        self.values.push(inst);
        value
    }

    /// Get an instruction by value reference.
    #[inline]
    pub fn get_inst(&self, value: CfgValue) -> &CfgInst {
        &self.values[value.0 as usize]
    }

    /// Get a mutable instruction by value reference.
    #[inline]
    pub(crate) fn get_inst_mut(&mut self, value: CfgValue) -> &mut CfgInst {
        &mut self.values[value.0 as usize]
    }

    /// Get the total number of values (instructions) in the CFG.
    #[inline]
    pub fn value_count(&self) -> usize {
        self.values.len()
    }

    pub(crate) fn push_intrinsic_args<I>(
        &mut self,
        values: I,
    ) -> Result<CfgIntrinsicArgs, CfgEditError>
    where
        I: IntoIterator<Item = CfgValue>,
        I::IntoIter: ExactSizeIterator,
    {
        payload::push_intrinsic_args(&mut self.extra, values).map_err(Into::into)
    }
    pub(crate) fn intrinsic_args(&self, range: &CfgIntrinsicArgs) -> &[CfgValue] {
        payload::intrinsic_args(&self.extra, range)
    }
    pub(crate) fn checked_intrinsic_args(
        &self,
        range: &CfgIntrinsicArgs,
    ) -> Result<&[CfgValue], payload::PayloadError> {
        payload::checked_intrinsic_args(&self.extra, range)
    }
    pub(crate) fn push_struct_fields<I>(
        &mut self,
        values: I,
    ) -> Result<CfgStructFields, CfgEditError>
    where
        I: IntoIterator<Item = CfgValue>,
        I::IntoIter: ExactSizeIterator,
    {
        payload::push_struct_fields(&mut self.extra, values).map_err(Into::into)
    }
    pub(crate) fn struct_fields(&self, range: &CfgStructFields) -> &[CfgValue] {
        payload::struct_fields(&self.extra, range)
    }
    pub(crate) fn checked_struct_fields(
        &self,
        range: &CfgStructFields,
    ) -> Result<&[CfgValue], payload::PayloadError> {
        payload::checked_struct_fields(&self.extra, range)
    }
    pub(crate) fn push_array_elements<I>(
        &mut self,
        values: I,
    ) -> Result<CfgArrayElements, CfgEditError>
    where
        I: IntoIterator<Item = CfgValue>,
        I::IntoIter: ExactSizeIterator,
    {
        payload::push_array_elements(&mut self.extra, values).map_err(Into::into)
    }
    pub(crate) fn array_elements(&self, range: &CfgArrayElements) -> &[CfgValue] {
        payload::array_elements(&self.extra, range)
    }
    pub(crate) fn checked_array_elements(
        &self,
        range: &CfgArrayElements,
    ) -> Result<&[CfgValue], payload::PayloadError> {
        payload::checked_array_elements(&self.extra, range)
    }
    pub(crate) fn push_enum_payload<I>(&mut self, values: I) -> Result<CfgEnumPayload, CfgEditError>
    where
        I: IntoIterator<Item = CfgValue>,
        I::IntoIter: ExactSizeIterator,
    {
        payload::push_enum_payload(&mut self.extra, values).map_err(Into::into)
    }
    pub(crate) fn enum_payload(&self, range: &CfgEnumPayload) -> &[CfgValue] {
        payload::enum_payload(&self.extra, range)
    }
    pub(crate) fn checked_enum_payload(
        &self,
        range: &CfgEnumPayload,
    ) -> Result<&[CfgValue], payload::PayloadError> {
        payload::checked_enum_payload(&self.extra, range)
    }
    pub(crate) fn push_goto_args<I>(&mut self, values: I) -> Result<CfgGotoArgs, CfgEditError>
    where
        I: IntoIterator<Item = CfgValue>,
        I::IntoIter: ExactSizeIterator,
    {
        payload::push_goto_args(&mut self.extra, values).map_err(Into::into)
    }
    pub(crate) fn goto_args(&self, range: &CfgGotoArgs) -> &[CfgValue] {
        payload::goto_args(&self.extra, range)
    }
    pub(crate) fn checked_goto_args(
        &self,
        range: &CfgGotoArgs,
    ) -> Result<&[CfgValue], payload::PayloadError> {
        payload::checked_goto_args(&self.extra, range)
    }
    pub(crate) fn then_args(&self, range: &CfgThenArgs) -> &[CfgValue] {
        payload::then_args(&self.extra, range)
    }
    pub(crate) fn checked_then_args(
        &self,
        range: &CfgThenArgs,
    ) -> Result<&[CfgValue], payload::PayloadError> {
        payload::checked_then_args(&self.extra, range)
    }
    pub(crate) fn else_args(&self, range: &CfgElseArgs) -> &[CfgValue] {
        payload::else_args(&self.extra, range)
    }
    pub(crate) fn checked_else_args(
        &self,
        range: &CfgElseArgs,
    ) -> Result<&[CfgValue], payload::PayloadError> {
        payload::checked_else_args(&self.extra, range)
    }
    pub(crate) fn push_then_args<I>(&mut self, values: I) -> Result<CfgThenArgs, CfgEditError>
    where
        I: IntoIterator<Item = CfgValue>,
        I::IntoIter: ExactSizeIterator,
    {
        payload::push_then_args(&mut self.extra, values).map_err(Into::into)
    }
    pub(crate) fn push_else_args<I>(&mut self, values: I) -> Result<CfgElseArgs, CfgEditError>
    where
        I: IntoIterator<Item = CfgValue>,
        I::IntoIter: ExactSizeIterator,
    {
        payload::push_else_args(&mut self.extra, values).map_err(Into::into)
    }
    pub(crate) fn push_call_args<I>(&mut self, args: I) -> Result<CfgCallArgs, CfgEditError>
    where
        I: IntoIterator<Item = CfgCallArg>,
        I::IntoIter: ExactSizeIterator,
    {
        payload::push_call_args(&mut self.call_args, args).map_err(Into::into)
    }
    pub(crate) fn call_args(&self, range: &CfgCallArgs) -> &[CfgCallArg] {
        payload::call_args(&self.call_args, range)
    }
    pub(crate) fn checked_call_args(
        &self,
        range: &CfgCallArgs,
    ) -> Result<&[CfgCallArg], payload::PayloadError> {
        payload::checked_call_args(&self.call_args, range)
    }

    /// Borrow call arguments from an instruction without exposing its opaque
    /// owner-bound payload range.
    pub fn get_call_args<'a>(&'a self, data: &CfgInstData) -> &'a [CfgCallArg] {
        match data {
            CfgInstData::Call { args, .. } => self.call_args(args),
            _ => panic!("get_call_args called on non-Call instruction"),
        }
    }

    pub fn get_intrinsic_args<'a>(&'a self, data: &CfgInstData) -> &'a [CfgValue] {
        match data {
            CfgInstData::Intrinsic { args, .. } => self.intrinsic_args(args),
            _ => panic!("get_intrinsic_args called on non-Intrinsic instruction"),
        }
    }

    pub fn get_struct_fields<'a>(&'a self, data: &CfgInstData) -> &'a [CfgValue] {
        match data {
            CfgInstData::StructInit { fields, .. } => self.struct_fields(fields),
            _ => panic!("get_struct_fields called on non-StructInit instruction"),
        }
    }

    pub fn get_array_elements<'a>(&'a self, data: &CfgInstData) -> &'a [CfgValue] {
        match data {
            CfgInstData::ArrayInit { elements } => self.array_elements(elements),
            _ => panic!("get_array_elements called on non-ArrayInit instruction"),
        }
    }

    pub fn get_enum_payload<'a>(&'a self, data: &CfgInstData) -> &'a [CfgValue] {
        match data {
            CfgInstData::EnumVariant { payload, .. } => self.enum_payload(payload),
            _ => panic!("get_enum_payload called on non-EnumVariant instruction"),
        }
    }
    pub fn replace_call_args(
        &mut self,
        value: CfgValue,
        values: impl IntoIterator<Item = CfgCallArg>,
    ) -> Result<(), CfgEditError> {
        const OP: &str = "call arguments";
        if !matches!(
            self.values.get(value.0 as usize).map(|inst| &inst.data),
            Some(CfgInstData::Call { .. })
        ) {
            return Err(Self::invalid_edit(OP, "target is not a Call instruction"));
        }
        let staged = Self::stage_edit(OP, values)?;
        self.validate_value_refs(OP, staged.iter(), |arg| arg.value)?;
        let args = payload::push_call_args(&mut self.call_args, staged)?;
        let CfgInstData::Call { args: stored, .. } = &mut self.values[value.0 as usize].data else {
            return Err(Self::invalid_edit(OP, "target changed during replacement"));
        };
        *stored = args;
        Ok(())
    }
    pub fn replace_intrinsic_args(
        &mut self,
        value: CfgValue,
        values: impl IntoIterator<Item = CfgValue>,
    ) -> Result<(), CfgEditError> {
        const OP: &str = "intrinsic arguments";
        if !matches!(
            self.values.get(value.0 as usize).map(|inst| &inst.data),
            Some(CfgInstData::Intrinsic { .. })
        ) {
            return Err(Self::invalid_edit(
                OP,
                "target is not an Intrinsic instruction",
            ));
        }
        let staged = Self::stage_edit(OP, values)?;
        self.validate_value_refs(OP, staged.iter(), |value| *value)?;
        let args = payload::push_intrinsic_args(&mut self.extra, staged)?;
        let CfgInstData::Intrinsic { args: stored, .. } = &mut self.values[value.0 as usize].data
        else {
            return Err(Self::invalid_edit(OP, "target changed during replacement"));
        };
        *stored = args;
        Ok(())
    }

    /// Replace the static result type of an instruction.
    ///
    /// This is primarily useful to construct verifier and oracle probes while
    /// keeping the instruction arena encapsulated.
    pub fn replace_inst_type(&mut self, value: CfgValue, ty: Type) -> Result<(), CfgEditError> {
        let Some(inst) = self.values.get_mut(value.0 as usize) else {
            return Err(Self::invalid_edit(
                "instruction type",
                "target instruction is undefined",
            ));
        };
        inst.ty = ty;
        Ok(())
    }

    /// Replace an integer cast without exposing mutable instruction storage.
    pub fn replace_int_cast(
        &mut self,
        value: CfgValue,
        operand: CfgValue,
        from_ty: Type,
    ) -> Result<(), CfgEditError> {
        const OP: &str = "integer cast";
        if operand.0 as usize >= self.values.len() {
            return Err(Self::invalid_edit(OP, "operand is undefined"));
        }
        let Some(data) = self
            .values
            .get_mut(value.0 as usize)
            .map(|inst| &mut inst.data)
        else {
            return Err(Self::invalid_edit(OP, "target instruction is undefined"));
        };
        if !matches!(data, CfgInstData::IntCast { .. }) {
            return Err(Self::invalid_edit(
                OP,
                "target is not an IntCast instruction",
            ));
        }
        *data = CfgInstData::IntCast {
            value: operand,
            from_ty,
        };
        Ok(())
    }

    /// Replace a struct initializer's fields in this CFG's payload store.
    pub fn replace_struct_fields(
        &mut self,
        value: CfgValue,
        values: impl IntoIterator<Item = CfgValue>,
    ) -> Result<(), CfgEditError> {
        const OP: &str = "struct fields";
        if !matches!(
            self.values.get(value.0 as usize).map(|inst| &inst.data),
            Some(CfgInstData::StructInit { .. })
        ) {
            return Err(Self::invalid_edit(
                OP,
                "target is not a StructInit instruction",
            ));
        }
        let staged = Self::stage_edit(OP, values)?;
        self.validate_value_refs(OP, staged.iter(), |value| *value)?;
        let fields = payload::push_struct_fields(&mut self.extra, staged)?;
        let CfgInstData::StructInit { fields: stored, .. } =
            &mut self.values[value.0 as usize].data
        else {
            return Err(Self::invalid_edit(OP, "target changed during replacement"));
        };
        *stored = fields;
        Ok(())
    }

    /// Replace an array initializer's elements atomically.
    pub fn replace_array_elements(
        &mut self,
        value: CfgValue,
        values: impl IntoIterator<Item = CfgValue>,
    ) -> Result<(), CfgEditError> {
        const OP: &str = "array elements";
        if !matches!(
            self.values.get(value.0 as usize).map(|inst| &inst.data),
            Some(CfgInstData::ArrayInit { .. })
        ) {
            return Err(Self::invalid_edit(
                OP,
                "target is not an ArrayInit instruction",
            ));
        }
        let staged = Self::stage_edit(OP, values)?;
        self.validate_value_refs(OP, staged.iter(), |value| *value)?;
        let elements = payload::push_array_elements(&mut self.extra, staged)?;
        let CfgInstData::ArrayInit { elements: stored } = &mut self.values[value.0 as usize].data
        else {
            return Err(Self::invalid_edit(OP, "target changed during replacement"));
        };
        *stored = elements;
        Ok(())
    }

    /// Replace an enum constructor's payload atomically after validating its
    /// nominal target and variant against the canonical type context.
    pub fn replace_enum_payload(
        &mut self,
        type_pool: &rue_air::FrozenTypeInternPool,
        value: CfgValue,
        values: impl IntoIterator<Item = CfgValue>,
    ) -> Result<(), CfgEditError> {
        const OP: &str = "enum payload";
        let Some(CfgInstData::EnumVariant {
            enum_id,
            variant_index,
            ..
        }) = self.values.get(value.0 as usize).map(|inst| &inst.data)
        else {
            return Err(Self::invalid_edit(
                OP,
                "target is not an EnumVariant instruction",
            ));
        };
        let Some(def) = type_pool.try_enum_def(*enum_id) else {
            return Err(Self::invalid_edit(OP, "enum target is undefined"));
        };
        if *variant_index as usize >= def.variant_count() {
            return Err(Self::invalid_edit(OP, "enum variant is out of bounds"));
        }
        let staged = Self::stage_edit(OP, values)?;
        self.validate_value_refs(OP, staged.iter(), |value| *value)?;
        let payload = payload::push_enum_payload(&mut self.extra, staged)?;
        let CfgInstData::EnumVariant {
            payload: stored, ..
        } = &mut self.values[value.0 as usize].data
        else {
            return Err(Self::invalid_edit(OP, "target changed during replacement"));
        };
        *stored = payload;
        Ok(())
    }

    /// Replace a place read, storing its projections in this CFG owner.
    pub fn replace_place_read(
        &mut self,
        value: CfgValue,
        base: PlaceBase,
        base_type: Type,
        projections: impl IntoIterator<Item = Projection>,
    ) -> Result<(), CfgEditError> {
        const OP: &str = "projections";
        if !matches!(
            self.values.get(value.0 as usize).map(|inst| &inst.data),
            Some(CfgInstData::PlaceRead { .. })
        ) {
            return Err(Self::invalid_edit(
                OP,
                "target is not a PlaceRead instruction",
            ));
        }
        let staged = Self::stage_edit(OP, projections)?;
        self.validate_place_input(OP, base, &staged)?;
        let projections = payload::push_projections(&mut self.projections, staged)?;
        self.values[value.0 as usize].data = CfgInstData::PlaceRead {
            place: Place {
                base,
                base_type,
                projections,
            },
        };
        Ok(())
    }

    /// Replace a place write, storing its projections in this CFG owner.
    pub fn replace_place_write(
        &mut self,
        instruction: CfgValue,
        base: PlaceBase,
        base_type: Type,
        projections: impl IntoIterator<Item = Projection>,
        value: CfgValue,
    ) -> Result<(), CfgEditError> {
        const OP: &str = "projections";
        if !matches!(
            self.values
                .get(instruction.0 as usize)
                .map(|inst| &inst.data),
            Some(CfgInstData::PlaceWrite { .. })
        ) {
            return Err(Self::invalid_edit(
                OP,
                "target is not a PlaceWrite instruction",
            ));
        }
        if value.0 as usize >= self.values.len() {
            return Err(Self::invalid_edit(OP, "written value is undefined"));
        }
        let staged = Self::stage_edit(OP, projections)?;
        self.validate_place_input(OP, base, &staged)?;
        let projections = payload::push_projections(&mut self.projections, staged)?;
        self.values[instruction.0 as usize].data = CfgInstData::PlaceWrite {
            place: Place {
                base,
                base_type,
                projections,
            },
            value,
        };
        Ok(())
    }
    pub(crate) fn push_switch_cases<I>(&mut self, cases: I) -> Result<CfgSwitchCases, CfgEditError>
    where
        I: IntoIterator<Item = (i64, BlockId)>,
        I::IntoIter: ExactSizeIterator,
    {
        payload::push_switch_cases(&mut self.switch_cases, cases).map_err(Into::into)
    }
    pub(crate) fn switch_cases(&self, range: &CfgSwitchCases) -> &[(i64, BlockId)] {
        payload::switch_cases(&self.switch_cases, range)
    }
    pub(crate) fn checked_switch_cases(
        &self,
        range: &CfgSwitchCases,
    ) -> Result<&[(i64, BlockId)], payload::PayloadError> {
        payload::checked_switch_cases(&self.switch_cases, range)
    }

    pub fn get_switch_cases<'a>(&'a self, term: &Terminator) -> &'a [(i64, BlockId)] {
        match term {
            Terminator::Switch { cases, .. } => self.switch_cases(cases),
            _ => panic!("get_switch_cases called on non-Switch terminator"),
        }
    }
    pub(crate) fn push_projections<I>(&mut self, projs: I) -> Result<CfgProjections, CfgEditError>
    where
        I: IntoIterator<Item = Projection>,
        I::IntoIter: ExactSizeIterator,
    {
        payload::push_projections(&mut self.projections, projs).map_err(Into::into)
    }

    /// Get projections for a place.
    #[inline]
    pub fn get_place_projections(&self, place: &Place) -> &[Projection] {
        payload::projections(&self.projections, &place.projections)
    }
    pub(crate) fn checked_place_projections(
        &self,
        place: &Place,
    ) -> Result<&[Projection], payload::PayloadError> {
        payload::checked_projections(&self.projections, &place.projections)
    }

    /// Create a place with the given base and projections.
    ///
    /// This adds the projections to the projections array and returns a Place
    /// that references them.
    pub(crate) fn make_place<I>(
        &mut self,
        base: PlaceBase,
        base_type: Type,
        projs: I,
    ) -> Result<Place, CfgEditError>
    where
        I: IntoIterator<Item = Projection>,
        I::IntoIter: ExactSizeIterator,
    {
        let projections = self.push_projections(projs)?;
        Ok(Place {
            base,
            base_type,
            projections,
        })
    }

    /// Get the block arguments from a Goto terminator.
    ///
    /// # Panics
    ///
    /// Panics if the terminator is not a Goto.
    #[inline]
    pub fn get_goto_args(&self, term: &Terminator) -> &[CfgValue] {
        match term {
            Terminator::Goto { args, .. } => payload::goto_args(&self.extra, args),
            _ => panic!("get_goto_args called on non-Goto terminator"),
        }
    }

    /// Get the then branch arguments from a Branch terminator.
    ///
    /// # Panics
    ///
    /// Panics if the terminator is not a Branch.
    #[inline]
    pub fn get_branch_then_args(&self, term: &Terminator) -> &[CfgValue] {
        match term {
            Terminator::Branch { then_args, .. } => payload::then_args(&self.extra, then_args),
            _ => panic!("get_branch_then_args called on non-Branch terminator"),
        }
    }

    /// Get the else branch arguments from a Branch terminator.
    ///
    /// # Panics
    ///
    /// Panics if the terminator is not a Branch.
    #[inline]
    pub fn get_branch_else_args(&self, term: &Terminator) -> &[CfgValue] {
        match term {
            Terminator::Branch { else_args, .. } => payload::else_args(&self.extra, else_args),
            _ => panic!("get_branch_else_args called on non-Branch terminator"),
        }
    }

    /// Add an instruction to a block.
    pub(crate) fn add_inst_to_block(&mut self, block: BlockId, inst: CfgInst) -> CfgValue {
        let value = self.add_inst(inst);
        self.blocks[block.0 as usize].insts.push(value);
        value
    }

    fn invalid_edit(operation: &'static str, detail: &'static str) -> CfgEditError {
        CfgEditError::InvalidBuilderInput { operation, detail }
    }

    fn stage_edit<E>(
        operation: &'static str,
        values: impl IntoIterator<Item = E>,
    ) -> Result<Vec<E>, CfgEditError> {
        let values = values.into_iter();
        let (lower, _) = values.size_hint();
        if lower > u32::MAX as usize {
            return Err(CfgEditError::ResourceLimitExceeded { family: operation });
        }
        let mut staged = Vec::new();
        staged
            .try_reserve(lower)
            .map_err(|_| CfgEditError::CapacityFailure { family: operation })?;
        for value in values {
            if staged.len() == u32::MAX as usize {
                return Err(CfgEditError::ResourceLimitExceeded { family: operation });
            }
            staged
                .try_reserve(1)
                .map_err(|_| CfgEditError::CapacityFailure { family: operation })?;
            staged.push(value);
        }
        Ok(staged)
    }

    fn preflight_append(
        &mut self,
        operation: &'static str,
        block: BlockId,
    ) -> Result<(), CfgEditError> {
        let block = self
            .blocks
            .get_mut(block.0 as usize)
            .ok_or_else(|| Self::invalid_edit(operation, "block is out of bounds"))?;
        self.values
            .try_reserve(1)
            .map_err(|_| CfgEditError::CapacityFailure { family: "values" })?;
        block
            .insts
            .try_reserve(1)
            .map_err(|_| CfgEditError::CapacityFailure {
                family: "block instructions",
            })
    }

    fn validate_value_refs<E>(
        &self,
        operation: &'static str,
        values: impl IntoIterator<Item = E>,
        value: impl Fn(E) -> CfgValue,
    ) -> Result<(), CfgEditError> {
        if values
            .into_iter()
            .any(|item| value(item).0 as usize >= self.values.len())
        {
            return Err(Self::invalid_edit(operation, "payload value is undefined"));
        }
        Ok(())
    }

    fn validate_place_input(
        &self,
        operation: &'static str,
        base: PlaceBase,
        projections: &[Projection],
    ) -> Result<(), CfgEditError> {
        let base_valid = match base {
            PlaceBase::Local(slot) => slot < self.num_locals,
            PlaceBase::Param(slot) => slot < self.num_params,
            PlaceBase::Accessor(value) => {
                (value.as_u32() as usize) < self.values.len()
                    && matches!(self.get_inst(value).data, CfgInstData::AccessorCall { .. })
            }
        };
        if !base_valid {
            return Err(Self::invalid_edit(operation, "place base is out of bounds"));
        }
        if projections.iter().any(|projection| {
            matches!(projection, Projection::Index { index, .. } if index.0 as usize >= self.values.len())
        }) {
            return Err(Self::invalid_edit(
                operation,
                "projection index value is undefined",
            ));
        }
        Ok(())
    }

    /// Append an instruction whose data has no owner-bound payload.
    ///
    /// Payload-bearing instructions cannot be constructed outside this crate;
    /// use the corresponding `append_*` method so the payload and its opaque
    /// range are created by the same CFG owner.
    pub fn append_inst(&mut self, block: BlockId, inst: CfgInst) -> CfgValue {
        self.add_inst_to_block(block, inst)
    }

    /// Append a call and store its arguments in this CFG's call-argument store.
    pub fn append_call(
        &mut self,
        block: BlockId,
        runtime: Option<rue_air::RuntimeCallKind>,
        name: Spur,
        args: impl IntoIterator<Item = CfgCallArg>,
        ty: Type,
        span: Span,
    ) -> Result<CfgValue, CfgEditError> {
        const OP: &str = "call arguments";
        let staged = Self::stage_edit(OP, args)?;
        self.validate_value_refs(OP, staged.iter(), |arg| arg.value)?;
        self.preflight_append(OP, block)?;
        let args = payload::push_call_args(&mut self.call_args, staged)?;
        Ok(self.add_inst_to_block(
            block,
            CfgInst {
                data: CfgInstData::Call {
                    runtime,
                    name,
                    args,
                },
                ty,
                span,
            },
        ))
    }

    /// Append an intrinsic and store its operands in this CFG's value store.
    pub fn append_intrinsic(
        &mut self,
        block: BlockId,
        runtime: Option<rue_air::RuntimeCallKind>,
        name: Spur,
        args: impl IntoIterator<Item = CfgValue>,
        ty: Type,
        span: Span,
    ) -> Result<CfgValue, CfgEditError> {
        const OP: &str = "intrinsic arguments";
        let staged = Self::stage_edit(OP, args)?;
        self.validate_value_refs(OP, staged.iter(), |value| *value)?;
        self.preflight_append(OP, block)?;
        let args = payload::push_intrinsic_args(&mut self.extra, staged)?;
        Ok(self.add_inst_to_block(
            block,
            CfgInst {
                data: CfgInstData::Intrinsic {
                    runtime,
                    name,
                    args,
                },
                ty,
                span,
            },
        ))
    }

    /// Append a struct initializer owned by this CFG.
    pub fn append_struct_init(
        &mut self,
        block: BlockId,
        struct_id: StructId,
        fields: impl IntoIterator<Item = CfgValue>,
        ty: Type,
        span: Span,
    ) -> Result<CfgValue, CfgEditError> {
        const OP: &str = "struct fields";
        let staged = Self::stage_edit(OP, fields)?;
        self.validate_value_refs(OP, staged.iter(), |value| *value)?;
        self.preflight_append(OP, block)?;
        let fields = payload::push_struct_fields(&mut self.extra, staged)?;
        Ok(self.add_inst_to_block(
            block,
            CfgInst {
                data: CfgInstData::StructInit { struct_id, fields },
                ty,
                span,
            },
        ))
    }

    /// Append an array initializer owned by this CFG.
    pub fn append_array_init(
        &mut self,
        block: BlockId,
        elements: impl IntoIterator<Item = CfgValue>,
        ty: Type,
        span: Span,
    ) -> Result<CfgValue, CfgEditError> {
        const OP: &str = "array elements";
        let staged = Self::stage_edit(OP, elements)?;
        self.validate_value_refs(OP, staged.iter(), |value| *value)?;
        self.preflight_append(OP, block)?;
        let elements = payload::push_array_elements(&mut self.extra, staged)?;
        Ok(self.add_inst_to_block(
            block,
            CfgInst {
                data: CfgInstData::ArrayInit { elements },
                ty,
                span,
            },
        ))
    }

    /// Append an enum variant owned by this CFG.
    pub fn append_enum_variant(
        &mut self,
        type_pool: &rue_air::FrozenTypeInternPool,
        block: BlockId,
        enum_id: EnumId,
        variant_index: u32,
        payload: impl IntoIterator<Item = CfgValue>,
        ty: Type,
        span: Span,
    ) -> Result<CfgValue, CfgEditError> {
        const OP: &str = "enum payload";
        let Some(def) = type_pool.try_enum_def(enum_id) else {
            return Err(Self::invalid_edit(OP, "enum target is undefined"));
        };
        if variant_index as usize >= def.variant_count() {
            return Err(Self::invalid_edit(OP, "enum variant is out of bounds"));
        }
        let staged = Self::stage_edit(OP, payload)?;
        self.validate_value_refs(OP, staged.iter(), |value| *value)?;
        self.preflight_append(OP, block)?;
        let payload = payload::push_enum_payload(&mut self.extra, staged)?;
        Ok(self.add_inst_to_block(
            block,
            CfgInst {
                data: CfgInstData::EnumVariant {
                    enum_id,
                    variant_index,
                    payload,
                },
                ty,
                span,
            },
        ))
    }

    /// Append a place read whose projections are owned by this CFG.
    pub fn append_place_read(
        &mut self,
        block: BlockId,
        base: PlaceBase,
        base_type: Type,
        projections: impl IntoIterator<Item = Projection>,
        ty: Type,
        span: Span,
    ) -> Result<CfgValue, CfgEditError> {
        const OP: &str = "projections";
        let staged = Self::stage_edit(OP, projections)?;
        self.validate_place_input(OP, base, &staged)?;
        self.preflight_append(OP, block)?;
        let projections = payload::push_projections(&mut self.projections, staged)?;
        let place = Place {
            base,
            base_type,
            projections,
        };
        Ok(self.add_inst_to_block(
            block,
            CfgInst {
                data: CfgInstData::PlaceRead { place },
                ty,
                span,
            },
        ))
    }

    /// Append a place write whose projections are owned by this CFG.
    #[allow(clippy::too_many_arguments)]
    pub fn append_place_write(
        &mut self,
        block: BlockId,
        base: PlaceBase,
        base_type: Type,
        projections: impl IntoIterator<Item = Projection>,
        value: CfgValue,
        ty: Type,
        span: Span,
    ) -> Result<CfgValue, CfgEditError> {
        const OP: &str = "projections";
        if value.0 as usize >= self.values.len() {
            return Err(Self::invalid_edit(OP, "written value is undefined"));
        }
        let staged = Self::stage_edit(OP, projections)?;
        self.validate_place_input(OP, base, &staged)?;
        self.preflight_append(OP, block)?;
        let projections = payload::push_projections(&mut self.projections, staged)?;
        let place = Place {
            base,
            base_type,
            projections,
        };
        Ok(self.add_inst_to_block(
            block,
            CfgInst {
                data: CfgInstData::PlaceWrite { place, value },
                ty,
                span,
            },
        ))
    }

    /// Add a block parameter and return its value.
    pub fn add_block_param(&mut self, block: BlockId, ty: Type) -> CfgValue {
        let param_index = self.blocks[block.0 as usize].params.len() as u32;
        let inst = CfgInst {
            data: CfgInstData::BlockParam { index: param_index },
            ty,
            span: Span::new(0, 0),
        };
        let value = self.add_inst(inst);
        self.blocks[block.0 as usize].params.push((value, ty));
        value
    }

    /// Set the terminator for a block.
    ///
    /// A block's terminator may only be set once (from `None`), or may replace
    /// an `Unreachable` placeholder. Silently overwriting a real terminator is
    /// how divergence bugs hide — e.g. a diverging let-initializer's `Return`
    /// being clobbered by the code after the `let` (RUE-128) — so that is a
    /// loud assertion that runs in release too (correctness guard, RUE-45).
    pub(crate) fn set_terminator(&mut self, block: BlockId, term: Terminator) {
        assert!(
            matches!(
                self.blocks[block.0 as usize].terminator,
                Terminator::None | Terminator::Unreachable
            ),
            "block {} already has terminator {:?}; refusing to overwrite with {:?}",
            block.0,
            self.blocks[block.0 as usize].terminator,
            term
        );
        self.blocks[block.0 as usize].terminator = term;
    }

    /// Terminate a block with an unconditional edge owned by this CFG.
    pub fn set_goto(
        &mut self,
        block: BlockId,
        target: BlockId,
        args: impl IntoIterator<Item = CfgValue>,
    ) {
        self.try_set_goto(block, target, args)
            .expect("valid CFG goto edit");
    }

    pub fn try_set_goto(
        &mut self,
        block: BlockId,
        target: BlockId,
        args: impl IntoIterator<Item = CfgValue>,
    ) -> Result<(), CfgEditError> {
        const OP: &str = "goto arguments";
        if block.0 as usize >= self.blocks.len() || target.0 as usize >= self.blocks.len() {
            return Err(Self::invalid_edit(OP, "block target is out of bounds"));
        }
        if !matches!(
            self.blocks[block.0 as usize].terminator,
            Terminator::None | Terminator::Unreachable
        ) {
            return Err(Self::invalid_edit(OP, "block already has a terminator"));
        }
        let staged = Self::stage_edit(OP, args)?;
        self.validate_value_refs(OP, staged.iter(), |value| *value)?;
        let args = payload::push_goto_args(&mut self.extra, staged)?;
        self.set_terminator(block, Terminator::Goto { target, args });
        Ok(())
    }

    /// Terminate a block with a conditional edge owned by this CFG.
    pub fn set_branch(
        &mut self,
        block: BlockId,
        cond: CfgValue,
        then_block: BlockId,
        then_args: impl IntoIterator<Item = CfgValue>,
        else_block: BlockId,
        else_args: impl IntoIterator<Item = CfgValue>,
    ) {
        self.try_set_branch(block, cond, then_block, then_args, else_block, else_args)
            .expect("valid CFG branch edit");
    }

    pub fn try_set_branch(
        &mut self,
        block: BlockId,
        cond: CfgValue,
        then_block: BlockId,
        then_args: impl IntoIterator<Item = CfgValue>,
        else_block: BlockId,
        else_args: impl IntoIterator<Item = CfgValue>,
    ) -> Result<(), CfgEditError> {
        const OP: &str = "branch arguments";
        if block.0 as usize >= self.blocks.len()
            || then_block.0 as usize >= self.blocks.len()
            || else_block.0 as usize >= self.blocks.len()
        {
            return Err(Self::invalid_edit(OP, "block target is out of bounds"));
        }
        if cond.0 as usize >= self.values.len() {
            return Err(Self::invalid_edit(OP, "condition value is undefined"));
        }
        if !matches!(
            self.blocks[block.0 as usize].terminator,
            Terminator::None | Terminator::Unreachable
        ) {
            return Err(Self::invalid_edit(OP, "block already has a terminator"));
        }
        let then_staged = Self::stage_edit(OP, then_args)?;
        let else_staged = Self::stage_edit(OP, else_args)?;
        self.validate_value_refs(OP, then_staged.iter(), |value| *value)?;
        self.validate_value_refs(OP, else_staged.iter(), |value| *value)?;
        let additional = then_staged
            .len()
            .checked_add(else_staged.len())
            .ok_or(CfgEditError::ResourceLimitExceeded { family: OP })?;
        payload::reserve_values(&mut self.extra, additional)?;
        let then_args = payload::push_then_args(&mut self.extra, then_staged)?;
        let else_args = payload::push_else_args(&mut self.extra, else_staged)?;
        self.set_terminator(
            block,
            Terminator::Branch {
                cond,
                then_block,
                then_args,
                else_block,
                else_args,
            },
        );
        Ok(())
    }

    /// Terminate a block with a switch owned by this CFG.
    pub fn set_switch(
        &mut self,
        block: BlockId,
        scrutinee: CfgValue,
        cases: impl IntoIterator<Item = (i64, BlockId)>,
        default: BlockId,
    ) {
        self.try_set_switch(block, scrutinee, cases, default)
            .expect("valid CFG switch edit");
    }

    pub fn try_set_switch(
        &mut self,
        block: BlockId,
        scrutinee: CfgValue,
        cases: impl IntoIterator<Item = (i64, BlockId)>,
        default: BlockId,
    ) -> Result<(), CfgEditError> {
        const OP: &str = "switch cases";
        if block.0 as usize >= self.blocks.len() || default.0 as usize >= self.blocks.len() {
            return Err(Self::invalid_edit(OP, "block target is out of bounds"));
        }
        if scrutinee.0 as usize >= self.values.len() {
            return Err(Self::invalid_edit(OP, "scrutinee value is undefined"));
        }
        if !matches!(
            self.blocks[block.0 as usize].terminator,
            Terminator::None | Terminator::Unreachable
        ) {
            return Err(Self::invalid_edit(OP, "block already has a terminator"));
        }
        let staged = Self::stage_edit(OP, cases)?;
        if staged
            .iter()
            .any(|(_, target)| target.0 as usize >= self.blocks.len())
        {
            return Err(Self::invalid_edit(OP, "case target is out of bounds"));
        }
        let cases = payload::push_switch_cases(&mut self.switch_cases, staged)?;
        self.set_terminator(
            block,
            Terminator::Switch {
                scrutinee,
                cases,
                default,
            },
        );
        Ok(())
    }

    /// Terminate a block by returning a value, or unit when `None`.
    pub fn set_return(&mut self, block: BlockId, value: Option<CfgValue>) {
        self.set_terminator(block, Terminator::Return { value });
    }

    /// Mark a block as unreachable.
    pub fn set_unreachable(&mut self, block: BlockId) {
        self.set_terminator(block, Terminator::Unreachable);
    }

    /// Get all blocks.
    pub fn blocks(&self) -> &[BasicBlock] {
        &self.blocks
    }

    /// Get the number of blocks.
    #[inline]
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Iterate over block IDs.
    pub fn block_ids(&self) -> impl Iterator<Item = BlockId> {
        (0..self.blocks.len() as u32).map(BlockId)
    }

    /// Rewrite every value USE in the CFG through `map`, covering all the
    /// places a `CfgValue` can be referenced: instruction operands, the
    /// `extra` array (struct fields, array elements, intrinsic args, and
    /// terminator block arguments), call arguments, place projection
    /// indices, and terminator operands (branch conditions, switch
    /// scrutinees, return values).
    ///
    /// Definitions are not touched: `blocks[..].params` entries and the
    /// mapped-away instructions themselves stay in the value arena as
    /// detached values for DCE to ignore.
    ///
    /// This is the canonical use-rewrite for optimization passes that
    /// substitute one value for another (e.g. block-merge parameter
    /// substitution, RUE-911). One call visits every use site exactly once.
    pub(crate) fn rewrite_value_uses(
        &mut self,
        map: impl Fn(CfgValue) -> CfgValue,
    ) -> Result<(), CfgEditError> {
        let mut rewritten = self.clone();
        rewritten.rewrite_value_uses_in_place(map)?;
        *self = rewritten;
        Ok(())
    }

    fn rewrite_value_uses_in_place(
        &mut self,
        map: impl Fn(CfgValue) -> CfgValue,
    ) -> Result<(), CfgEditError> {
        use CfgInstData::*;
        let old_extra = std::mem::replace(&mut self.extra, payload::Values::new());
        let old_call_args = std::mem::replace(&mut self.call_args, payload::CallArgs::new());
        let old_projections = std::mem::replace(&mut self.projections, payload::Projections::new());
        for inst in &mut self.values {
            match &mut inst.data {
                Add(a, b)
                | Sub(a, b)
                | Mul(a, b)
                | WrappingAdd(a, b)
                | WrappingSub(a, b)
                | WrappingMul(a, b)
                | Div(a, b)
                | Mod(a, b)
                | Eq(a, b)
                | Ne(a, b)
                | Lt(a, b)
                | Gt(a, b)
                | Le(a, b)
                | Ge(a, b)
                | BitAnd(a, b)
                | BitOr(a, b)
                | BitXor(a, b)
                | Shl(a, b)
                | Shr(a, b) => {
                    *a = map(*a);
                    *b = map(*b);
                }
                Neg(a) | Not(a) | BitNot(a) => *a = map(*a),
                Alloc { init: v, .. }
                | Store { value: v, .. }
                | ParamStore { value: v, .. }
                | EnumPayloadGet { base: v, .. }
                | IntCast { value: v, .. }
                | Drop { value: v } => *v = map(*v),
                PlaceRead { place } => {
                    if let PlaceBase::Accessor(value) = &mut place.base {
                        *value = map(*value);
                    }
                    place.projections = payload::push_projections(
                        &mut self.projections,
                        payload::projections(&old_projections, &place.projections)
                            .iter()
                            .map(|projection| match projection {
                                Projection::Field {
                                    struct_id,
                                    field_index,
                                } => Projection::Field {
                                    struct_id: *struct_id,
                                    field_index: *field_index,
                                },
                                Projection::Index { array_type, index } => Projection::Index {
                                    array_type: *array_type,
                                    index: map(*index),
                                },
                            }),
                    )?;
                }
                PlaceWrite { place, value } => {
                    *value = map(*value);
                    if let PlaceBase::Accessor(base) = &mut place.base {
                        *base = map(*base);
                    }
                    place.projections = payload::push_projections(
                        &mut self.projections,
                        payload::projections(&old_projections, &place.projections)
                            .iter()
                            .map(|projection| match projection {
                                Projection::Field {
                                    struct_id,
                                    field_index,
                                } => Projection::Field {
                                    struct_id: *struct_id,
                                    field_index: *field_index,
                                },
                                Projection::Index { array_type, index } => Projection::Index {
                                    array_type: *array_type,
                                    index: map(*index),
                                },
                            }),
                    )?;
                }
                Call { args, .. } | AccessorCall { args, .. } => {
                    *args = payload::push_call_args(
                        &mut self.call_args,
                        payload::call_args(&old_call_args, args)
                            .iter()
                            .map(|arg| CfgCallArg {
                                value: map(arg.value),
                                mode: arg.mode,
                            }),
                    )?;
                }
                Intrinsic { args, .. } => {
                    *args = payload::push_intrinsic_args(
                        &mut self.extra,
                        payload::intrinsic_args(&old_extra, args)
                            .iter()
                            .copied()
                            .map(&map),
                    )?;
                }
                StructInit { fields, .. } => {
                    *fields = payload::push_struct_fields(
                        &mut self.extra,
                        payload::struct_fields(&old_extra, fields)
                            .iter()
                            .copied()
                            .map(&map),
                    )?;
                }
                ArrayInit { elements } => {
                    *elements = payload::push_array_elements(
                        &mut self.extra,
                        payload::array_elements(&old_extra, elements)
                            .iter()
                            .copied()
                            .map(&map),
                    )?;
                }
                EnumVariant { payload: range, .. } => {
                    *range = payload::push_enum_payload(
                        &mut self.extra,
                        payload::enum_payload(&old_extra, range)
                            .iter()
                            .copied()
                            .map(&map),
                    )?;
                }
                Const(_)
                | BoolConst(_)
                | StringConst(_)
                | Param { .. }
                | BlockParam { .. }
                | Load { .. }
                | StorageLive { .. }
                | StorageDead { .. } => {}
            }
        }
        for block in &mut self.blocks {
            match &mut block.terminator {
                Terminator::Goto { args, .. } => {
                    *args = payload::push_goto_args(
                        &mut self.extra,
                        payload::goto_args(&old_extra, args)
                            .iter()
                            .copied()
                            .map(&map),
                    )?;
                }
                Terminator::Branch {
                    cond,
                    then_args,
                    else_args,
                    ..
                } => {
                    *cond = map(*cond);
                    *then_args = payload::push_then_args(
                        &mut self.extra,
                        payload::then_args(&old_extra, then_args)
                            .iter()
                            .copied()
                            .map(&map),
                    )?;
                    *else_args = payload::push_else_args(
                        &mut self.extra,
                        payload::else_args(&old_extra, else_args)
                            .iter()
                            .copied()
                            .map(&map),
                    )?;
                }
                Terminator::Switch { scrutinee, .. } => *scrutinee = map(*scrutinee),
                Terminator::Return { value: Some(value) } => *value = map(*value),
                Terminator::Return { value: None } | Terminator::Unreachable | Terminator::None => {
                }
            }
        }
        Ok(())
    }

    /// Compute predecessor lists for all blocks, indexed by block id.
    ///
    /// Predecessors are computed on demand from the current terminators
    /// (rather than cached on each block) so the result is always in sync
    /// with the CFG, even after optimization passes rewrite terminators.
    pub fn compute_predecessors(&self) -> Vec<Vec<BlockId>> {
        let mut preds: Vec<Vec<BlockId>> = vec![Vec::new(); self.blocks.len()];

        for block in &self.blocks {
            match &block.terminator {
                Terminator::Goto { target, .. } => {
                    preds[target.0 as usize].push(block.id);
                }
                Terminator::Branch {
                    then_block,
                    else_block,
                    ..
                } => {
                    preds[then_block.0 as usize].push(block.id);
                    preds[else_block.0 as usize].push(block.id);
                }
                Terminator::Switch { cases, default, .. } => {
                    for (_, target) in self.switch_cases(cases) {
                        preds[target.0 as usize].push(block.id);
                    }
                    preds[default.0 as usize].push(block.id);
                }
                Terminator::Return { .. } | Terminator::Unreachable | Terminator::None => {}
            }
        }

        preds
    }
}

/// Interner-aware CFG display adapter for stable, human-readable symbols.
pub struct CfgDisplay<'a> {
    cfg: &'a Cfg,
    interner: &'a ThreadedRodeo,
}

impl Cfg {
    /// Display this CFG with call and intrinsic symbols resolved to names.
    pub fn display_with_interner<'a>(&'a self, interner: &'a ThreadedRodeo) -> CfgDisplay<'a> {
        CfgDisplay {
            cfg: self,
            interner,
        }
    }

    fn fmt_with_interner(
        &self,
        f: &mut fmt::Formatter<'_>,
        interner: Option<&ThreadedRodeo>,
    ) -> fmt::Result {
        writeln!(
            f,
            "cfg {} (return_type: {}) {{",
            self.fn_name,
            self.return_type.name()
        )?;
        let all_preds = self.compute_predecessors();
        for block in &self.blocks {
            write!(f, "  {}:", block.id)?;
            if !block.params.is_empty() {
                write!(f, "(")?;
                for (i, (val, ty)) in block.params.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: {}", val, ty.name())?;
                }
                write!(f, ")")?;
            }
            writeln!(f)?;

            // Print predecessors
            let preds = &all_preds[block.id.0 as usize];
            if !preds.is_empty() {
                write!(f, "    ; preds: ")?;
                for (i, pred) in preds.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", pred)?;
                }
                writeln!(f)?;
            }

            // Print instructions
            for &val in &block.insts {
                let inst = self.get_inst(val);
                write!(f, "    {} : {} = ", val, inst.ty.name())?;
                self.fmt_inst_data(f, &inst.data, interner)?;
                writeln!(f)?;
            }

            // Print terminator
            write!(f, "    ")?;
            match &block.terminator {
                Terminator::Goto { target, args: _ } => {
                    write!(f, "goto {}", target)?;
                    let values = self.get_goto_args(&block.terminator);
                    if !values.is_empty() {
                        write!(f, "(")?;
                        for (i, arg) in values.iter().enumerate() {
                            if i > 0 {
                                write!(f, ", ")?;
                            }
                            write!(f, "{}", arg)?;
                        }
                        write!(f, ")")?;
                    }
                }
                Terminator::Branch {
                    cond,
                    then_block,
                    then_args: _,
                    else_block,
                    else_args: _,
                } => {
                    write!(f, "branch {}, {}", cond, then_block)?;
                    let then_args = self.get_branch_then_args(&block.terminator);
                    if !then_args.is_empty() {
                        write!(f, "(")?;
                        for (i, arg) in then_args.iter().enumerate() {
                            if i > 0 {
                                write!(f, ", ")?;
                            }
                            write!(f, "{}", arg)?;
                        }
                        write!(f, ")")?;
                    }
                    write!(f, ", {}", else_block)?;
                    let else_args = self.get_branch_else_args(&block.terminator);
                    if !else_args.is_empty() {
                        write!(f, "(")?;
                        for (i, arg) in else_args.iter().enumerate() {
                            if i > 0 {
                                write!(f, ", ")?;
                            }
                            write!(f, "{}", arg)?;
                        }
                        write!(f, ")")?;
                    }
                }
                Terminator::Switch {
                    scrutinee,
                    cases,
                    default,
                } => {
                    write!(f, "switch {} [", scrutinee)?;
                    let cases_view = self.switch_cases(cases);
                    for (i, (val, target)) in cases_view.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{} => {}", val, target)?;
                    }
                    write!(f, "], default: {}", default)?;
                }
                Terminator::Return { value } => {
                    if let Some(value) = value {
                        write!(f, "return {}", value)?;
                    } else {
                        write!(f, "return")?;
                    }
                }
                Terminator::Unreachable => {
                    write!(f, "unreachable")?;
                }
                Terminator::None => {
                    write!(f, "<no terminator>")?;
                }
            }
            writeln!(f)?;
            writeln!(f)?;
        }
        writeln!(f, "}}")
    }
}

impl fmt::Display for CfgDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.cfg.fmt_with_interner(f, Some(self.interner))
    }
}

impl fmt::Display for Cfg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.fmt_with_interner(f, None)
    }
}

impl Cfg {
    fn fmt_inst_data(
        &self,
        f: &mut fmt::Formatter<'_>,
        data: &CfgInstData,
        interner: Option<&ThreadedRodeo>,
    ) -> fmt::Result {
        match data {
            CfgInstData::Const(v) => write!(f, "const {}", v),
            CfgInstData::BoolConst(v) => write!(f, "const {}", v),
            CfgInstData::StringConst(idx) => write!(f, "string_const @{}", idx),
            CfgInstData::Param { index } => write!(f, "param {}", index),
            CfgInstData::BlockParam { index } => write!(f, "block_param {}", index),
            CfgInstData::Add(lhs, rhs) => write!(f, "add {}, {}", lhs, rhs),
            CfgInstData::Sub(lhs, rhs) => write!(f, "sub {}, {}", lhs, rhs),
            CfgInstData::Mul(lhs, rhs) => write!(f, "mul {}, {}", lhs, rhs),
            CfgInstData::WrappingAdd(lhs, rhs) => write!(f, "wrapping_add {}, {}", lhs, rhs),
            CfgInstData::WrappingSub(lhs, rhs) => write!(f, "wrapping_sub {}, {}", lhs, rhs),
            CfgInstData::WrappingMul(lhs, rhs) => write!(f, "wrapping_mul {}, {}", lhs, rhs),
            CfgInstData::Div(lhs, rhs) => write!(f, "div {}, {}", lhs, rhs),
            CfgInstData::Mod(lhs, rhs) => write!(f, "mod {}, {}", lhs, rhs),
            CfgInstData::Eq(lhs, rhs) => write!(f, "eq {}, {}", lhs, rhs),
            CfgInstData::Ne(lhs, rhs) => write!(f, "ne {}, {}", lhs, rhs),
            CfgInstData::Lt(lhs, rhs) => write!(f, "lt {}, {}", lhs, rhs),
            CfgInstData::Gt(lhs, rhs) => write!(f, "gt {}, {}", lhs, rhs),
            CfgInstData::Le(lhs, rhs) => write!(f, "le {}, {}", lhs, rhs),
            CfgInstData::Ge(lhs, rhs) => write!(f, "ge {}, {}", lhs, rhs),
            CfgInstData::BitAnd(lhs, rhs) => write!(f, "bit_and {}, {}", lhs, rhs),
            CfgInstData::BitOr(lhs, rhs) => write!(f, "bit_or {}, {}", lhs, rhs),
            CfgInstData::BitXor(lhs, rhs) => write!(f, "bit_xor {}, {}", lhs, rhs),
            CfgInstData::Shl(lhs, rhs) => write!(f, "shl {}, {}", lhs, rhs),
            CfgInstData::Shr(lhs, rhs) => write!(f, "shr {}, {}", lhs, rhs),
            CfgInstData::Neg(v) => write!(f, "neg {}", v),
            CfgInstData::Not(v) => write!(f, "not {}", v),
            CfgInstData::BitNot(v) => write!(f, "bit_not {}", v),
            CfgInstData::Alloc { slot, init } => write!(f, "alloc ${} = {}", slot, init),
            CfgInstData::Load { slot } => write!(f, "load ${}", slot),
            CfgInstData::Store { slot, value } => write!(f, "store ${} = {}", slot, value),
            CfgInstData::ParamStore { param_slot, value } => {
                write!(f, "param_store %{} = {}", param_slot, value)
            }
            CfgInstData::PlaceRead { place } => {
                write!(f, "place_read ")?;
                self.fmt_place(f, place)
            }
            CfgInstData::PlaceWrite { place, value } => {
                write!(f, "place_write ")?;
                self.fmt_place(f, place)?;
                write!(f, " = {}", value)
            }
            CfgInstData::Call {
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
                for (i, arg) in self.call_args(args).iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    match arg.mode {
                        CfgArgMode::Inout => write!(f, "inout {}", arg.value)?,
                        CfgArgMode::Borrow => write!(f, "borrow {}", arg.value)?,
                        CfgArgMode::Normal => write!(f, "{}", arg.value)?,
                    }
                }
                write!(f, ")")
            }
            CfgInstData::AccessorCall { name, args } => {
                match interner {
                    Some(interner) => write!(f, "accessor_call @{}(", interner.resolve(name))?,
                    None => write!(f, "accessor_call @{}(", name.into_usize())?,
                }
                for (i, arg) in self.call_args(args).iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    match arg.mode {
                        CfgArgMode::Inout => write!(f, "inout {}", arg.value)?,
                        CfgArgMode::Borrow => write!(f, "borrow {}", arg.value)?,
                        CfgArgMode::Normal => write!(f, "{}", arg.value)?,
                    }
                }
                write!(f, ")")
            }
            CfgInstData::Intrinsic {
                runtime,
                name,
                args,
            } => {
                if let Some(runtime) = runtime {
                    write!(f, "runtime.{runtime:?} ")?;
                }
                match interner {
                    Some(interner) => write!(f, "intrinsic @{}(", interner.resolve(name))?,
                    None => write!(f, "intrinsic @{}(", name.into_usize())?,
                }
                for (i, arg) in self.intrinsic_args(args).iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", arg)?;
                }
                write!(f, ")")
            }
            CfgInstData::StructInit { struct_id, fields } => {
                write!(f, "struct_init #{struct_id:?} {{")?;
                for (i, field) in self.struct_fields(fields).iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", field)?;
                }
                write!(f, "}}")
            }
            CfgInstData::ArrayInit { elements } => {
                write!(f, "array_init [")?;
                for (i, elem) in self.array_elements(elements).iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", elem)?;
                }
                write!(f, "]")
            }
            CfgInstData::EnumVariant {
                enum_id,
                variant_index,
                payload,
            } => {
                if payload.is_empty() {
                    write!(f, "enum_variant #{enum_id:?}::{variant_index}")
                } else {
                    // Print the actual payload operands (like StructInit and
                    // ArrayInit do) so the variant's dataflow inputs are
                    // readable in the dump, not just their count.
                    write!(f, "enum_variant #{enum_id:?}::{variant_index}(")?;
                    for (i, value) in self.enum_payload(payload).iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", value)?;
                    }
                    write!(f, ")")
                }
            }
            CfgInstData::EnumPayloadGet {
                base,
                enum_id,
                variant_index,
                field_index,
            } => {
                write!(
                    f,
                    "enum_payload_get {} #{:?}::{}.{}",
                    base, enum_id, variant_index, field_index
                )
            }
            CfgInstData::IntCast { value, from_ty } => {
                write!(f, "intcast {} from {}", value, from_ty.name())
            }
            CfgInstData::Drop { value } => {
                write!(f, "drop {}", value)
            }
            CfgInstData::StorageLive { slot, .. } => {
                write!(f, "storage_live ${}", slot)
            }
            CfgInstData::StorageDead { slot, .. } => {
                write!(f, "storage_dead ${}", slot)
            }
        }
    }

    /// Format a place for display, showing the base and projections.
    fn fmt_place(&self, f: &mut fmt::Formatter<'_>, place: &Place) -> fmt::Result {
        write!(f, "{}", self.place_to_string(place))
    }

    /// Render a place with its projections RESOLVED (e.g.
    /// `$0.#StructId(2).1($arr)[v3]`),
    /// unlike `Place`'s own `Display`, which cannot see this Cfg's projection
    /// arena and can only print the raw index range (`$0[3..5]`). Use this for
    /// any human-facing dump or tracing description of a place.
    pub fn place_to_string(&self, place: &Place) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        match place.base {
            PlaceBase::Local(slot) => {
                let _ = write!(out, "${}", slot);
            }
            PlaceBase::Param(slot) => {
                let _ = write!(out, "param%{}", slot);
            }
            PlaceBase::Accessor(value) => {
                let _ = write!(out, "accessor%{}", value.as_u32());
            }
        }
        for proj in self.get_place_projections(place) {
            match proj {
                Projection::Field {
                    struct_id,
                    field_index,
                } => {
                    let _ = write!(out, ".#{struct_id:?}.{field_index}");
                }
                Projection::Index { array_type, index } => {
                    let _ = write!(out, "({})[{}]", array_type.name(), index);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn cfg_payload_limit_message_names_the_published_ceiling() {
        // RUE-1221 / spec C.1:2: exceeding a published implementation limit is
        // a diagnosable failure that names the limit, not an ICE.
        let error = CfgEditError::ResourceLimitExceeded {
            family: "call args",
        };
        assert_eq!(
            error.to_string(),
            "CFG call args exceeded the implementation limit of 4294967295 payload words per \
             program (spec Appendix C.6:1)"
        );
        assert_eq!(
            error.error_kind("CFG payload construction failed").code(),
            rue_error::ErrorCode::COMPILER_RESOURCE_LIMIT
        );
    }

    #[test]
    fn cfg_edit_failures_are_classified_apart_from_internal_errors() {
        assert_eq!(
            CfgEditError::CapacityFailure { family: "f" }
                .error_kind("ctx")
                .code(),
            rue_error::ErrorCode::COMPILER_RESOURCE_EXHAUSTION
        );
        assert_eq!(
            CfgEditError::InvalidBuilderInput {
                operation: "append_call",
                detail: "unknown value",
            }
            .error_kind("ctx")
            .code(),
            rue_error::ErrorCode::INTERNAL_ERROR
        );
    }

    #[test]
    fn cfg_owner_limit_message_names_the_published_per_function_ceiling() {
        // RUE-1226 / spec C.1:2: the per-function block and value arenas are a
        // separate ceiling from the per-program payload word store, and its
        // diagnostic has to name the one actually exceeded.
        let error = CfgEditError::OwnerLimitExceeded {
            family: "basic blocks",
        };
        assert_eq!(
            error.to_string(),
            "this function's CFG has more basic blocks than the implementation limit of \
             4294967295 per function (spec Appendix C.6:1)"
        );
        assert_eq!(
            error.error_kind("CFG construction failed").code(),
            rue_error::ErrorCode::COMPILER_RESOURCE_LIMIT
        );
        assert_eq!(MAX_CFG_ENTITIES_PER_FUNCTION, u32::MAX);
    }

    #[test]
    fn owner_index_allocation_stops_at_the_published_ceiling() {
        assert_eq!(checked_owner_index(0), Some(0));
        assert_eq!(
            checked_owner_index(MAX_CFG_ENTITIES_PER_FUNCTION as usize - 1),
            Some(MAX_CFG_ENTITIES_PER_FUNCTION - 1)
        );
        // The final u32 identity is not allocated: `len() as u32` would then
        // wrap the next entity onto identity 0.
        assert_eq!(
            checked_owner_index(MAX_CFG_ENTITIES_PER_FUNCTION as usize),
            None
        );
    }

    #[test]
    fn an_ordinary_graph_never_latches_the_per_function_ceiling() {
        let mut cfg = Cfg::new(Type::I32, 0, 0, "f".into(), Vec::<bool>::new());
        let block = cfg.new_block();
        assert_eq!(block, BlockId(0));
        assert!(cfg.new_block() != block);
        cfg.add_inst(CfgInst {
            data: CfgInstData::Const(7),
            ty: Type::I32,
            span: Span::new(0, 1),
        });
        assert!(cfg.latched_capacity_error().is_none());
    }

    #[test]
    fn a_latched_graph_reports_the_family_that_ran_out() {
        // Reaching the ceiling needs 2^32 entities, so the latch itself is set
        // here; `new_block`/`add_inst` set it the same way and then hand back an
        // existing identity rather than a wrapped one.
        let mut cfg = Cfg::new(Type::I32, 0, 0, "f".into(), Vec::<bool>::new());
        cfg.new_block();
        cfg.capacity_exceeded = Some("values");
        let error = cfg.latched_capacity_error().expect("latched");
        assert!(matches!(
            error,
            CfgEditError::OwnerLimitExceeded { family: "values" }
        ));
        assert_eq!(
            error.error_kind("CFG construction failed").code(),
            rue_error::ErrorCode::COMPILER_RESOURCE_LIMIT
        );
        // The latch survives the clone every optimization transaction takes.
        assert!(cfg.clone().latched_capacity_error().is_some());
    }

    #[test]
    fn test_block_id_size() {
        assert_eq!(std::mem::size_of::<BlockId>(), 4);
    }

    #[test]
    fn test_cfg_value_size() {
        assert_eq!(std::mem::size_of::<CfgValue>(), 4);
    }

    #[test]
    fn test_cfg_inst_size() {
        // Document actual sizes for future reference.
        // If this test fails, update the const assertions at the top of this file.
        let cfg_inst_size = std::mem::size_of::<CfgInst>();
        let cfg_inst_data_size = std::mem::size_of::<CfgInstData>();

        // These assertions document the current sizes.
        // If the layout changes, update both these values and the const assertions.
        assert!(
            cfg_inst_size <= 48,
            "CfgInst grew beyond 48 bytes: {}",
            cfg_inst_size
        );
        assert!(
            cfg_inst_data_size <= 32,
            "CfgInstData grew beyond 32 bytes: {}",
            cfg_inst_data_size
        );
    }

    #[test]
    fn test_terminator_size() {
        // Terminator should be a reasonable size (no heap allocations inside)
        // 32 bytes: 8 (CfgValue cond) + 4+4+4+4 (BlockId, start, len x2) + 4+4+4 (else) = 36, rounded to 40
        // Actually: Branch is the largest with cond(4) + then_block(4) + then_start(4) + then_len(4) + else_block(4) + else_start(4) + else_len(4) = 28 bytes + discriminant
        let size = std::mem::size_of::<Terminator>();
        assert!(size <= 40, "Terminator is {} bytes, expected <= 40", size);
    }

    #[test]
    fn test_create_cfg() {
        let mut cfg = Cfg::new(Type::I32, 0, 0, "test".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;

        let const_val = cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Const(42),
                ty: Type::I32,
                span: Span::new(0, 2),
            },
        );

        cfg.set_terminator(
            entry,
            Terminator::Return {
                value: Some(const_val),
            },
        );

        assert_eq!(cfg.block_count(), 1);
    }

    #[test]
    fn interner_aware_display_resolves_call_symbols() {
        let interner = ThreadedRodeo::new();
        let call = interner.get_or_intern("callee");
        let intrinsic = interner.get_or_intern("assert");
        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "test".to_string(), vec![]);
        let entry = cfg.new_block();
        cfg.entry = entry;

        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Call {
                    runtime: None,
                    name: call,
                    args: crate::payload::CfgCallArgs::EMPTY,
                },
                ty: Type::UNIT,
                span: Span::new(0, 0),
            },
        );
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Intrinsic {
                    runtime: None,
                    name: intrinsic,
                    args: crate::payload::CfgIntrinsicArgs::EMPTY,
                },
                ty: Type::UNIT,
                span: Span::new(0, 0),
            },
        );
        cfg.set_terminator(entry, Terminator::Return { value: None });

        let resolved = cfg.display_with_interner(&interner).to_string();
        assert!(resolved.contains("call @callee()"));
        assert!(resolved.contains("intrinsic @assert()"));

        // The context-free Display API remains backward compatible.
        let raw = cfg.to_string();
        assert!(raw.contains(&format!("call @{}()", call.into_usize())));
        assert!(raw.contains(&format!("intrinsic @{}()", intrinsic.into_usize())));
    }

    #[test]
    fn fallible_owner_edits_reject_invalid_input_atomically() {
        struct ImpossiblePayload;
        impl Iterator for ImpossiblePayload {
            type Item = CfgCallArg;

            fn next(&mut self) -> Option<Self::Item> {
                None
            }

            fn size_hint(&self) -> (usize, Option<usize>) {
                let len = u32::MAX as usize + 1;
                (len, Some(len))
            }
        }
        impl ExactSizeIterator for ImpossiblePayload {
            fn len(&self) -> usize {
                u32::MAX as usize + 1
            }
        }

        let mut cfg = Cfg::new(Type::UNIT, 0, 0, "atomic".into(), vec![]);
        let entry = cfg.new_block();
        let target = cfg.new_block();
        cfg.entry = entry;
        let before = cfg.to_string();
        let before_values = cfg.value_count();

        let call_error = cfg
            .append_call(
                entry,
                None,
                Spur::default(),
                [CfgCallArg {
                    value: CfgValue::from_raw(99),
                    mode: CfgArgMode::Normal,
                }],
                Type::UNIT,
                Span::default(),
            )
            .unwrap_err();
        assert!(matches!(
            call_error,
            CfgEditError::InvalidBuilderInput { .. }
        ));
        assert_eq!(cfg.value_count(), before_values);
        assert_eq!(cfg.to_string(), before);

        let resource_error = cfg.push_call_args(ImpossiblePayload).unwrap_err();
        assert!(matches!(
            resource_error,
            CfgEditError::ResourceLimitExceeded { .. }
        ));
        assert_eq!(cfg.to_string(), before);

        let branch_error = cfg
            .try_set_branch(entry, CfgValue::from_raw(99), target, [], target, [])
            .unwrap_err();
        assert!(matches!(
            branch_error,
            CfgEditError::InvalidBuilderInput { .. }
        ));
        assert_eq!(cfg.to_string(), before);
    }

    #[test]
    fn whole_owner_clone_remap_rewrite_and_print_preserve_typed_payloads() {
        use rue_air::{EnumDef, FrozenTypeInternPool, StructDef, StructField, TypeInternPool};
        use rue_span::FileId;

        let pool = TypeInternPool::new();
        let interner = ThreadedRodeo::new();
        let (struct_id, _) = pool.register_struct(
            interner.get_or_intern("Pair"),
            StructDef {
                name: "Pair".into(),
                fields: vec![
                    StructField {
                        name: "a".into(),
                        ty: Type::I32,
                    },
                    StructField {
                        name: "b".into(),
                        ty: Type::I32,
                    },
                ],
                is_copy: true,
                is_linear: false,
                destructor: None,
                is_builtin: false,
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        let (enum_id, _) = pool.register_enum(
            interner.get_or_intern("Maybe"),
            EnumDef {
                name: "Maybe".into(),
                variants: Arc::from(["None".into(), "Some".into()]),
                variant_payloads: vec![vec![], vec![Type::I32]],
                is_pub: false,
                file_id: FileId::DEFAULT,
            },
        );
        let array_id = pool.intern_array_from_type(Type::I32, 2);
        let pool: FrozenTypeInternPool = pool.freeze();
        let array_ty = Type::new_array(array_id);
        let mut cfg = Cfg::new(Type::I32, 2, 0, "payloads".into(), vec![]);
        let entry = cfg.new_block();
        let then_block = cfg.new_block();
        let else_block = cfg.new_block();
        let exit = cfg.new_block();
        cfg.entry = entry;
        let old = cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Const(1),
                ty: Type::I32,
                span: Span::new(3, 4),
            },
        );
        let replacement = cfg.append_inst(
            entry,
            CfgInst {
                data: CfgInstData::Const(2),
                ty: Type::I32,
                span: Span::new(5, 6),
            },
        );
        let call = cfg
            .append_call(
                entry,
                None,
                interner.get_or_intern("callee"),
                [CfgCallArg {
                    value: old,
                    mode: CfgArgMode::Borrow,
                }],
                Type::I32,
                Span::new(7, 8),
            )
            .unwrap();
        let intrinsic = cfg
            .append_intrinsic(
                entry,
                None,
                interner.get_or_intern("intrinsic"),
                [old],
                Type::I32,
                Span::new(9, 10),
            )
            .unwrap();
        let strukt = cfg
            .append_struct_init(
                entry,
                struct_id,
                [old, replacement],
                Type::new_struct(struct_id),
                Span::new(11, 12),
            )
            .unwrap();
        let array = cfg
            .append_array_init(entry, [old, replacement], array_ty, Span::new(13, 14))
            .unwrap();
        let enm = cfg
            .append_enum_variant(
                &pool,
                entry,
                enum_id,
                1,
                [old],
                Type::new_enum(enum_id),
                Span::new(15, 16),
            )
            .unwrap();
        let place = cfg
            .append_place_read(
                entry,
                PlaceBase::Local(0),
                array_ty,
                [Projection::Index {
                    array_type: array_ty,
                    index: old,
                }],
                Type::I32,
                Span::new(17, 18),
            )
            .unwrap();
        cfg.set_branch(entry, old, then_block, [old], else_block, [replacement]);
        cfg.set_goto(then_block, exit, [old]);
        cfg.set_switch(else_block, old, [(1, exit)], exit);
        cfg.set_return(exit, Some(replacement));

        let printed = cfg.display_with_interner(&interner).to_string();
        for needle in [
            "call @callee",
            "intrinsic @intrinsic",
            "struct_init",
            "array_init",
            "enum_variant",
            "place_read",
            "branch",
            "goto",
            "switch",
        ] {
            assert!(printed.contains(needle), "missing {needle} in {printed}");
        }

        let cloned = cfg.clone();
        assert_eq!(cloned.to_string(), cfg.to_string());
        assert_eq!(
            cloned.get_call_args(&cloned.get_inst(call).data)[0].mode,
            CfgArgMode::Borrow
        );
        assert_eq!(
            cloned.get_struct_fields(&cloned.get_inst(strukt).data),
            [old, replacement]
        );

        let remapped = cfg
            .try_remap_domains::<()>(Ok, Ok, Ok, Ok, Ok, |span| {
                Ok(Span::new(span.start + 100, span.end + 100))
            })
            .unwrap();
        assert_eq!(remapped.to_string(), cfg.to_string());
        assert_eq!(remapped.get_inst(call).span, Span::new(107, 108));
        assert_eq!(
            remapped.get_array_elements(&remapped.get_inst(array).data),
            [old, replacement]
        );
        assert_eq!(
            remapped.get_enum_payload(&remapped.get_inst(enm).data),
            [old]
        );

        let mut rewritten = remapped;
        rewritten
            .rewrite_value_uses(|value| if value == old { replacement } else { value })
            .unwrap();
        assert_eq!(
            rewritten.get_call_args(&rewritten.get_inst(call).data)[0].value,
            replacement
        );
        assert_eq!(
            rewritten.get_call_args(&rewritten.get_inst(call).data)[0].mode,
            CfgArgMode::Borrow
        );
        assert_eq!(
            rewritten.get_intrinsic_args(&rewritten.get_inst(intrinsic).data),
            [replacement]
        );
        assert_eq!(
            rewritten.get_struct_fields(&rewritten.get_inst(strukt).data),
            [replacement, replacement]
        );
        assert_eq!(
            rewritten.get_array_elements(&rewritten.get_inst(array).data),
            [replacement, replacement]
        );
        assert_eq!(
            rewritten.get_enum_payload(&rewritten.get_inst(enm).data),
            [replacement]
        );
        let CfgInstData::PlaceRead {
            place: rewritten_place,
        } = &rewritten.get_inst(place).data
        else {
            unreachable!()
        };
        assert!(matches!(
            rewritten.get_place_projections(rewritten_place),
            [Projection::Index { index, .. }] if *index == replacement
        ));
        assert!(rewritten.to_string().contains("branch v1"));

        let before_rejections = rewritten.to_string();
        for error in [
            rewritten
                .replace_struct_fields(old, [replacement])
                .unwrap_err(),
            rewritten
                .replace_array_elements(old, [replacement])
                .unwrap_err(),
            rewritten
                .replace_enum_payload(&pool, old, [replacement])
                .unwrap_err(),
            rewritten
                .replace_place_read(old, PlaceBase::Local(0), array_ty, [])
                .unwrap_err(),
            rewritten
                .replace_place_write(old, PlaceBase::Local(0), array_ty, [], replacement)
                .unwrap_err(),
        ] {
            assert!(matches!(error, CfgEditError::InvalidBuilderInput { .. }));
            assert_eq!(rewritten.to_string(), before_rejections);
        }

        rewritten
            .replace_struct_fields(strukt, [replacement, replacement])
            .unwrap();
        rewritten
            .replace_array_elements(array, [replacement, replacement])
            .unwrap();
        rewritten
            .replace_enum_payload(&pool, enm, [replacement])
            .unwrap();
        rewritten
            .replace_place_read(
                place,
                PlaceBase::Local(0),
                array_ty,
                [Projection::Index {
                    array_type: array_ty,
                    index: replacement,
                }],
            )
            .unwrap();
    }
}
