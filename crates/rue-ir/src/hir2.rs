//! HIR2 - Data-Driven High-level Intermediate Representation
//!
//! HIR2 transforms the tree-based HIR into an instruction stream inspired by Zig's ZIR.
//! This provides better cache efficiency, simpler optimization passes, and easier traversal
//! compared to the traditional tree representation.
//!
//! ## Design Philosophy
//!
//! Following Zig's approach: "Everything is instructions, not trees."
//! The HIR2 represents a program as a flat sequence of typed instructions that can be
//! processed sequentially rather than through recursive tree traversal.
//!
//! ## Memory Layout
//!
//! The instruction format uses struct-of-arrays for maximum cache efficiency:
//! - `tags`: Instruction type (1 byte each)
//! - `data`: Instruction data (8 bytes each, two u32 fields)
//! - `types`: Type information (parallel array, Option<RueType>)
//! - `spans`: Source location information (parallel array)
//! - `extra_data`: Variable-length data for complex instructions
//!
//! This layout allows ~7 instructions per cache line and enables vectorized processing.
//!
//! ## Instruction Encoding
//!
//! Each instruction has:
//! - 1-byte tag identifying the instruction type
//! - 8-byte data containing two u32 operands (indices/values)
//! - Optional type information (None for statements)
//! - Source span for debugging
//! - Extra data for complex operations (stored separately)

use crate::types::RueType;
use rue_lexer::Span;
use std::fmt;
use std::num::NonZeroU32;

/// Instruction index - wraps u32 for type safety with 0 reserved as "none"
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstIndex(NonZeroU32);

impl InstIndex {
    pub const NONE: u32 = 0;

    /// Create a new instruction index (0 is reserved for "none")
    #[inline]
    pub fn new(index: u32) -> Option<Self> {
        NonZeroU32::new(index).map(InstIndex)
    }

    /// Get the raw index value
    #[inline]
    pub fn raw(self) -> u32 {
        self.0.get()
    }

    /// Get as array index
    #[inline]
    pub fn as_usize(self) -> usize {
        self.0.get() as usize
    }
}

/// Block index - identifies a block of instructions
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockIndex(u32);

impl BlockIndex {
    pub const NONE: u32 = 0;

