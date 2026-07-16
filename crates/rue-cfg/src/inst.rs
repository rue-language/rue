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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    pub proj_start: u32,
    /// Number of projections
    pub proj_len: u32,
}

impl Place {
    /// Create a place for a local variable with no projections.
    #[inline]
    pub const fn local(slot: u32, base_type: Type) -> Self {
        Self {
            base: PlaceBase::Local(slot),
            base_type,
            proj_start: 0,
            proj_len: 0,
        }
    }

    /// Create a place for a parameter with no projections.
    #[inline]
    pub const fn param(slot: u32, base_type: Type) -> Self {
        Self {
            base: PlaceBase::Param(slot),
            base_type,
            proj_start: 0,
            proj_len: 0,
        }
    }

    /// Returns the local slot if this is a simple local place with no projections.
    #[inline]
    pub const fn as_local(&self) -> Option<u32> {
        if self.proj_len == 0 {
            match self.base {
                PlaceBase::Local(slot) => Some(slot),
                PlaceBase::Param(_) => None,
            }
        } else {
            None
        }
    }

    /// Returns the param slot if this is a simple param place with no projections.
    #[inline]
    pub const fn as_param(&self) -> Option<u32> {
        if self.proj_len == 0 {
            match self.base {
                PlaceBase::Param(slot) => Some(slot),
                PlaceBase::Local(_) => None,
            }
        } else {
            None
        }
    }
}

impl fmt::Display for Place {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.base {
            PlaceBase::Local(slot) => write!(f, "${}", slot)?,
            PlaceBase::Param(slot) => write!(f, "%{}", slot)?,
        }
        if self.proj_len > 0 {
            write!(
                f,
                "[{}..{}]",
                self.proj_start,
                self.proj_start + self.proj_len
            )?;
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
}

/// A projection applied to a place to reach a nested location.
///
/// Projections are stored in `Cfg::projections` and referenced by
/// `Place::proj_start` and `Place::proj_len`.
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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
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
    /// Function call. Arguments are stored in the Cfg's call_args array.
    /// Use `Cfg::get_call_args(args_start, args_len)` to retrieve them.
    Call {
        /// Function name (interned symbol)
        name: Spur,
        /// Start index into Cfg's call_args array
        args_start: u32,
        /// Number of arguments
        args_len: u32,
    },

    /// Intrinsic call (e.g., @dbg). Arguments are stored in the Cfg's extra array.
    /// Use `Cfg::get_extra(args_start, args_len)` to retrieve them.
    Intrinsic {
        /// Intrinsic name (interned symbol)
        name: Spur,
        /// Start index into Cfg's extra array
        args_start: u32,
        /// Number of arguments
        args_len: u32,
    },

