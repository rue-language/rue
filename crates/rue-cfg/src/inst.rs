//! CFG instruction definitions.
//!
//! Unlike AIR, the CFG has explicit basic blocks and terminators.
//! Control flow only happens at block boundaries via terminators.
//!
//! # Place Expressions (ADR-0030)
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

use lasso::{Key, Spur};
use rue_air::{EnumId, StructId, Type};
use rue_span::Span;

// ============================================================================
// Place Expressions (ADR-0030)
// ============================================================================

/// A memory location that can be read from or written to.
///
/// A place represents a path to a memory location, consisting of a base
/// (local variable or parameter) and zero or more projections (field access,
/// array indexing).
///
/// # Examples
///
/// - `x` → `Place { base: Local(0), proj_start: 0, proj_len: 0 }`
/// - `arr[i]` → `Place { base: Local(0), proj_start: 0, proj_len: 1 }` with `Index` projection
/// - `point.x` → `Place { base: Local(0), proj_start: 0, proj_len: 1 }` with `Field` projection
/// - `arr[i].x` → `Place { base: Local(0), proj_start: 0, proj_len: 2 }` with `Index` then `Field`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Place {
    /// The base of the place - either a local slot or parameter slot
    pub base: PlaceBase,
    /// Start index into Cfg's projections array
    pub proj_start: u32,
    /// Number of projections
    pub proj_len: u32,
}

impl Place {
    /// Create a place for a local variable with no projections.
    #[inline]
    pub const fn local(slot: u32) -> Self {
        Self {
            base: PlaceBase::Local(slot),
            proj_start: 0,
            proj_len: 0,
        }
    }

    /// Create a place for a parameter with no projections.
    #[inline]
    pub const fn param(slot: u32) -> Self {
        Self {
            base: PlaceBase::Param(slot),
            proj_start: 0,
            proj_len: 0,
        }
    }

