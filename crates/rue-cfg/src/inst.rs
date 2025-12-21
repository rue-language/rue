//! CFG instruction definitions.
//!
//! Unlike AIR, the CFG has explicit basic blocks and terminators.
//! Control flow only happens at block boundaries via terminators.

use std::fmt;

use rue_air::{ArrayTypeId, EnumId, StructId, Type};
use rue_span::Span;

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

    // Logical operations (return bool)
    And(CfgValue, CfgValue),
    Or(CfgValue, CfgValue),

    // Unary operations
    Neg(CfgValue),
    Not(CfgValue),

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

    // Function calls
    Call {
        name: String,
        args: Vec<CfgValue>,
    },

    /// Intrinsic call (e.g., @dbg)
    Intrinsic {
        name: String,
        args: Vec<CfgValue>,
    },

    // Struct operations
    StructInit {
        struct_id: StructId,
        fields: Vec<CfgValue>,
    },
    FieldGet {
        base: CfgValue,
        struct_id: StructId,
        field_index: u32,
    },
    FieldSet {
        slot: u32,
        struct_id: StructId,
        field_index: u32,
        value: CfgValue,
    },

    // Array operations
    ArrayInit {
        array_type_id: ArrayTypeId,
        elements: Vec<CfgValue>,
    },
    IndexGet {
        base: CfgValue,
        array_type_id: ArrayTypeId,
        index: CfgValue,
    },
    IndexSet {
        slot: u32,
        array_type_id: ArrayTypeId,
        index: CfgValue,
        value: CfgValue,
    },

    // Enum operations
    /// Create an enum variant (discriminant value)
    EnumVariant {
        enum_id: EnumId,
        variant_index: u32,
    },
}

/// Block terminator - how control leaves a basic block.
///
/// Terminators are the ONLY place where control flow happens in the CFG.
#[derive(Debug, Clone)]
pub enum Terminator {
    /// Unconditional jump to another block.
    Goto {
        target: BlockId,
        /// Arguments to pass to target block's parameters
        args: Vec<CfgValue>,
    },

    /// Conditional branch.
    Branch {
        cond: CfgValue,
        then_block: BlockId,
        then_args: Vec<CfgValue>,
        else_block: BlockId,
        else_args: Vec<CfgValue>,
    },

    /// Multi-way branch (switch/match).
    Switch {
        /// The value to switch on
        scrutinee: CfgValue,
        /// Cases: (value, target_block)
        cases: Vec<(u64, BlockId)>,
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
    /// Number of local variable slots
    num_locals: u32,
    /// Number of parameter slots
    num_params: u32,
    /// Function name
    fn_name: String,
}

impl Cfg {
    /// Create a new CFG.
    pub fn new(return_type: Type, num_locals: u32, num_params: u32, fn_name: String) -> Self {
        Self {
            blocks: Vec::new(),
            entry: BlockId(0),
            return_type,
            values: Vec::new(),
            num_locals,
            num_params,
            fn_name,
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
                Terminator::Switch { cases, default, .. } => {
                    for (_, target) in cases {
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
                Terminator::Goto { target, args } => {
                    write!(f, "goto {}", target)?;
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
                    then_args,
                    else_block,
                    else_args,
                } => {
                    write!(f, "branch {}, {}", cond, then_block)?;
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
            CfgInstData::And(lhs, rhs) => write!(f, "and {}, {}", lhs, rhs),
            CfgInstData::Or(lhs, rhs) => write!(f, "or {}, {}", lhs, rhs),
            CfgInstData::Neg(v) => write!(f, "neg {}", v),
            CfgInstData::Not(v) => write!(f, "not {}", v),
            CfgInstData::Alloc { slot, init } => write!(f, "alloc ${} = {}", slot, init),
            CfgInstData::Load { slot } => write!(f, "load ${}", slot),
            CfgInstData::Store { slot, value } => write!(f, "store ${} = {}", slot, value),
            CfgInstData::Call { name, args } => {
                write!(f, "call {}(", name)?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", arg)?;
                }
                write!(f, ")")
            }
            CfgInstData::Intrinsic { name, args } => {
                write!(f, "intrinsic @{}(", name)?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", arg)?;
                }
                write!(f, ")")
            }
            CfgInstData::StructInit { struct_id, fields } => {
                write!(f, "struct_init #{} {{", struct_id.0)?;
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", field)?;
                }
                write!(f, "}}")
            }
            CfgInstData::FieldGet {
                base,
                struct_id,
                field_index,
            } => {
                write!(f, "field_get {}.#{}.{}", base, struct_id.0, field_index)
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
            CfgInstData::ArrayInit {
                array_type_id,
                elements,
            } => {
                write!(f, "array_init @{} [", array_type_id.0)?;
                for (i, elem) in elements.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", elem)?;
                }
                write!(f, "]")
            }
            CfgInstData::IndexGet {
                base,
                array_type_id,
                index,
            } => {
                write!(f, "index_get {}(@{})[{}]", base, array_type_id.0, index)
            }
            CfgInstData::IndexSet {
                slot,
                array_type_id,
                index,
                value,
            } => {
                write!(
                    f,
                    "index_set ${}(@{})[{}] = {}",
                    slot, array_type_id.0, index, value
                )
            }
            CfgInstData::EnumVariant {
                enum_id,
                variant_index,
            } => {
                write!(f, "enum_variant #{}::{}", enum_id.0, variant_index)
            }
        }
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
    fn test_create_cfg() {
        let mut cfg = Cfg::new(Type::I32, 0, 0, "test".to_string());
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