    // Struct operations
    /// Struct initialization. Field values are stored in the Cfg's extra array.
    /// Use `Cfg::get_extra(fields_start, fields_len)` to retrieve them.
    StructInit {
        struct_id: StructId,
        /// Start index into Cfg's extra array
        fields_start: u32,
        /// Number of fields
        fields_len: u32,
    },
    // Array operations
    /// Array initialization. Element values are stored in the Cfg's extra array.
    /// Use `Cfg::get_extra(elements_start, elements_len)` to retrieve them.
    /// The array type is stored in `CfgInst.ty`.
    ArrayInit {
        /// Start index into Cfg's extra array
        elements_start: u32,
        /// Number of elements
        elements_len: u32,
    },
    // Enum operations
    /// Create an enum variant value: the discriminant plus (for tuple
    /// variants, RUE-221) the payload operands, stored in the Cfg's extra
    /// array. Use `Cfg::get_extra(payload_start, payload_len)` to retrieve the
    /// payload values (empty for a discriminant-only variant).
    EnumVariant {
        enum_id: EnumId,
        variant_index: u32,
        /// Start index into the Cfg's extra array for payload values.
        payload_start: u32,
        /// Number of payload values.
        payload_len: u32,
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
#[derive(Debug, Clone, Copy)]
pub enum Terminator {
    /// Unconditional jump to another block.
    /// Arguments are stored in Cfg's extra array.
    Goto {
        target: BlockId,
        /// Start index into Cfg's extra array
        args_start: u32,
        /// Number of arguments
        args_len: u32,
    },

    /// Conditional branch.
    /// Arguments for each branch are stored in Cfg's extra array.
    Branch {
        cond: CfgValue,
        then_block: BlockId,
        /// Start index into Cfg's extra array for then branch args
        then_args_start: u32,
        /// Number of arguments for then branch
        then_args_len: u32,
        else_block: BlockId,
        /// Start index into Cfg's extra array for else branch args
        else_args_start: u32,
        /// Number of arguments for else branch
        else_args_len: u32,
    },

    /// Multi-way branch (switch/match).
    /// Cases are stored in Cfg's switch_cases array.
    Switch {
        /// The value to switch on
        scrutinee: CfgValue,
        /// Start index into Cfg's switch_cases array
        cases_start: u32,
        /// Number of cases
        cases_len: u32,
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

/// A basic block in the CFG.
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
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
    extra: Vec<CfgValue>,
    /// Extra storage for call arguments (CfgCallArg).
    /// Call instructions store (start, len) indices into this array.
    call_args: Vec<CfgCallArg>,
    /// Extra storage for switch cases (value, target block pairs).
    /// Switch terminators store (start, len) indices into this array.
    switch_cases: Vec<(i64, BlockId)>,
    /// Extra storage for place projections.
    /// Place instructions store (start, len) indices into this array.
    projections: Vec<Projection>,
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
    ) -> Result<Self, E> {
        let mut cfg = self.clone();
        cfg.return_type = ty(cfg.return_type)?;
        for block in &mut cfg.blocks {
            for (_, value_ty) in &mut block.params {
                *value_ty = ty(*value_ty)?;
            }
        }
        for projection in &mut cfg.projections {
            match projection {
                Projection::Field { struct_id, .. } => *struct_id = strukt(*struct_id)?,
                Projection::Index { array_type, .. } => *array_type = ty(*array_type)?,
            }
        }
        for inst in &mut cfg.values {
            inst.ty = ty(inst.ty)?;
            inst.span = span(inst.span)?;
            match &mut inst.data {
                CfgInstData::StringConst(index) => *index = string(*index)?,
                CfgInstData::Call { name, .. } | CfgInstData::Intrinsic { name, .. } => {
                    *name = symbol(*name)?
                }
                CfgInstData::StructInit { struct_id, .. } => *struct_id = strukt(*struct_id)?,
                CfgInstData::EnumVariant { enum_id, .. }
                | CfgInstData::EnumPayloadGet { enum_id, .. } => *enum_id = enm(*enum_id)?,
                CfgInstData::IntCast { from_ty, .. } => *from_ty = ty(*from_ty)?,
                CfgInstData::StorageLive { local_ty, .. }
                | CfgInstData::StorageDead { local_ty, .. } => *local_ty = ty(*local_ty)?,
                _ => {}
            }
            if let CfgInstData::PlaceRead { place } | CfgInstData::PlaceWrite { place, .. } =
                &mut inst.data
            {
                place.base_type = ty(place.base_type)?;
            }
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
            extra: Vec::new(),
            call_args: Vec::new(),
            switch_cases: Vec::new(),
            projections: Vec::new(),
            num_locals,
            num_params,
            fn_name,
            param_modes: param_modes.into(),
            address_taken_slots: std::collections::HashSet::new(),
        }
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

    /// Get whether a parameter ABI slot is logically writable (`inout`).
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

    /// Create a new basic block and return its ID.
    pub fn new_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(BasicBlock::new(id));
        id
    }

    /// Get a block by ID.
    #[inline]
    pub fn get_block(&self, id: BlockId) -> &BasicBlock {
        &self.blocks[id.0 as usize]
    }

    /// Get a block mutably by ID.
    #[inline]
    pub fn get_block_mut(&mut self, id: BlockId) -> &mut BasicBlock {
        &mut self.blocks[id.0 as usize]
    }

    /// Add an instruction and return its value reference.
    pub fn add_inst(&mut self, inst: CfgInst) -> CfgValue {
        let value = CfgValue::from_raw(self.values.len() as u32);
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
    pub fn get_inst_mut(&mut self, value: CfgValue) -> &mut CfgInst {
        &mut self.values[value.0 as usize]
    }

    /// Get the total number of values (instructions) in the CFG.
    #[inline]
    pub fn value_count(&self) -> usize {
        self.values.len()
    }

    #[inline]
    pub(crate) fn extra_len(&self) -> usize {
        self.extra.len()
    }

    #[inline]
    pub(crate) fn call_args_len(&self) -> usize {
        self.call_args.len()
    }

    #[inline]
    pub(crate) fn switch_cases_len(&self) -> usize {
        self.switch_cases.len()
    }

    #[inline]
    pub(crate) fn projections_len(&self) -> usize {
        self.projections.len()
    }

    /// Add values to the extra array and return (start, len).
    ///
    /// Used for StructInit fields, ArrayInit elements, and Intrinsic args.
    pub fn push_extra(&mut self, values: impl IntoIterator<Item = CfgValue>) -> (u32, u32) {
        let start = self.extra.len() as u32;
        self.extra.extend(values);
        let len = self.extra.len() as u32 - start;
        (start, len)
    }

    /// Get a slice from the extra array.
    #[inline]
    pub fn get_extra(&self, start: u32, len: u32) -> &[CfgValue] {
        &self.extra[start as usize..(start + len) as usize]
    }

    /// Add call arguments to the call_args array and return (start, len).
    ///
    /// Used for Call instruction arguments.
    pub fn push_call_args(&mut self, args: impl IntoIterator<Item = CfgCallArg>) -> (u32, u32) {
        let start = self.call_args.len() as u32;
        self.call_args.extend(args);
        let len = self.call_args.len() as u32 - start;
        (start, len)
    }

    /// Get a slice from the call_args array.
    #[inline]
    pub fn get_call_args(&self, start: u32, len: u32) -> &[CfgCallArg] {
        &self.call_args[start as usize..(start + len) as usize]
    }

    /// Add switch cases to the switch_cases array and return (start, len).
    ///
    /// Used for Switch terminator cases.
    pub fn push_switch_cases(
        &mut self,
        cases: impl IntoIterator<Item = (i64, BlockId)>,
    ) -> (u32, u32) {
        let start = self.switch_cases.len() as u32;
        self.switch_cases.extend(cases);
        let len = self.switch_cases.len() as u32 - start;
        (start, len)
    }

    /// Get a slice from the switch_cases array.
    #[inline]
    pub fn get_switch_cases(&self, start: u32, len: u32) -> &[(i64, BlockId)] {
        &self.switch_cases[start as usize..(start + len) as usize]
    }

    /// Add projections to the projections array and return (start, len).
    ///
    /// Used for `PlaceRead` and `PlaceWrite` instructions.
    pub fn push_projections(&mut self, projs: impl IntoIterator<Item = Projection>) -> (u32, u32) {
        let start = self.projections.len() as u32;
        self.projections.extend(projs);
        let len = self.projections.len() as u32 - start;
        (start, len)
    }

    /// Get a slice from the projections array.
    #[inline]
    pub fn get_projections(&self, start: u32, len: u32) -> &[Projection] {
        &self.projections[start as usize..(start + len) as usize]
    }

    /// Get projections for a place.
    #[inline]
    pub fn get_place_projections(&self, place: &Place) -> &[Projection] {
        self.get_projections(place.proj_start, place.proj_len)
    }

    /// Create a place with the given base and projections.
    ///
    /// This adds the projections to the projections array and returns a Place
    /// that references them.
    pub fn make_place(
        &mut self,
        base: PlaceBase,
        base_type: Type,
        projs: impl IntoIterator<Item = Projection>,
    ) -> Place {
        let (proj_start, proj_len) = self.push_projections(projs);
        Place {
            base,
            base_type,
            proj_start,
            proj_len,
        }
    }

    /// Get the block arguments from a Goto terminator.
    ///
    /// # Panics
    ///
    /// Panics if the terminator is not a Goto.
    #[inline]
    pub fn get_goto_args(&self, term: &Terminator) -> &[CfgValue] {
        match term {
            Terminator::Goto {
                args_start,
                args_len,
                ..
            } => self.get_extra(*args_start, *args_len),
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
            Terminator::Branch {
                then_args_start,
                then_args_len,
                ..
            } => self.get_extra(*then_args_start, *then_args_len),
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
            Terminator::Branch {
                else_args_start,
                else_args_len,
                ..
            } => self.get_extra(*else_args_start, *else_args_len),
            _ => panic!("get_branch_else_args called on non-Branch terminator"),
        }
    }

    /// Add an instruction to a block.
    pub fn add_inst_to_block(&mut self, block: BlockId, inst: CfgInst) -> CfgValue {
        let value = self.add_inst(inst);
        self.blocks[block.0 as usize].insts.push(value);
        value
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
    pub fn set_terminator(&mut self, block: BlockId, term: Terminator) {
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
    pub fn rewrite_value_uses(&mut self, map: impl Fn(CfgValue) -> CfgValue) {
        use CfgInstData::*;
        for inst in &mut self.values {
            match &mut inst.data {
                Add(a, b)
                | Sub(a, b)
                | Mul(a, b)
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
                | PlaceWrite { value: v, .. }
                | EnumPayloadGet { base: v, .. }
                | IntCast { value: v, .. }
                | Drop { value: v } => *v = map(*v),
                Const(_)
                | BoolConst(_)
                | StringConst(_)
                | Param { .. }
                | BlockParam { .. }
                | Load { .. }
                | PlaceRead { .. }
                | Call { .. }
                | Intrinsic { .. }
                | StructInit { .. }
                | ArrayInit { .. }
                | EnumVariant { .. }
                | StorageLive { .. }
                | StorageDead { .. } => {}
            }
        }
        for value in &mut self.extra {
            *value = map(*value);
        }
        for arg in &mut self.call_args {
            arg.value = map(arg.value);
        }
        for proj in &mut self.projections {
            if let Projection::Index { index, .. } = proj {
                *index = map(*index);
            }
        }
        for block in &mut self.blocks {
            match &mut block.terminator {
                Terminator::Branch { cond, .. } => *cond = map(*cond),
                Terminator::Switch { scrutinee, .. } => *scrutinee = map(*scrutinee),
                Terminator::Return { value: Some(value) } => *value = map(*value),
                Terminator::Goto { .. }
                | Terminator::Return { value: None }
                | Terminator::Unreachable
                | Terminator::None => {}
            }
        }
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
                Terminator::Switch {
                    cases_start,
                    cases_len,
                    default,
                    ..
                } => {
                    for (_, target) in self.get_switch_cases(*cases_start, *cases_len) {
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
                Terminator::Goto {
                    target,
                    args_start,
                    args_len,
                } => {
                    write!(f, "goto {}", target)?;
                    let args = self.get_extra(*args_start, *args_len);
                    if !args.is_empty() {
                        write!(f, "(")?;
                        for (i, arg) in args.iter().enumerate() {
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
                    then_args_start,
                    then_args_len,
                    else_block,
                    else_args_start,
                    else_args_len,
                } => {
                    write!(f, "branch {}, {}", cond, then_block)?;
                    let then_args = self.get_extra(*then_args_start, *then_args_len);
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
                    let else_args = self.get_extra(*else_args_start, *else_args_len);
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
                    cases_start,
                    cases_len,
                    default,
                } => {
                    write!(f, "switch {} [", scrutinee)?;
                    let cases = self.get_switch_cases(*cases_start, *cases_len);
                    for (i, (val, target)) in cases.iter().enumerate() {
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
                name,
                args_start,
                args_len,
            } => {
                match interner {
                    Some(interner) => write!(f, "call @{}(", interner.resolve(name))?,
                    None => write!(f, "call @{}(", name.into_usize())?,
                }
                let args = self.get_call_args(*args_start, *args_len);
                for (i, arg) in args.iter().enumerate() {
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
                name,
                args_start,
                args_len,
            } => {
                match interner {
                    Some(interner) => write!(f, "intrinsic @{}(", interner.resolve(name))?,
                    None => write!(f, "intrinsic @{}(", name.into_usize())?,
                }
                let args = self.get_extra(*args_start, *args_len);
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", arg)?;
                }
                write!(f, ")")
            }
            CfgInstData::StructInit {
                struct_id,
                fields_start,
                fields_len,
            } => {
                write!(f, "struct_init #{} {{", struct_id.0)?;
                let fields = self.get_extra(*fields_start, *fields_len);
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", field)?;
                }
                write!(f, "}}")
            }
            CfgInstData::ArrayInit {
                elements_start,
                elements_len,
            } => {
                write!(f, "array_init [")?;
                let elements = self.get_extra(*elements_start, *elements_len);
                for (i, elem) in elements.iter().enumerate() {
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
                payload_start,
                payload_len,
            } => {
                if *payload_len == 0 {
                    write!(f, "enum_variant #{}::{}", enum_id.0, variant_index)
                } else {
                    // Print the actual payload operands (like StructInit and
                    // ArrayInit do) so the variant's dataflow inputs are
                    // readable in the dump, not just their count.
                    write!(f, "enum_variant #{}::{}(", enum_id.0, variant_index)?;
                    let payload = self.get_extra(*payload_start, *payload_len);
                    for (i, value) in payload.iter().enumerate() {
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
                    "enum_payload_get {} #{}::{}.{}",
                    base, enum_id.0, variant_index, field_index
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

    /// Render a place with its projections RESOLVED (e.g. `$0.#2.1($arr)[v3]`),
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
        }
        for proj in self.get_place_projections(place) {
            match proj {
                Projection::Field {
                    struct_id,
                    field_index,
                } => {
                    let _ = write!(out, ".#{}.{}", struct_id.0, field_index);
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
    use super::*;

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
                    name: call,
                    args_start: 0,
                    args_len: 0,
                },
                ty: Type::UNIT,
                span: Span::new(0, 0),
            },
        );
        cfg.add_inst_to_block(
            entry,
            CfgInst {
                data: CfgInstData::Intrinsic {
                    name: intrinsic,
                    args_start: 0,
                    args_len: 0,
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
}