    /// Returns true if this place has no projections (is just a variable).
    #[inline]
    pub const fn is_simple(&self) -> bool {
        self.proj_len == 0
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

    // Place operations (ADR-0030)
    /// Read a value from a memory location.
    ///
    /// This unifies Load, IndexGet, and FieldGet into a single instruction
    /// that can handle arbitrarily nested access patterns like `arr[i].field`.
    PlaceRead {
        place: Place,
    },

    /// Write a value to a memory location.
    ///
    /// This unifies Store, IndexSet, ParamIndexSet, FieldSet, and ParamFieldSet
    /// into a single instruction that can handle nested writes.
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
    FieldSet {
        slot: u32,
        struct_id: StructId,
        field_index: u32,
        value: CfgValue,
    },
    /// Store a value to a struct field (for parameters, including inout)
    ParamFieldSet {
        /// The parameter's ABI slot (relative to params, not locals)
        param_slot: u32,
        /// Offset within the struct for nested field access
        inner_offset: u32,
        struct_id: StructId,
        field_index: u32,
        value: CfgValue,
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
    /// Store a value to an array element.
    IndexSet {
        slot: u32,
        /// The array type (for bounds checking and element size)
        array_type: Type,
        index: CfgValue,
        value: CfgValue,
    },
    /// Store a value to an array element of an inout parameter
    ParamIndexSet {
        /// The parameter's ABI slot (relative to params, not locals)
        param_slot: u32,
        /// The array type (for bounds checking and element size)
        array_type: Type,
        /// Index expression
        index: CfgValue,
        /// Value to store
        value: CfgValue,
    },

    // Enum operations
    /// Create an enum variant (discriminant value)
    EnumVariant {
        enum_id: EnumId,
        variant_index: u32,
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
    },

    /// Marks that a local slot becomes dead (storage can be deallocated).
    /// The slot is now invalid to read from.
    /// Drop elaboration inserts Drop before this if the type needs drop.
    StorageDead {
        slot: u32,
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
    /// Predecessor blocks (filled in after construction)
    pub preds: Vec<BlockId>,
}

impl BasicBlock {
    /// Create a new empty basic block.
    pub fn new(id: BlockId) -> Self {
        Self {
            id,
            params: Vec::new(),
            insts: Vec::new(),
            terminator: Terminator::None,
            preds: Vec::new(),
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
    extra: Vec<CfgValue>,
    /// Extra storage for call arguments (CfgCallArg).
    /// Call instructions store (start, len) indices into this array.
    call_args: Vec<CfgCallArg>,
    /// Extra storage for switch cases (value, target block pairs).
    /// Switch terminators store (start, len) indices into this array.
    switch_cases: Vec<(i64, BlockId)>,
    /// Extra storage for place projections (ADR-0030).
    /// Place instructions store (start, len) indices into this array.
    projections: Vec<Projection>,
    /// Number of local variable slots
    num_locals: u32,
    /// Number of parameter slots
    num_params: u32,
    /// Function name
    fn_name: String,
    /// Whether each parameter slot is inout (passed by reference)
    param_modes: Vec<bool>,
}

impl Cfg {
    /// Create a new CFG.
    pub fn new(
        return_type: Type,
        num_locals: u32,
        num_params: u32,
        fn_name: String,
        param_modes: Vec<bool>,
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
            param_modes,
        }
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
        let slot = self.num_locals;
        self.num_locals += 1;
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

    /// Get whether a parameter slot is inout.
    #[inline]
    pub fn is_param_inout(&self, slot: u32) -> bool {
        self.param_modes
            .get(slot as usize)
            .copied()
            .unwrap_or(false)
    }

    /// Get the parameter modes slice.
    #[inline]
    pub fn param_modes(&self) -> &[bool] {
        &self.param_modes
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
    /// Used for PlaceRead and PlaceWrite instructions (ADR-0030).
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
        projs: impl IntoIterator<Item = Projection>,
    ) -> Place {
        let (proj_start, proj_len) = self.push_projections(projs);
        Place {
            base,
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
    pub fn set_terminator(&mut self, block: BlockId, term: Terminator) {
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

    /// Compute predecessor lists for all blocks.
    pub fn compute_predecessors(&mut self) {
        // Clear existing predecessors
        for block in &mut self.blocks {
            block.preds.clear();
        }

        // Collect edges
        let mut edges: Vec<(BlockId, BlockId)> = Vec::new();
        for block in &self.blocks {
            match &block.terminator {
                Terminator::Goto { target, .. } => {
                    edges.push((block.id, *target));
                }
                Terminator::Branch {
                    then_block,
                    else_block,
                    ..
                } => {
                    edges.push((block.id, *then_block));
                    edges.push((block.id, *else_block));
                }
                Terminator::Switch {
                    cases_start,
                    cases_len,
                    default,
                    ..
                } => {
                    for (_, target) in self.get_switch_cases(*cases_start, *cases_len) {
                        edges.push((block.id, *target));
                    }
                    edges.push((block.id, *default));
                }
                Terminator::Return { .. } | Terminator::Unreachable | Terminator::None => {}
            }
        }

        // Add predecessors
        for (from, to) in edges {
            self.blocks[to.0 as usize].preds.push(from);
        }
    }
}

impl fmt::Display for Cfg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "cfg {} (return_type: {}) {{",
            self.fn_name,
            self.return_type.name()
        )?;
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
            if !block.preds.is_empty() {
                write!(f, "    ; preds: ")?;
                for (i, pred) in block.preds.iter().enumerate() {
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
                self.fmt_inst_data(f, &inst.data)?;
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

impl Cfg {
    fn fmt_inst_data(&self, f: &mut fmt::Formatter<'_>, data: &CfgInstData) -> fmt::Result {
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
                // Display symbol as @{id} since we don't have interner access here
                write!(f, "call @{}(", name.into_usize())?;
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
                // Display symbol as @{id} since we don't have interner access here
                write!(f, "intrinsic @{}(", name.into_usize())?;
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
            CfgInstData::FieldSet {
                slot,
                struct_id,
                field_index,
                value,
            } => {
                write!(
                    f,
                    "field_set ${}.#{}.{} = {}",
                    slot, struct_id.0, field_index, value
                )
            }
            CfgInstData::ParamFieldSet {
                param_slot,
                inner_offset,
                struct_id,
                field_index,
                value,
            } => {
                write!(
                    f,
                    "param_field_set %{}+{}.#{}.{} = {}",
                    param_slot, inner_offset, struct_id.0, field_index, value
                )
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
            CfgInstData::IndexSet {
                slot,
                array_type,
                index,
                value,
            } => {
                write!(
                    f,
                    "index_set ${}({})[{}] = {}",
                    slot,
                    array_type.name(),
                    index,
                    value
                )
            }
            CfgInstData::ParamIndexSet {
                param_slot,
                array_type,
                index,
                value,
            } => {
                write!(
                    f,
                    "param_index_set %{}({})[{}] = {}",
                    param_slot,
                    array_type.name(),
                    index,
                    value
                )
            }
            CfgInstData::EnumVariant {
                enum_id,
                variant_index,
            } => {
                write!(f, "enum_variant #{}::{}", enum_id.0, variant_index)
            }
            CfgInstData::IntCast { value, from_ty } => {
                write!(f, "intcast {} from {}", value, from_ty.name())
            }
            CfgInstData::Drop { value } => {
                write!(f, "drop {}", value)
            }
            CfgInstData::StorageLive { slot } => {
                write!(f, "storage_live ${}", slot)
            }
            CfgInstData::StorageDead { slot } => {
                write!(f, "storage_dead ${}", slot)
            }
        }
    }

    /// Format a place for display, showing the base and projections.
    fn fmt_place(&self, f: &mut fmt::Formatter<'_>, place: &Place) -> fmt::Result {
        // Write the base
        match place.base {
            PlaceBase::Local(slot) => write!(f, "${}", slot)?,
            PlaceBase::Param(slot) => write!(f, "param%{}", slot)?,
        }

        // Write the projections
        let projections = self.get_place_projections(place);
        for proj in projections {
            match proj {
                Projection::Field {
                    struct_id,
                    field_index,
                } => {
                    write!(f, ".#{}.{}", struct_id.0, field_index)?;
                }
                Projection::Index { array_type, index } => {
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
}