    #[inline]
    pub fn new(index: u32) -> Option<Self> {
        if index == 0 {
            None
        } else {
            Some(BlockIndex(index))
        }
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

/// Instruction tags - kept small (1 byte) for cache efficiency
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstTag {
    // Declaration instructions
    Function,
    Param,

    // Block instructions
    Block,
    BlockParam,

    // Statement instructions
    Let,
    Assign,
    Return,
    While,
    Break,
    Continue,

    // Expression instructions
    Literal,
    Load,
    Binary,
    Unary,
    Call,
    Cast,
    If,

    // Aggregate instructions
    StructLit,
    TupleLit,
    ArrayLit,
    FieldAccess,
    IndexAccess,
}

/// Binary operators - stored in extra_data
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

/// Unary operators - stored in extra_data
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

/// Instruction data - two u32 fields for flexible use
#[derive(Debug, Clone, Copy)]
pub struct InstData {
    pub lhs: u32, // First operand (index/value)
    pub rhs: u32, // Second operand (index/value)
}

impl InstData {
    pub const EMPTY: Self = Self { lhs: 0, rhs: 0 };

    /// Create instruction data with two instruction indices
    pub fn insts(left: InstIndex, right: InstIndex) -> Self {
        Self {
            lhs: left.raw(),
            rhs: right.raw(),
        }
    }

    /// Create instruction data with instruction and extra_data index
    pub fn inst_extra(inst: InstIndex, extra_index: u32) -> Self {
        Self {
            lhs: inst.raw(),
            rhs: extra_index,
        }
    }

    /// Create instruction data with extra_data index and count
    pub fn extra_range(start: u32, count: u32) -> Self {
        Self {
            lhs: start,
            rhs: count,
        }
    }

    /// Create instruction data with string index
    pub fn string(index: StringIndex) -> Self {
        Self {
            lhs: index.raw(),
            rhs: 0,
        }
    }

    /// Create instruction data with literal value
    pub fn literal(value: u64) -> Self {
        Self {
            lhs: (value & 0xFFFFFFFF) as u32,
            rhs: (value >> 32) as u32,
        }
    }

    /// Get left instruction if present
    pub fn left_inst(&self) -> Option<InstIndex> {
        InstIndex::new(self.lhs)
    }

    /// Get right instruction if present
    pub fn right_inst(&self) -> Option<InstIndex> {
        InstIndex::new(self.rhs)
    }
}

/// Block data - describes a range of instructions
#[derive(Debug, Clone, Copy)]
pub struct BlockData {
    pub first_inst: u32, // Index of first instruction in block
    pub inst_count: u32, // Number of instructions in block
}

/// Function data - metadata for function definitions
#[derive(Debug, Clone)]
pub struct FunctionData {
    pub name: StringIndex,
    pub param_count: u32,
    pub return_type: Option<RueType>,
    pub body_block: BlockIndex,
}

/// Struct-of-arrays instruction storage for cache efficiency
pub struct InstructionList {
    /// Instruction type tags (1 byte each)
    pub tags: Vec<InstTag>,
    /// Instruction data (8 bytes each)
    pub data: Vec<InstData>,
}

impl Default for InstructionList {
    fn default() -> Self {
        Self::new()
    }
}

impl InstructionList {
    pub fn new() -> Self {
        // Reserve index 0 as "null"
        Self {
            tags: vec![InstTag::Literal], // Placeholder
            data: vec![InstData::EMPTY],
        }
    }

    pub fn reserve(&mut self, additional: usize) {
        self.tags.reserve(additional);
        self.data.reserve(additional);
    }

    pub fn add(&mut self, tag: InstTag, data: InstData) -> InstIndex {
        let index = self.tags.len() as u32;
        self.tags.push(tag);
        self.data.push(data);
        InstIndex::new(index).expect("instruction index overflow")
    }

    pub fn len(&self) -> usize {
        self.tags.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    #[inline]
    pub fn tag(&self, index: InstIndex) -> InstTag {
        self.tags[index.as_usize()]
    }

    #[inline]
    pub fn data(&self, index: InstIndex) -> InstData {
        self.data[index.as_usize()]
    }
}

/// String pool for deduplication
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
        if let Some(&index) = self.lookup.get(s) {
            return index;
        }
        let index = StringIndex(self.strings.len() as u32);
        self.strings.push(s.to_string());
        self.lookup.insert(s.to_string(), index);
        index
    }

    pub fn lookup(&self, s: &str) -> Option<StringIndex> {
        self.lookup.get(s).copied()
    }

    #[inline]
    pub fn get(&self, index: StringIndex) -> &str {
        &self.strings[index.as_usize()]
    }

    /// Get the number of interned strings
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

/// Main HIR2 structure with instruction-based layout
pub struct Hir {
    /// Instruction storage using struct-of-arrays
    pub instructions: InstructionList,

    /// Extra data for variable-length content
    /// Layout depends on instruction type:
    /// - Binary: [operator_u8]
    /// - Call: [arg_count, arg1, arg2, ...]
    /// - Function: [param1_name, param1_type, param2_name, param2_type, ...]
    pub extra_data: Vec<u32>,

    /// Types parallel to instructions (None for statements)
    pub types: Vec<Option<RueType>>,

    /// Source spans parallel to instructions
    pub spans: Vec<Span>,

    /// Block storage
    pub blocks: Vec<BlockData>,

    /// Function storage
    pub functions: Vec<FunctionData>,

    /// String pool for identifiers
    pub strings: StringPool,

    /// Main function index (entry point)
    pub main_function: Option<InstIndex>,
}

impl Default for Hir {
    fn default() -> Self {
        Self::new()
    }
}

impl Hir {
    /// Create a new empty HIR
    pub fn new() -> Self {
        let instructions = InstructionList::new();

        Self {
            instructions,
            extra_data: Vec::with_capacity(1024),
            types: vec![None],                      // Placeholder for index 0
            spans: vec![Span { start: 0, end: 0 }], // Placeholder for index 0
            blocks: Vec::with_capacity(64),
            functions: Vec::with_capacity(16),
            strings: StringPool::new(),
            main_function: None,
        }
    }

    /// Add an instruction to the HIR
    pub fn add_instruction(
        &mut self,
        tag: InstTag,
        data: InstData,
        ty: Option<RueType>,
        span: Span,
    ) -> InstIndex {
        let index = self.instructions.add(tag, data);
        self.types.push(ty);
        self.spans.push(span);
        index
    }

    /// Add extra data and return the starting index
    pub fn add_extra_data(&mut self, data: &[u32]) -> u32 {
        let start = self.extra_data.len() as u32;
        self.extra_data.extend_from_slice(data);
        start
    }

    /// Add a block and return its index
    pub fn add_block(&mut self, first_inst: u32, inst_count: u32) -> BlockIndex {
        let index = self.blocks.len() as u32;
        self.blocks.push(BlockData {
            first_inst,
            inst_count,
        });
        BlockIndex::new(index + 1).unwrap() // +1 because 0 is reserved
    }

    /// Add a function and return its index
    pub fn add_function(&mut self, func_data: FunctionData) -> InstIndex {
        let index = self.functions.len();
        self.functions.push(func_data);

        // For now, functions are not stored as instructions
        // This is a simplification - in full implementation, functions would be instructions too
        InstIndex::new((index + 1) as u32).unwrap()
    }

    /// Get instruction type
    #[inline]
    pub fn get_type(&self, inst: InstIndex) -> Option<&RueType> {
        self.types.get(inst.as_usize()).and_then(|t| t.as_ref())
    }

    /// Get instruction span
    #[inline]
    pub fn get_span(&self, inst: InstIndex) -> Span {
        self.spans[inst.as_usize()]
    }

    /// Walk over all instructions in a block
    pub fn walk_block<F>(&self, block: BlockIndex, mut f: F)
    where
        F: FnMut(InstIndex, InstTag, InstData),
    {
        let block_data = &self.blocks[block.as_usize() - 1]; // -1 because 0 is reserved
        let start = block_data.first_inst as usize;
        let end = start + block_data.inst_count as usize;

        for i in start..end {
            if let Some(inst_index) = InstIndex::new(i as u32) {
                let tag = self.instructions.tag(inst_index);
                let data = self.instructions.data(inst_index);
                f(inst_index, tag, data);
            }
        }
    }

    /// Find a function by name
    pub fn find_function(&self, name: &str) -> Option<usize> {
        let name_idx = self.strings.lookup(name)?;

        self.functions.iter().position(|func| func.name == name_idx)
    }

    /// Get function data
    pub fn get_function(&self, index: usize) -> Option<&FunctionData> {
        self.functions.get(index)
    }
}

impl fmt::Debug for Hir {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hir")
            .field("instructions", &self.instructions.len())
            .field("extra_data", &self.extra_data.len())
            .field("blocks", &self.blocks.len())
            .field("functions", &self.functions.len())
            .field("strings", &self.strings.strings.len())
            .finish()
    }
}

/// Pretty printing for debugging
impl fmt::Display for Hir {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "HIR2 Instructions:")?;

