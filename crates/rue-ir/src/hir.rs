//! HIR - Hierarchical High-level Intermediate Representation
//!
//! HIR transforms the tree-based AST into a hierarchical instruction-based representation
//! inspired by Zig's ZIR. This provides better cache efficiency, simpler optimization passes,
//! and proper hierarchical structure for control flow.
//!
//! ## Design Philosophy
//!
//! Following Zig's approach with proper hierarchy: "Everything is instructions, but blocks own them."
//! The HIR represents a program as hierarchical blocks containing instructions, where control
//! flow instructions directly own their nested blocks.
//!
//! ## Key Design Principles
//!
//! 1. **Blocks Own Instructions**: Each block contains its own vector of instructions
//! 2. **Control Flow Owns Blocks**: While/If instructions directly own their body blocks
//! 3. **No Circular References**: Parent-child relationships are explicit and acyclic
//! 4. **SSA Support**: Block parameters enable proper SSA form with loop back-edges
//! 5. **Cache Efficiency**: Instructions within a block are stored contiguously

use crate::types::RueType;
use rue_lexer::Span;
use std::fmt;

/// Instruction index within a block - wraps u32 for type safety
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalInstIndex(u32);

impl LocalInstIndex {
    /// Create a new local instruction index
    #[inline]
    pub fn new(index: u32) -> Self {
        LocalInstIndex(index)
    }

    /// Get the raw index value
    #[inline]
    pub fn raw(self) -> u32 {
        self.0
    }

    /// Get as array index
    #[inline]
    pub fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// Block parameter index within a block - wraps u32 for type safety
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockParamIndex(u32);

impl BlockParamIndex {
    /// Create a new block parameter index
    #[inline]
    pub fn new(index: u32) -> Self {
        BlockParamIndex(index)
    }

    /// Get the raw index value
    #[inline]
    pub fn raw(self) -> u32 {
        self.0
    }

    /// Get as array index
    #[inline]
    pub fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// Value reference - can refer to either an instruction result or a block parameter
/// This is critical for SSA form where values can come from either instructions
/// or block parameters (which serve the same purpose as phi nodes)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueRef {
    /// Reference to an instruction within a specific block
    Instruction {
        block_id: BlockId,
        index: LocalInstIndex,
    },
    /// Reference to a block parameter within a specific block (serves the same purpose as phi nodes in traditional SSA)
    BlockParam {
        block_id: BlockId,
        index: BlockParamIndex,
    },
}

impl ValueRef {
    /// Create a ValueRef from a block ID and instruction index
    #[inline]
    pub fn from_inst(block_id: BlockId, index: LocalInstIndex) -> Self {
        ValueRef::Instruction { block_id, index }
    }

    /// Create a ValueRef from an instruction index (legacy method, assumes current block)
    #[inline]
    pub fn from_instruction(index: LocalInstIndex) -> Self {
        // This method is deprecated and should not be used in new code
        // For compatibility, we use a dummy block ID - this should be replaced with from_inst
        ValueRef::Instruction {
            block_id: BlockId::new(0),
            index,
        }
    }

    /// Create a ValueRef from a block parameter with block ID
    #[inline]
    pub fn from_block_param_with_id(block_id: BlockId, index: BlockParamIndex) -> Self {
        ValueRef::BlockParam { block_id, index }
    }

    /// Create a ValueRef from a block parameter index (legacy method)
    #[inline]
    pub fn from_block_param(index: BlockParamIndex) -> Self {
        // This method is deprecated and should not be used in new code
        // For compatibility, we use a dummy block ID
        ValueRef::BlockParam {
            block_id: BlockId::new(0),
            index,
        }
    }

    /// Create a ValueRef from a raw instruction index with block ID
    #[inline]
    pub fn from_inst_index_with_block(block_id: BlockId, index: u32) -> Self {
        ValueRef::Instruction {
            block_id,
            index: LocalInstIndex::new(index),
        }
    }

    /// Create a ValueRef from a raw instruction index (legacy method)
    #[inline]
    pub fn from_inst_index(index: u32) -> Self {
        ValueRef::Instruction {
            block_id: BlockId::new(0),
            index: LocalInstIndex::new(index),
        }
    }

    /// Create a ValueRef from a raw block parameter index
    #[inline]
    pub fn from_param_index(index: u32) -> Self {
        ValueRef::BlockParam {
            block_id: BlockId::new(0),
            index: BlockParamIndex::new(index),
        }
    }

    /// Check if this is an instruction reference
    #[inline]
    pub fn is_instruction(&self) -> bool {
        matches!(self, ValueRef::Instruction { .. })
    }

    /// Check if this is a block parameter reference
    #[inline]
    pub fn is_block_param(&self) -> bool {
        matches!(self, ValueRef::BlockParam { .. })
    }

    /// Get the instruction index if this is an instruction reference
    #[inline]
    pub fn as_instruction(&self) -> Option<LocalInstIndex> {
        match self {
            ValueRef::Instruction { index, .. } => Some(*index),
            _ => None,
        }
    }

    /// Get the block parameter index if this is a block parameter reference
    #[inline]
    pub fn as_block_param(&self) -> Option<BlockParamIndex> {
        match self {
            ValueRef::BlockParam { index, .. } => Some(*index),
            _ => None,
        }
    }

    /// Get the block ID of this value reference
    #[inline]
    pub fn block_id(&self) -> BlockId {
        match self {
            ValueRef::Instruction { block_id, .. } => *block_id,
            ValueRef::BlockParam { block_id, .. } => *block_id,
        }
    }

    /// Get both block ID and instruction index if this is an instruction reference
    #[inline]
    pub fn as_instruction_with_block(&self) -> Option<(BlockId, LocalInstIndex)> {
        match self {
            ValueRef::Instruction { block_id, index } => Some((*block_id, *index)),
            _ => None,
        }
    }

    /// Get both block ID and parameter index if this is a block parameter reference
    #[inline]
    pub fn as_block_param_with_block(&self) -> Option<(BlockId, BlockParamIndex)> {
        match self {
            ValueRef::BlockParam { block_id, index } => Some((*block_id, *index)),
            _ => None,
        }
    }
}

/// Global instruction identifier - combines block ID and local index
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstId {
    pub block: BlockId,
    pub local_index: LocalInstIndex,
}

/// Block identifier - unique across the HIR
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(u32);

impl BlockId {
    #[inline]
    pub fn new(id: u32) -> Self {
        BlockId(id)
    }

    #[inline]
    pub fn raw(self) -> u32 {
        self.0
    }

    #[inline]
    pub fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// Function identifier - unique across the HIR
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FunctionId(u32);

impl FunctionId {
    #[inline]
    pub fn new(id: u32) -> Self {
        FunctionId(id)
    }

    #[inline]
    pub fn raw(self) -> u32 {
        self.0
    }

    #[inline]
    pub fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// String index into the string pool
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StringIndex(u32);

impl StringIndex {
    #[inline]
    pub fn raw(self) -> u32 {
        self.0
    }

    #[inline]
    pub fn as_usize(self) -> usize {
        self.0 as usize
    }

    #[inline]
    pub fn from_raw(idx: u32) -> Self {
        StringIndex(idx)
    }
}

/// Instruction tags - identifies the instruction type
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstTag {
    // Statement instructions
    Let,
    Assign,

    // Expression instructions
    Literal,
    Binary,
    Unary,
    Call,
    Cast,

    // Aggregate instructions
    StructLit,
    TupleLit,
    ArrayLit,
    FieldAccess,
    IndexAccess,
}

/// Binary operators
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Unary operators
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
}

impl BinOp {
    pub fn from_u32(val: u32) -> Self {
        match val {
            0 => BinOp::Add,
            1 => BinOp::Sub,
            2 => BinOp::Mul,
            3 => BinOp::Div,
            4 => BinOp::Mod,
            5 => BinOp::Eq,
            6 => BinOp::Ne,
            7 => BinOp::Lt,
            8 => BinOp::Le,
            9 => BinOp::Gt,
            10 => BinOp::Ge,
            _ => panic!("Invalid BinOp value: {}", val),
        }
    }
}

impl UnaryOp {
    pub fn from_u32(val: u32) -> Self {
        match val {
            0 => UnaryOp::Neg,
            _ => panic!("Invalid UnaryOp value: {}", val),
        }
    }
}

/// Block terminators - explicit control flow instructions that end blocks
#[derive(Debug, Clone, PartialEq)]
pub enum Terminator {
    /// Unconditional jump to target block with arguments
    Goto {
        target: BlockId,
        args: Vec<ValueRef>,
    },
    /// Conditional branch based on condition value
    If {
        cond: ValueRef,
        then_target: BlockId,
        then_args: Vec<ValueRef>,
        else_target: BlockId,
        else_args: Vec<ValueRef>,
    },
    /// Return from function with optional value
    Return { value: Option<ValueRef> },
    /// Break out of loop (jump to loop exit with arguments)
    Break {
        loop_header: BlockId,
        args: Vec<ValueRef>,
    },
    /// Continue to next loop iteration (jump to loop header with arguments)
    Continue {
        loop_header: BlockId,
        args: Vec<ValueRef>,
    },
}

/// Block parameter definition
#[derive(Debug, Clone, PartialEq)]
pub struct BlockParam {
    pub name: StringIndex,
    pub ty: RueType,
}

/// A block containing its own instructions and terminator
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub id: BlockId,
    pub params: Vec<BlockParam>,
    pub body: Vec<Instruction>,
    pub term: Terminator,
    pub span: Span,
}

impl Block {
    /// Create a new block with terminator
    pub fn new(id: BlockId, params: Vec<BlockParam>, term: Terminator, span: Span) -> Self {
        Self {
            id,
            params,
            body: Vec::new(),
            term,
            span,
        }
    }