        for i in 1..self.instructions.len() {
            // Skip index 0
            if let Some(inst_index) = InstIndex::new(i as u32) {
                let tag = self.instructions.tag(inst_index);
                let data = self.instructions.data(inst_index);
                let ty = self.get_type(inst_index);

                write!(f, "{:04}: {:?} ", i, tag)?;
                self.format_instruction(f, inst_index, tag, data)?;

                if let Some(ty) = ty {
                    write!(f, " : {}", ty)?;
                }
                writeln!(f)?;
            }
        }

        Ok(())
    }
}

impl Hir {
    fn format_instruction(
        &self,
        f: &mut fmt::Formatter,
        _index: InstIndex,
        tag: InstTag,
        data: InstData,
    ) -> fmt::Result {
        match tag {
            InstTag::Function => {
                let name_idx = StringIndex::from_raw(data.lhs);
                let name = self.strings.get(name_idx);
                write!(f, "\"{}\" -> Block({})", name, data.rhs)
            }
            InstTag::Block => {
                write!(f, "[{}..{}]", data.lhs, data.lhs + data.rhs)
            }
            InstTag::Let => {
                let name_idx = StringIndex::from_raw(data.lhs);
                let name = self.strings.get(name_idx);
                if let Some(init_inst) = InstIndex::new(data.rhs) {
                    write!(f, "\"{}\" = Inst({})", name, init_inst.raw())
                } else {
                    write!(f, "\"{}\" = NONE", name)
                }
            }
            InstTag::Literal => {
                let value = ((data.rhs as u64) << 32) | (data.lhs as u64);
                write!(f, "{}", value)
            }
            InstTag::Load => {
                let name_idx = StringIndex::from_raw(data.lhs);
                let name = self.strings.get(name_idx);
                write!(f, "\"{}\"", name)
            }
            InstTag::Binary => {
                if let (Some(lhs), Some(rhs)) = (InstIndex::new(data.lhs), InstIndex::new(data.rhs))
                {
                    write!(f, "Inst({}) op Inst({})", lhs.raw(), rhs.raw())
                } else {
                    write!(f, "INVALID BINARY")
                }
            }
            InstTag::Call => {
                let func_idx = StringIndex::from_raw(data.lhs);
                let func_name = self.strings.get(func_idx);
                write!(f, "\"{}\"(...)", func_name)
            }
            InstTag::Return => {
                if let Some(value_inst) = InstIndex::new(data.lhs) {
                    write!(f, "Inst({})", value_inst.raw())
                } else {
                    write!(f, "NONE")
                }
            }
            _ => {
                write!(f, "({}, {})", data.lhs, data.rhs)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instruction_index() {
        assert_eq!(InstIndex::new(0), None);
        assert!(InstIndex::new(1).is_some());

        let idx = InstIndex::new(42).unwrap();
        assert_eq!(idx.raw(), 42);
        assert_eq!(idx.as_usize(), 42);
    }

    #[test]
    fn test_instruction_list() {
        let mut list = InstructionList::new();
        assert_eq!(list.len(), 1); // Index 0 is reserved

        let idx = list.add(InstTag::Literal, InstData::literal(42));
        assert_eq!(idx.raw(), 1);
        assert_eq!(list.tag(idx), InstTag::Literal);
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
    fn test_hir_creation() {
        let hir = Hir::new();
        assert_eq!(hir.instructions.len(), 1); // Index 0 reserved
        assert_eq!(hir.types.len(), 1);
        assert_eq!(hir.spans.len(), 1);
    }
}