    /// Create a new empty block with a placeholder return terminator
    pub fn new_empty(id: BlockId, params: Vec<BlockParam>, span: Span) -> Self {
        Self::new(id, params, Terminator::Return { value: None }, span)
    }

    /// Add an instruction to the block and return its local index
    pub fn push_instruction(&mut self, inst: Instruction) -> LocalInstIndex {
        let index = self.body.len() as u32;
        self.body.push(inst);
        LocalInstIndex::new(index)
    }

    /// Get an instruction by local index
    pub fn get_instruction(&self, index: LocalInstIndex) -> Option<&Instruction> {
        self.body.get(index.as_usize())
    }

    /// Iterate over all instructions in the block
    pub fn iter_instructions(&self) -> impl Iterator<Item = (LocalInstIndex, &Instruction)> {
        self.body
            .iter()
            .enumerate()
            .map(|(i, inst)| (LocalInstIndex::new(i as u32), inst))
    }

    /// Set the block terminator
    pub fn set_terminator(&mut self, term: Terminator) {
        self.term = term;
    }
}

/// Instruction data variants for different instruction types
#[derive(Debug, Clone, PartialEq)]
pub enum InstData {
    /// Simple two-operand data (most instructions)
    Simple { lhs: u32, rhs: u32 },

    /// Literal value
    Literal(u64),

    /// Variable/function name reference
    Name(StringIndex),

    /// Let binding with name and optional initializer
    Let {
        name: StringIndex,
        init: Option<ValueRef>,
    },

    /// Assignment with name and value
    Assign { name: StringIndex, value: ValueRef },

    /// Binary operation with operator and operands
    Binary {
        op: BinOp,
        lhs: ValueRef,
        rhs: ValueRef,
    },

    /// Unary operation with operator and operand
    Unary { op: UnaryOp, operand: ValueRef },

    /// Function call with name and arguments
    Call {
        func: StringIndex,
        callee: Option<FunctionId>, // Resolved function ID (filled during resolution)
        args: Vec<ValueRef>,
    },

    /// Array literal with elements
    ArrayLit(Vec<ValueRef>),

    /// Tuple literal with elements
    TupleLit(Vec<ValueRef>),

    /// Struct literal with field values
    StructLit {
        struct_id: u32,
        fields: Vec<ValueRef>,
    },

    /// Field access (tuple/struct)
    FieldAccess { base: ValueRef, field: u32 },

    /// Array index access
    IndexAccess { base: ValueRef, index: ValueRef },

    /// Type cast
    Cast {
        value: ValueRef,
        target_type: RueType,
    },
}

/// A single instruction
#[derive(Debug, Clone, PartialEq)]
pub struct Instruction {
    pub tag: InstTag,
    pub data: InstData,
    pub ty: Option<RueType>,
    pub span: Span,
}

/// Function definition
#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub name: StringIndex,
    pub params: Vec<BlockParam>,
    pub return_type: Option<RueType>,
    pub body: Block,
    pub span: Span,
}

/// String pool for deduplication
#[derive(Debug, Clone, PartialEq)]
pub struct StringPool {
    strings: Vec<String>,
    lookup: std::collections::HashMap<String, StringIndex>,
}

impl Default for StringPool {
    fn default() -> Self {
        Self::new()
    }
}

impl StringPool {
    pub fn new() -> Self {
        Self {
            strings: Vec::with_capacity(256),
            lookup: std::collections::HashMap::new(),
        }
    }

    pub fn intern(&mut self, s: &str) -> StringIndex {
        use std::collections::hash_map::Entry;

        match self.lookup.entry(s.to_string()) {
            Entry::Occupied(o) => *o.get(),
            Entry::Vacant(v) => {
                let index = StringIndex(self.strings.len() as u32);
                self.strings.push(v.key().clone());
                *v.insert(index)
            }
        }
    }

    pub fn lookup(&self, s: &str) -> Option<StringIndex> {
        self.lookup.get(s).copied()
    }

    #[inline]
    pub fn get(&self, index: StringIndex) -> &str {
        &self.strings[index.as_usize()]
    }

    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

/// Main HIR structure with hierarchical blocks
#[derive(Clone, PartialEq)]
pub struct Hir {
    /// Central storage for all blocks
    pub block_arena: Vec<Block>,

    /// Top-level block IDs (function bodies and module-level code)
    pub blocks: Vec<BlockId>,

    /// Function arena - all functions stored here
    pub function_arena: Vec<Function>,

    /// Function IDs (for tracking which functions exist)
    pub functions: Vec<FunctionId>,

    /// String pool for identifiers
    pub strings: StringPool,

    /// Main function index (entry point)
    pub main_function: Option<usize>,

    /// Next block ID to allocate
    next_block_id: u32,
}

impl Default for Hir {
    fn default() -> Self {
        Self::new()
    }
}

impl Hir {
    /// Create a new empty HIR
    pub fn new() -> Self {
        Self {
            block_arena: Vec::new(),
            blocks: Vec::new(),
            function_arena: Vec::new(),
            functions: Vec::new(),
            strings: StringPool::new(),
            main_function: None,
            next_block_id: 0,
        }
    }

    /// Generate a new unique block ID
    pub fn next_block_id(&mut self) -> BlockId {
        let id = self.next_block_id;
        self.next_block_id += 1;
        BlockId::new(id)
    }

    /// Allocate a new block in the arena
    pub fn alloc_block(&mut self, params: Vec<BlockParam>, span: Span) -> BlockId {
        // The block ID should match the arena index
        let id = BlockId::new(self.block_arena.len() as u32);
        let block = Block {
            id,
            params,
            body: Vec::new(),
            term: Terminator::Return { value: None }, // Default terminator
            span,
        };
        self.block_arena.push(block);
        // Update the next_block_id counter to stay in sync
        self.next_block_id = self.block_arena.len() as u32;
        id
    }

    /// Add a block to the arena and return its ID
    pub fn add_block(&mut self, block: Block) -> BlockId {
        let id = block.id;
        // Ensure the ID matches the arena position
        assert_eq!(id.as_usize(), self.block_arena.len());
        self.block_arena.push(block);
        id
    }

    /// Add a top-level block ID
    pub fn add_top_level_block(&mut self, id: BlockId) {
        self.blocks.push(id);
    }

    /// Add a function definition
    pub fn add_function(&mut self, func: Function) -> FunctionId {
        let index = self.function_arena.len();
        let id = FunctionId::new(index as u32);

        // If this is the main function, mark it
        if self.strings.get(func.name) == "main" {
            self.main_function = Some(index);
        }

        self.function_arena.push(func);
        self.functions.push(id);
        id
    }

    /// Find a function by name
    pub fn find_function(&self, name: &str) -> Option<FunctionId> {
        let name_idx = self.strings.lookup(name)?;

        self.function_arena
            .iter()
            .position(|func| func.name == name_idx)
            .map(|idx| FunctionId::new(idx as u32))
    }

    /// Get function data by ID
    pub fn get_function(&self, id: FunctionId) -> Option<&Function> {
        self.function_arena.get(id.as_usize())
    }

    /// Get function data by index (legacy)
    pub fn get_function_by_index(&self, index: usize) -> Option<&Function> {
        self.function_arena.get(index)
    }

    /// Get a block by ID from the arena
    pub fn block(&self, id: BlockId) -> Option<&Block> {
        self.block_arena.get(id.as_usize())
    }

    /// Get a mutable block by ID from the arena
    pub fn block_mut(&mut self, id: BlockId) -> Option<&mut Block> {
        self.block_arena.get_mut(id.as_usize())
    }

    /// Get a block by ID (legacy method, redirects to block())
    pub fn get_block(&self, id: BlockId) -> Option<&Block> {
        self.block(id)
    }

    /// Get the successor block IDs for a given block
    pub fn successors(&self, block_id: BlockId) -> Vec<BlockId> {
        let block = match self.get_block(block_id) {
            Some(block) => block,
            None => return vec![],
        };

        match &block.term {
            Terminator::Goto { target, .. } => vec![*target],
            Terminator::If {
                then_target,
                else_target,
                ..
            } => {
                vec![*then_target, *else_target]
            }
            Terminator::Return { .. } => vec![], // No successors
            Terminator::Break { loop_header, .. } => vec![*loop_header],
            Terminator::Continue { loop_header, .. } => vec![*loop_header],
        }
    }

    /// Get all predecessor block IDs for a given block
    pub fn predecessors(&self, target_id: BlockId) -> Vec<BlockId> {
        let mut preds = vec![];

        // Check all blocks in the arena
        for (i, _) in self.block_arena.iter().enumerate() {
            let block_id = BlockId::new(i as u32);
            if self.successors(block_id).contains(&target_id) {
                preds.push(block_id);
            }
        }

        preds
    }

    /// Check if a block is reachable from another block
    pub fn is_reachable_from(&self, from: BlockId, to: BlockId) -> bool {
        if from == to {
            return true;
        }

        let mut visited = std::collections::HashSet::new();
        let mut worklist = vec![from];

        while let Some(current) = worklist.pop() {
            if visited.contains(&current) {
                continue;
            }
            visited.insert(current);

            for successor in self.successors(current) {
                if successor == to {
                    return true;
                }
                if !visited.contains(&successor) {
                    worklist.push(successor);
                }
            }
        }

        false
    }

    /// Walk a block hierarchy (including nested blocks in control flow)
    /// Note: With explicit terminators, nested blocks are no longer owned by instructions.
    /// This method now only walks the current block's instructions and terminator.
    /// For CFG traversal, use the CFG utilities instead.
    pub fn walk_block_hierarchy<F>(&self, block: &Block, visitor: &mut F)
    where
        F: FnMut(&Block, &Instruction),
    {
        for inst in &block.body {
            visitor(block, inst);
        }
        // Terminators are handled separately - they don't contain nested instructions
        // For CFG analysis, use the CFG utilities that will be added in step 6
    }
}

impl fmt::Debug for Hir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hir")
            .field("blocks", &self.blocks.len())
            .field("functions", &self.functions.len())
            .field("strings", &self.strings.strings.len())
            .finish()
    }
}

/// Pretty printing for debugging
impl fmt::Display for Hir {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "HIR:")?;
        writeln!(f)?;

        // Print functions
        for (i, func_id) in self.functions.iter().enumerate() {
            let func = &self.function_arena[func_id.as_usize()];
            writeln!(f, "Function #{}: {}", i, self.strings.get(func.name))?;
            writeln!(f, "  Parameters:")?;
            for param in &func.params {
                writeln!(f, "    {}: {}", self.strings.get(param.name), param.ty)?;
            }
            if let Some(ret_ty) = &func.return_type {
                writeln!(f, "  Returns: {}", ret_ty)?;
            }
            writeln!(f, "  Body:")?;
            self.format_block(f, &func.body, 2)?;
            writeln!(f)?;
        }

        // Print top-level blocks
        for &block_id in &self.blocks {
            writeln!(f, "Block #{}:", block_id.raw())?;
            if let Some(block) = self.block(block_id) {
                self.format_block(f, block, 1)?;
            }
            writeln!(f)?;
        }

        Ok(())
    }
}

impl Hir {
    /// Helper function to format a ValueRef for display
    fn format_value_ref(&self, value_ref: &ValueRef) -> String {
        match value_ref {
            ValueRef::Instruction { block_id, index } => {
                format!("b{}:[{}]", block_id.raw(), index.raw())
            }
            ValueRef::BlockParam { block_id, index } => {
                format!("b{}:$param{}", block_id.raw(), index.raw())
            }
        }
    }

    fn format_block(&self, f: &mut fmt::Formatter, block: &Block, indent: usize) -> fmt::Result {
        let indent_str = "  ".repeat(indent);

        // Print block parameters if any
        if !block.params.is_empty() {
            write!(f, "{}Parameters: ", indent_str)?;
            for (i, param) in block.params.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}: {}", self.strings.get(param.name), param.ty)?;
            }
            writeln!(f)?;
        }

        // Print instructions
        for (idx, inst) in block.iter_instructions() {
            write!(f, "{}[{}] {:?} ", indent_str, idx.raw(), inst.tag)?;
            self.format_instruction(f, inst, indent)?;

            if let Some(ty) = &inst.ty {
                write!(f, " : {}", ty)?;
            }
            writeln!(f)?;
        }

        // Print terminator
        write!(f, "{}Terminator: ", indent_str)?;
        self.format_terminator(f, &block.term)?;
        writeln!(f)?;

        Ok(())
    }

    fn format_instruction(
        &self,
        f: &mut fmt::Formatter,
        inst: &Instruction,
        indent: usize,
    ) -> fmt::Result {
        let _indent_str = "  ".repeat(indent + 1);

        match &inst.data {
            InstData::Simple { lhs, rhs } => {
                write!(f, "({}, {})", lhs, rhs)
            }
            InstData::Literal(value) => {
                write!(f, "{}", value)
            }
            InstData::Name(name) => {
                write!(f, "\"{}\"", self.strings.get(*name))
            }
            InstData::Let { name, init } => {
                write!(f, "\"{}\"", self.strings.get(*name))?;
                if let Some(init_ref) = init {
                    write!(f, " = {}", self.format_value_ref(init_ref))?;
                }
                Ok(())
            }
            InstData::Assign { name, value } => {
                write!(
                    f,
                    "\"{}\" = {}",
                    self.strings.get(*name),
                    self.format_value_ref(value)
                )
            }
            InstData::Binary { op, lhs, rhs } => {
                write!(
                    f,
                    "{} {:?} {}",
                    self.format_value_ref(lhs),
                    op,
                    self.format_value_ref(rhs)
                )
            }
            InstData::Unary { op, operand } => {
                write!(f, "{:?} {}", op, self.format_value_ref(operand))
            }
            InstData::Call { func, callee, args } => {
                write!(f, "\"{}\"", self.strings.get(*func))?;
                if let Some(id) = callee {
                    write!(f, "[fn#{}]", id.raw())?;
                }
                write!(f, "(")?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", self.format_value_ref(arg))?;
                }
                write!(f, ")")
            }
            InstData::ArrayLit(elements) => {
                write!(f, "[")?;
                for (i, elem) in elements.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", self.format_value_ref(elem))?;
                }
                write!(f, "]")
            }
            InstData::TupleLit(elements) => {
                write!(f, "(")?;
                for (i, elem) in elements.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", self.format_value_ref(elem))?;
                }
                write!(f, ")")
            }
            InstData::StructLit { struct_id, fields } => {
                write!(f, "Struct#{} {{", struct_id)?;
                for (i, field) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", self.format_value_ref(field))?;
                }
                write!(f, "}}")
            }
            InstData::FieldAccess { base, field } => {
                write!(f, "{}.{}", self.format_value_ref(base), field)
            }
            InstData::IndexAccess { base, index } => {
                write!(
                    f,
                    "{}[{}]",
                    self.format_value_ref(base),
                    self.format_value_ref(index)
                )
            }
            InstData::Cast { value, target_type } => {
                write!(f, "{} as {}", self.format_value_ref(value), target_type)
            }
        }
    }

    fn format_terminator(&self, f: &mut fmt::Formatter, term: &Terminator) -> fmt::Result {
        match term {
            Terminator::Goto { target, args } => {
                write!(f, "goto block#{}", target.raw())?;
                if !args.is_empty() {
                    write!(f, "(")?;
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", self.format_value_ref(arg))?;
                    }
                    write!(f, ")")?;
                }
                Ok(())
            }
            Terminator::If {
                cond,
                then_target,
                then_args,
                else_target,
                else_args,
            } => {
                write!(
                    f,
                    "if {} then block#{}",
                    self.format_value_ref(cond),
                    then_target.raw()
                )?;
                if !then_args.is_empty() {
                    write!(f, "(")?;
                    for (i, arg) in then_args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", self.format_value_ref(arg))?;
                    }
                    write!(f, ")")?;
                }
                write!(f, " else block#{}", else_target.raw())?;
                if !else_args.is_empty() {
                    write!(f, "(")?;
                    for (i, arg) in else_args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", self.format_value_ref(arg))?;
                    }
                    write!(f, ")")?;
                }
                Ok(())
            }
            Terminator::Return { value } => {
                if let Some(val) = value {
                    write!(f, "return {}", self.format_value_ref(val))
                } else {
                    write!(f, "return")
                }
            }
            Terminator::Break { loop_header, args } => {
                write!(f, "break loop#{}", loop_header.raw())?;
                if !args.is_empty() {
                    write!(f, "(")?;
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", self.format_value_ref(arg))?;
                    }
                    write!(f, ")")?;
                }
                Ok(())
            }
            Terminator::Continue { loop_header, args } => {
                write!(f, "continue loop#{}", loop_header.raw())?;
                if !args.is_empty() {
                    write!(f, "(")?;
                    for (i, arg) in args.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{}", self.format_value_ref(arg))?;
                    }
                    write!(f, ")")?;
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_block_creation() {
        let block = Block::new_empty(BlockId::new(0), vec![], Span { start: 0, end: 0 });
        assert_eq!(block.body.len(), 0);
        assert_eq!(block.params.len(), 0);
        // Check that it has a default terminator
        assert!(matches!(block.term, Terminator::Return { value: None }));
    }

    #[test]
    fn test_string_pool() {
        let mut pool = StringPool::new();

        let hello = pool.intern("hello");
        let world = pool.intern("world");
        let hello2 = pool.intern("hello");

        assert_eq!(hello, hello2);
        assert_ne!(hello, world);
        assert_eq!(pool.get(hello), "hello");
        assert_eq!(pool.get(world), "world");
    }

    #[test]
    fn test_value_ref_creation() {
        // Test creating ValueRef from instruction index
        let inst_idx = LocalInstIndex::new(5);
        let value_ref1 = ValueRef::from_instruction(inst_idx);
        assert!(value_ref1.is_instruction());
        assert!(!value_ref1.is_block_param());
        assert_eq!(value_ref1.as_instruction(), Some(inst_idx));
        assert_eq!(value_ref1.as_block_param(), None);

        // Test creating ValueRef from block parameter index
        let param_idx = BlockParamIndex::new(3);
        let value_ref2 = ValueRef::from_block_param(param_idx);
        assert!(value_ref2.is_block_param());
        assert!(!value_ref2.is_instruction());
        assert_eq!(value_ref2.as_block_param(), Some(param_idx));
        assert_eq!(value_ref2.as_instruction(), None);

        // Test convenience methods
        let value_ref3 = ValueRef::from_inst_index(7);
        assert_eq!(value_ref3.as_instruction().unwrap().raw(), 7);

        let value_ref4 = ValueRef::from_param_index(2);
        assert_eq!(value_ref4.as_block_param().unwrap().raw(), 2);
    }

    #[test]
    fn test_value_ref_equality() {
        let inst_ref1 = ValueRef::from_inst_index(5);
        let inst_ref2 = ValueRef::from_inst_index(5);
        let inst_ref3 = ValueRef::from_inst_index(6);
        let param_ref1 = ValueRef::from_param_index(5);

        assert_eq!(inst_ref1, inst_ref2);
        assert_ne!(inst_ref1, inst_ref3);
        assert_ne!(inst_ref1, param_ref1); // Different types even with same index
    }

    #[test]
    fn test_hierarchical_structure() {
        let mut hir = Hir::new();

        // Create a simple block with a condition and conditional terminator
        let main_block_id = hir.next_block_id();
        let then_block_id = hir.next_block_id();
        let else_block_id = hir.next_block_id();

        // Main block with condition
        let mut main_block = Block::new_empty(main_block_id, vec![], Span { start: 0, end: 0 });

        // Add a literal for the condition
        let cond_inst = Instruction {
            tag: InstTag::Literal,
            data: InstData::Literal(1),
            ty: Some(RueType::Bool),
            span: Span { start: 0, end: 0 },
        };
        let cond_idx = main_block.push_instruction(cond_inst);

        // Set conditional terminator
        let cond_terminator = Terminator::If {
            cond: ValueRef::from_instruction(cond_idx),
            then_target: then_block_id,
            then_args: vec![],
            else_target: else_block_id,
            else_args: vec![],
        };
        main_block.set_terminator(cond_terminator);

        // Create then block
        let mut then_block = Block::new_empty(then_block_id, vec![], Span { start: 0, end: 0 });
        let then_inst = Instruction {
            tag: InstTag::Literal,
            data: InstData::Literal(42),
            ty: Some(RueType::I32),
            span: Span { start: 0, end: 0 },
        };
        then_block.push_instruction(then_inst);

        // Create else block
        let else_block = Block::new_empty(else_block_id, vec![], Span { start: 0, end: 0 });

        hir.add_block(main_block);
        hir.add_block(then_block);
        hir.add_block(else_block);

        // Verify the structure
        assert_eq!(hir.block_arena.len(), 3);
        let main = hir.block(main_block_id).unwrap();
        assert_eq!(main.body.len(), 1); // only the condition literal

        // Verify the conditional terminator
        if let Terminator::If {
            then_target,
            else_target,
            ..
        } = &main.term
        {
            assert_eq!(*then_target, then_block_id);
            assert_eq!(*else_target, else_block_id);
        } else {
            panic!("Expected If terminator");
        }

        // Verify then block has the instruction
        let then = hir.block(then_block_id).unwrap();
        assert_eq!(then.body.len(), 1);
    }

    #[test]
    fn test_cfg_utilities() {
        let mut hir = Hir::new();

        // Create a simple CFG: main -> then, main -> else
        let main_id = hir.next_block_id();
        let then_id = hir.next_block_id();
        let else_id = hir.next_block_id();

        // Main block with conditional terminator
        let mut main_block = Block::new_empty(main_id, vec![], Span { start: 0, end: 0 });
        let cond_inst = Instruction {
            tag: InstTag::Literal,
            data: InstData::Literal(1),
            ty: Some(RueType::Bool),
            span: Span { start: 0, end: 0 },
        };
        let cond_idx = main_block.push_instruction(cond_inst);
        main_block.set_terminator(Terminator::If {
            cond: ValueRef::from_instruction(cond_idx),
            then_target: then_id,
            then_args: vec![],
            else_target: else_id,
            else_args: vec![],
        });

        let then_block = Block::new_empty(then_id, vec![], Span { start: 0, end: 0 });
        let else_block = Block::new_empty(else_id, vec![], Span { start: 0, end: 0 });

        hir.add_block(main_block);
        hir.add_block(then_block);
        hir.add_block(else_block);

        // Test successors
        let main_successors = hir.successors(main_id);
        assert_eq!(main_successors.len(), 2);
        assert!(main_successors.contains(&then_id));
        assert!(main_successors.contains(&else_id));

        let then_successors = hir.successors(then_id);
        assert_eq!(then_successors.len(), 0); // Return terminator has no successors

        // Test predecessors
        let then_preds = hir.predecessors(then_id);
        assert_eq!(then_preds.len(), 1);
        assert!(then_preds.contains(&main_id));

        let else_preds = hir.predecessors(else_id);
        assert_eq!(else_preds.len(), 1);
        assert!(else_preds.contains(&main_id));

        // Test reachability
        assert!(hir.is_reachable_from(main_id, then_id));
        assert!(hir.is_reachable_from(main_id, else_id));
        assert!(!hir.is_reachable_from(then_id, main_id)); // No back edge
        assert!(hir.is_reachable_from(main_id, main_id)); // Self-reachable
    }

    #[test]
    fn test_while_loop_with_terminators() {
        // This test demonstrates how while loops are now structured with explicit terminators
        // A while loop becomes: preheader -> header -> body -> header (loop) | exit
        let mut hir = Hir::new();

        let preheader_id = hir.next_block_id();
        let header_id = hir.next_block_id();
        let body_id = hir.next_block_id();
        let exit_id = hir.next_block_id();

        // Preheader: sets up initial values and jumps to header
        let mut preheader = Block::new_empty(preheader_id, vec![], Span { start: 0, end: 0 });
        let init_val = Instruction {
            tag: InstTag::Literal,
            data: InstData::Literal(0), // counter = 0
            ty: Some(RueType::I32),
            span: Span { start: 0, end: 0 },
        };
        let init_idx = preheader.push_instruction(init_val);
        preheader.set_terminator(Terminator::Goto {
            target: header_id,
            args: vec![ValueRef::from_instruction(init_idx)],
        });

        // Header: receives loop-carried variables, checks condition, branches
        let header_param = BlockParam {
            name: StringIndex::from_raw(0),
            ty: RueType::I32,
        };
        let mut header = Block::new_empty(header_id, vec![header_param], Span { start: 0, end: 0 });
        let counter_ref = ValueRef::from_param_index(0);
        let limit = Instruction {
            tag: InstTag::Literal,
            data: InstData::Literal(10),
            ty: Some(RueType::I32),
            span: Span { start: 0, end: 0 },
        };
        let limit_idx = header.push_instruction(limit);
        let condition = Instruction {
            tag: InstTag::Binary,
            data: InstData::Binary {
                op: BinOp::Lt,
                lhs: counter_ref,
                rhs: ValueRef::from_instruction(limit_idx),
            },
            ty: Some(RueType::Bool),
            span: Span { start: 0, end: 0 },
        };
        let cond_idx = header.push_instruction(condition);
        header.set_terminator(Terminator::If {
            cond: ValueRef::from_instruction(cond_idx),
            then_target: body_id,
            then_args: vec![counter_ref], // Pass counter to body
            else_target: exit_id,
            else_args: vec![], // Exit with no values
        });

        // Body: does work, increments counter, jumps back to header
        let body_param = BlockParam {
            name: StringIndex::from_raw(1),
            ty: RueType::I32,
        };
        let mut body = Block::new_empty(body_id, vec![body_param], Span { start: 0, end: 0 });
        let body_counter_ref = ValueRef::from_param_index(0);
        let one = Instruction {
            tag: InstTag::Literal,
            data: InstData::Literal(1),
            ty: Some(RueType::I32),
            span: Span { start: 0, end: 0 },
        };
        let one_idx = body.push_instruction(one);
        let incremented = Instruction {
            tag: InstTag::Binary,
            data: InstData::Binary {
                op: BinOp::Add,
                lhs: body_counter_ref,
                rhs: ValueRef::from_instruction(one_idx),
            },
            ty: Some(RueType::I32),
            span: Span { start: 0, end: 0 },
        };
        let inc_idx = body.push_instruction(incremented);
        body.set_terminator(Terminator::Goto {
            target: header_id,
            args: vec![ValueRef::from_instruction(inc_idx)], // Back-edge with updated counter
        });

        // Exit: terminates the loop
        let exit = Block::new_empty(exit_id, vec![], Span { start: 0, end: 0 });

        hir.add_block(preheader);
        hir.add_block(header);
        hir.add_block(body);
        hir.add_block(exit);

        // Verify the CFG structure
        assert_eq!(hir.successors(preheader_id), vec![header_id]);
        let header_successors = hir.successors(header_id);
        assert_eq!(header_successors.len(), 2);
        assert!(header_successors.contains(&body_id));
        assert!(header_successors.contains(&exit_id));
        assert_eq!(hir.successors(body_id), vec![header_id]); // Back-edge
        assert_eq!(hir.successors(exit_id), vec![]); // No successors

        // Verify loop structure
        assert!(hir.is_reachable_from(preheader_id, body_id));
        assert!(hir.is_reachable_from(body_id, header_id)); // Back-edge exists
        assert!(hir.is_reachable_from(header_id, exit_id)); // Exit path exists

        // Verify block parameters (SSA form)
        assert_eq!(hir.get_block(header_id).unwrap().params.len(), 1); // Loop-carried variable
        assert_eq!(hir.get_block(body_id).unwrap().params.len(), 1); // Received from header
    }
}
