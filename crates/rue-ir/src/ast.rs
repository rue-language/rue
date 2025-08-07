//! Abstract Syntax Tree (AST) - Untyped intermediate representation
//!
//! This module implements a Zig-inspired ultra-compact AST representation that achieves
//! exceptional cache efficiency through struct-of-arrays layout and minimal memory overhead.
//!
//! ## Design Philosophy
//!
//! Following Zig's approach: "The AST brings structure to a flat list of tokens."
//! Every design decision prioritizes cache efficiency and traversal performance.
//!
//! ## Memory Layout
//!
//! Each node occupies exactly 16 bytes (after alignment):
//! - 1 byte: Node type tag
//! - 4 bytes: Main token/string ID
//! - 8 bytes: Two u32 fields for children/data
//! - 3 bytes: Padding for alignment
//!
//! This allows ~4 nodes per 64-byte cache line, compared to 1-2 nodes in traditional ASTs.
//!
//! ## Architecture
//!
//! The AST uses three main storage areas:
//! 1. `nodes`: Struct-of-arrays for all AST nodes
//! 2. `extra_data`: Variable-length data (function parameters, call arguments, etc.)
//! 3. `strings`: Interned string pool for identifiers
//!
//! Node references are 32-bit indices, providing:
//! - 50% memory savings over 64-bit pointers
//! - Better cache density
//! - Predictable access patterns for hardware prefetchers

use rue_lexer::Span;
use std::fmt;
use std::num::NonZeroU32;

/// Node index - wraps u32 for type safety
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeIndex(NonZeroU32);

impl NodeIndex {
    pub const NONE: u32 = 0;

    /// Create a new node index (0 is reserved for "none")
    #[inline]
    pub fn new(index: u32) -> Option<Self> {
        NonZeroU32::new(index).map(NodeIndex)
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

/// Node type tags - kept small (1 byte) for cache efficiency
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeTag {
    // Root
    Root,

    // Declarations
    Function,
    Param,
    StructDef,

    // Statements
    Block,
    Let,
    Assign,
    Return,
    ExprStmt,

    // Expressions
    Binary,
    Unary,
    Call,
    If,
    While,
    Literal,
    Identifier,
    FieldAccess,
    ArrayAccess,
    Cast,
    StructLiteral,
    ArrayLiteral,
    TupleLiteral,

    // Type annotations
    TupleType,
    ArrayType,
    StructType,

    // Special markers
    Error,
}

/// Binary operators - stored in extra_data
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
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
    And,
    Or,
}

/// Unary operators - stored in extra_data
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
}

/// Node data - two u32 fields for flexible use
#[derive(Debug, Clone, Copy)]
pub struct NodeData {
    pub lhs: u32,
    pub rhs: u32,
}

impl NodeData {
    pub const EMPTY: Self = Self { lhs: 0, rhs: 0 };

    /// Create node data with two node indices
    pub fn nodes(left: NodeIndex, right: NodeIndex) -> Self {
        Self {
            lhs: left.raw(),
            rhs: right.raw(),
        }
    }

    /// Create node data with node and extra_data index
    pub fn node_extra(node: NodeIndex, extra_index: u32) -> Self {
        Self {
            lhs: node.raw(),
            rhs: extra_index,
        }
    }

    /// Create node data with extra_data index and count
    pub fn extra_range(start: u32, count: u32) -> Self {
        Self {
            lhs: start,
            rhs: count,
        }
    }

    /// Create node data with string index
    pub fn string(index: StringIndex) -> Self {
        Self {
            lhs: index.raw(),
            rhs: 0,
        }
    }

    /// Create node data with literal value
    pub fn literal(value: u64) -> Self {
        Self {
            lhs: (value & 0xFFFFFFFF) as u32,
            rhs: (value >> 32) as u32,
        }
    }

    /// Get left node if present
    pub fn left_node(&self) -> Option<NodeIndex> {
        NodeIndex::new(self.lhs)
    }

    /// Get right node if present
    pub fn right_node(&self) -> Option<NodeIndex> {
        NodeIndex::new(self.rhs)
    }
}

/// Main AST structure with Zig-inspired layout
pub struct Ast {
    /// Node storage using struct-of-arrays
    pub nodes: NodeList,

    /// Extra data for variable-length content
    /// Layout depends on node type:
    /// - Binary: [operator_u8]
    /// - Call: [callee, arg1, arg2, ...]
    /// - Function: [name_str, param_count, param1, param2, ..., body]
    /// - StructDef: [field_count, field1_name_str, field1_type, field2_name_str, field2_type, ...]
    /// - TupleType: [element_count, element_type1, element_type2, ...]
    /// - ArrayType: [element_type, size]
    pub extra_data: Vec<u32>,

    /// Source spans parallel to nodes
    pub spans: Vec<Span>,

    /// String pool for identifiers
    pub strings: StringPool,

    /// Root node index (usually a synthetic root with children)
    pub root: NodeIndex,
}

/// Struct-of-arrays node storage for cache efficiency
pub struct NodeList {
    /// Node type tags (1 byte each)
    pub tags: Vec<NodeTag>,
    /// Main token or string index (4 bytes each)  
    pub tokens: Vec<u32>,
    /// Node data (8 bytes each)
    pub data: Vec<NodeData>,
}

impl Default for NodeList {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeList {
    pub fn new() -> Self {
        // Reserve index 0 as "null"
        Self {
            tags: vec![NodeTag::Error],
            tokens: vec![0],
            data: vec![NodeData::EMPTY],
        }
    }

    pub fn reserve(&mut self, additional: usize) {
        self.tags.reserve(additional);
        self.tokens.reserve(additional);
        self.data.reserve(additional);
    }

    pub fn add(&mut self, tag: NodeTag, token: u32, data: NodeData) -> NodeIndex {
        let index = self.tags.len() as u32;
        self.tags.push(tag);
        self.tokens.push(token);
        self.data.push(data);
        NodeIndex::new(index).expect("node index overflow")
    }

    pub fn len(&self) -> usize {
        self.tags.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    #[inline]
    pub fn tag(&self, index: NodeIndex) -> NodeTag {
        self.tags[index.as_usize()]
    }

    #[inline]
    pub fn token(&self, index: NodeIndex) -> u32 {
        self.tokens[index.as_usize()]
    }

    #[inline]
    pub fn data(&self, index: NodeIndex) -> NodeData {
        self.data[index.as_usize()]
    }
}

/// String pool for deduplication
pub struct StringPool {
    strings: Vec<String>,
    lookup: rustc_hash::FxHashMap<String, StringIndex>,
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
            lookup: rustc_hash::FxHashMap::default(),
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

    #[inline]
    pub fn get(&self, index: StringIndex) -> &str {
        &self.strings[index.as_usize()]
    }
}

impl Default for Ast {
    fn default() -> Self {
        Self::new()
    }
}

impl Ast {
    /// Create a new empty AST
    pub fn new() -> Self {
        let mut nodes = NodeList::new();
        let root = nodes.add(NodeTag::Root, 0, NodeData::EMPTY);

        Self {
            nodes,
            extra_data: Vec::with_capacity(1024),
            spans: vec![
                Span { start: 0, end: 0 }, // Placeholder for index 0
                Span { start: 0, end: 0 }, // Placeholder for root node at index 1
            ],
            strings: StringPool::new(),
            root,
        }
    }

    /// Add a node to the AST
    pub fn add_node(&mut self, tag: NodeTag, span: Span, token: u32, data: NodeData) -> NodeIndex {
        self.spans.push(span);
        self.nodes.add(tag, token, data)
    }

    /// Add extra data and return the starting index
    pub fn add_extra_data(&mut self, data: &[u32]) -> u32 {
        let start = self.extra_data.len() as u32;
        self.extra_data.extend_from_slice(data);
        start
    }

    /// Get node span
    #[inline]
    pub fn span(&self, index: NodeIndex) -> Span {
        self.spans[index.as_usize()]
    }

    /// Iterate over child nodes
    pub fn children(&self, index: NodeIndex) -> impl Iterator<Item = NodeIndex> + '_ {
        let data = self.nodes.data(index);
        let tag = self.nodes.tag(index);

        // Return an iterator based on node type
        match tag {
            NodeTag::Binary => {
                // Binary nodes store children directly
                vec![data.left_node(), data.right_node()]
                    .into_iter()
                    .flatten()
            }
            NodeTag::If => {
                // If stores: condition in lhs, then_block in rhs
                // else_block (if any) in extra_data[lhs]
                vec![data.left_node(), data.right_node()]
                    .into_iter()
                    .flatten()
            }
            _ => {
                // Most nodes don't have inline children
                vec![].into_iter().flatten()
            }
        }
    }

    /// Get extra data for a node
    pub fn extra_data(&self, index: NodeIndex) -> ExtraDataIterator<'_> {
        let data = self.nodes.data(index);
        let tag = self.nodes.tag(index);

        match tag {
            NodeTag::Call => {
                // lhs = start index, rhs = count
                ExtraDataIterator {
                    extra_data: &self.extra_data,
                    start: data.lhs as usize,
                    end: (data.lhs + data.rhs) as usize,
                }
            }
            NodeTag::Function => {
                // lhs = extra_data index for function info
                // extra_data[lhs] = param count
                // extra_data[lhs+1..] = param nodes, then body node
                let start = data.lhs as usize;
                let param_count = self.extra_data[start] as usize;
                ExtraDataIterator {
                    extra_data: &self.extra_data,
                    start: start + 1,
                    end: start + 1 + param_count + 1, // params + body
                }
            }
            NodeTag::StructDef => {
                // lhs = extra_data index, rhs = 0
                // extra_data[lhs] = field count
                // extra_data[lhs+1..] = alternating field_name_str, field_type pairs
                let start = data.lhs as usize;
                let field_count = self.extra_data[start] as usize;
                ExtraDataIterator {
                    extra_data: &self.extra_data,
                    start,
                    end: start + 1 + (field_count * 2), // count + name/type pairs
                }
            }
            NodeTag::TupleType => {
                // lhs = extra_data index, rhs = 0
                // extra_data[lhs] = element count
                // extra_data[lhs+1..] = element type nodes
                let start = data.lhs as usize;
                let element_count = self.extra_data[start] as usize;
                ExtraDataIterator {
                    extra_data: &self.extra_data,
                    start,
                    end: start + 1 + element_count, // count + elements
                }
            }
            NodeTag::ArrayType => {
                // lhs = extra_data index, rhs = 0
                // extra_data[lhs] = element_type node
                // extra_data[lhs+1] = size literal
                let start = data.lhs as usize;
                ExtraDataIterator {
                    extra_data: &self.extra_data,
                    start,
                    end: start + 2, // element_type + size
                }
            }
            _ => ExtraDataIterator {
                extra_data: &self.extra_data,
                start: 0,
                end: 0,
            },
        }
    }
}

/// Iterator over extra data
pub struct ExtraDataIterator<'a> {
    extra_data: &'a [u32],
    start: usize,
    end: usize,
}

impl<'a> Iterator for ExtraDataIterator<'a> {
    type Item = u32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.start < self.end {
            let value = self.extra_data[self.start];
            self.start += 1;
            Some(value)
        } else {
            None
        }
    }
}

impl fmt::Debug for Ast {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ast")
            .field("nodes", &self.nodes.len())
            .field("extra_data", &self.extra_data.len())
            .field("strings", &self.strings.strings.len())
            .finish()
    }
}

/// Builder for constructing AST with Zig-inspired layout
pub struct AstBuilder {
    ast: Ast,
}

impl Default for AstBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl AstBuilder {
    pub fn new() -> Self {
        Self { ast: Ast::new() }
    }

    pub fn intern_string(&mut self, s: &str) -> StringIndex {
        self.ast.strings.intern(s)
    }

    /// Add a node to the AST (forwarding method)
    pub fn add_node(&mut self, tag: NodeTag, span: Span, token: u32, data: NodeData) -> NodeIndex {
        self.ast.add_node(tag, span, token, data)
    }

    /// Add extra data to the AST (forwarding method)
    pub fn add_extra_data(&mut self, data: &[u32]) -> u32 {
        self.ast.add_extra_data(data)
    }

    /// Build a binary expression
    pub fn build_binary(
        &mut self,
        op: BinaryOp,
        left: NodeIndex,
        right: NodeIndex,
        span: Span,
    ) -> NodeIndex {
        // Store operator in extra_data
        let op_index = self.ast.add_extra_data(&[op as u32]);

        // Create node with children
        let data = NodeData {
            lhs: left.raw(),
            rhs: right.raw(),
        };

        self.ast.add_node(NodeTag::Binary, span, op_index, data)
    }

    /// Build a function call
    pub fn build_call(&mut self, callee: NodeIndex, args: Vec<NodeIndex>, span: Span) -> NodeIndex {
        // Store arguments in extra_data
        let arg_data: Vec<u32> = args.iter().map(|n| n.raw()).collect();
        let start = self.ast.add_extra_data(&arg_data);

        // Node data stores range
        let data = NodeData::extra_range(start, arg_data.len() as u32);

        self.ast.add_node(NodeTag::Call, span, callee.raw(), data)
    }

    /// Build an identifier reference
    pub fn build_identifier(&mut self, name: StringIndex, span: Span) -> NodeIndex {
        let data = NodeData::string(name);
        self.ast
            .add_node(NodeTag::Identifier, span, name.raw(), data)
    }

    /// Build a type node
    pub fn build_type(&mut self, type_name: StringIndex, span: Span) -> NodeIndex {
        let data = NodeData::string(type_name);
        // For now, we'll reuse Identifier tag for type names
        // In the future, we might want a specific Type tag
        self.ast
            .add_node(NodeTag::Identifier, span, type_name.raw(), data)
    }

    /// Build a block
    pub fn build_block(&mut self, statements: Vec<NodeIndex>, span: Span) -> NodeIndex {
        let stmt_data: Vec<u32> = statements.iter().map(|n| n.raw()).collect();
        let start = self.ast.add_extra_data(&stmt_data);

        let data = NodeData::extra_range(start, stmt_data.len() as u32);
        self.ast.add_node(NodeTag::Block, span, 0, data)
    }

    /// Build a function
    pub fn build_function(
        &mut self,
        name: StringIndex,
        params: Vec<NodeIndex>,
        return_type: Option<NodeIndex>,
        body: NodeIndex,
        span: Span,
    ) -> NodeIndex {
        // Layout in extra_data: [param_count, param1, param2, ..., return_type_flag, return_type?, body]
        let mut extra = vec![params.len() as u32];
        extra.extend(params.iter().map(|n| n.raw()));

        // Add return type info
        if let Some(ret_type) = return_type {
            extra.push(1); // Has return type
            extra.push(ret_type.raw());
        } else {
            extra.push(0); // No return type
        }

        extra.push(body.raw());

        let start = self.ast.add_extra_data(&extra);
        let data = NodeData { lhs: start, rhs: 0 };

        self.ast.add_node(NodeTag::Function, span, name.raw(), data)
    }

    /// Build a struct definition
    pub fn build_struct_def(
        &mut self,
        name: StringIndex,
        fields: Vec<(StringIndex, NodeIndex)>, // (field_name, field_type) pairs
        span: Span,
    ) -> NodeIndex {
        // Layout in extra_data: [field_count, field1_name, field1_type, field2_name, field2_type, ...]
        let mut extra = vec![fields.len() as u32];
        for (field_name, field_type) in fields {
            extra.push(field_name.raw());
            extra.push(field_type.raw());
        }

        let start = self.ast.add_extra_data(&extra);
        let data = NodeData { lhs: start, rhs: 0 };

        self.ast
            .add_node(NodeTag::StructDef, span, name.raw(), data)
    }

    /// Build a tuple type annotation
    pub fn build_tuple_type(&mut self, element_types: Vec<NodeIndex>, span: Span) -> NodeIndex {
        // Layout in extra_data: [element_count, element_type1, element_type2, ...]
        let mut extra = vec![element_types.len() as u32];
        extra.extend(element_types.iter().map(|n| n.raw()));

        let start = self.ast.add_extra_data(&extra);
        let data = NodeData { lhs: start, rhs: 0 };

        self.ast.add_node(NodeTag::TupleType, span, 0, data)
    }

    /// Build an array type annotation
    pub fn build_array_type(
        &mut self,
        element_type: NodeIndex,
        size: u32, // Size as literal value
        span: Span,
    ) -> NodeIndex {
        // Layout in extra_data: [element_type, size]
        let extra = vec![element_type.raw(), size];

        let start = self.ast.add_extra_data(&extra);
        let data = NodeData { lhs: start, rhs: 0 };

        self.ast.add_node(NodeTag::ArrayType, span, 0, data)
    }

    /// Build a struct type reference (for use in type annotations)
    pub fn build_struct_type(&mut self, struct_name: StringIndex, span: Span) -> NodeIndex {
        let data = NodeData::string(struct_name);
        self.ast
            .add_node(NodeTag::StructType, span, struct_name.raw(), data)
    }

    pub fn finish(self) -> Ast {
        self.ast
    }
}

/// Visitor for AST traversal
pub trait AstVisitor {
    fn visit_node(&mut self, ast: &Ast, index: NodeIndex) {
        let tag = ast.nodes.tag(index);

        // Visit based on node type
        match tag {
            NodeTag::Binary => {
                let data = ast.nodes.data(index);
                if let Some(left) = data.left_node() {
                    self.visit_node(ast, left);
                }
                if let Some(right) = data.right_node() {
                    self.visit_node(ast, right);
                }
            }
            NodeTag::Call => {
                // Visit callee
                let token = ast.nodes.token(index);
                if let Some(callee) = NodeIndex::new(token) {
                    self.visit_node(ast, callee);
                }
                // Visit arguments from extra_data
                for arg_index in ast.extra_data(index) {
                    if let Some(arg) = NodeIndex::new(arg_index) {
                        self.visit_node(ast, arg);
                    }
                }
            }
            NodeTag::Block => {
                // Visit statements from extra_data
                for stmt_index in ast.extra_data(index) {
                    if let Some(stmt) = NodeIndex::new(stmt_index) {
                        self.visit_node(ast, stmt);
                    }
                }
            }
            NodeTag::StructDef => {
                // Visit field types from extra_data (skip field names which are string indices)
                let mut iter = ast.extra_data(index);
                let _field_count = iter.next(); // Skip field count
                while let Some(_field_name) = iter.next() {
                    if let Some(field_type_idx) = iter.next()
                        && let Some(field_type) = NodeIndex::new(field_type_idx)
                    {
                        self.visit_node(ast, field_type);
                    }
                }
            }
            NodeTag::TupleType => {
                // Visit element types from extra_data
                let mut iter = ast.extra_data(index);
                let _element_count = iter.next(); // Skip element count
                for element_type_idx in iter {
                    if let Some(element_type) = NodeIndex::new(element_type_idx) {
                        self.visit_node(ast, element_type);
                    }
                }
            }
            NodeTag::ArrayType => {
                // Visit element type from extra_data
                let mut iter = ast.extra_data(index);
                if let Some(element_type_idx) = iter.next()
                    && let Some(element_type) = NodeIndex::new(element_type_idx)
                {
                    self.visit_node(ast, element_type);
                }
                // Size is a literal, no need to visit
            }
            _ => {}
        }
    }
}

/// Fast filtering iterator - leverages SoA layout
pub struct FilteredNodes<'a> {
    ast: &'a Ast,
    current: usize,
    end: usize,
    tag_filter: NodeTag,
}

impl<'a> FilteredNodes<'a> {
    pub fn new(ast: &'a Ast, tag: NodeTag) -> Self {
        Self {
            ast,
            current: 1, // Skip index 0 (null)
            end: ast.nodes.len(),
            tag_filter: tag,
        }
    }
}

impl<'a> Iterator for FilteredNodes<'a> {
    type Item = NodeIndex;

    fn next(&mut self) -> Option<Self::Item> {
        // This is where SoA shines - we only touch the tags array!
        while self.current < self.end {
            let index = self.current;
            self.current += 1;

            if self.ast.nodes.tags[index] == self.tag_filter {
                return NodeIndex::new(index as u32);
            }
        }
        None
    }
}

impl Ast {
    /// Efficiently iterate over all nodes of a specific type
    pub fn nodes_with_tag(&self, tag: NodeTag) -> FilteredNodes<'_> {
        FilteredNodes::new(self, tag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_size() {
        // Verify our compact representation
        assert_eq!(std::mem::size_of::<NodeTag>(), 1);
        assert_eq!(std::mem::size_of::<NodeData>(), 8);
        // Total per node: 1 + 4 + 8 = 13 bytes (16 with alignment)
    }

    #[test]
    fn test_string_interning() {
        let mut pool = StringPool::new();
        let s1 = pool.intern("hello");
        let s2 = pool.intern("world");
        let s3 = pool.intern("hello");

        assert_eq!(s1, s3);
        assert_ne!(s1, s2);
        assert_eq!(pool.get(s1), "hello");
    }

    #[test]
    fn test_ast_building() {
        let mut builder = AstBuilder::new();

        let x = builder.intern_string("x");
        let x_node = builder.build_identifier(x, Span { start: 0, end: 1 });

        let y = builder.intern_string("y");
        let y_node = builder.build_identifier(y, Span { start: 2, end: 3 });

        let add = builder.build_binary(BinaryOp::Add, x_node, y_node, Span { start: 0, end: 3 });

        let ast = builder.finish();

        assert_eq!(ast.nodes.tag(add), NodeTag::Binary);
        assert_eq!(ast.nodes.tag(x_node), NodeTag::Identifier);
    }

    #[test]
    fn test_struct_definition() {
        let mut builder = AstBuilder::new();

        // Build struct Point { x: i32, y: i32 }
        let point_name = builder.intern_string("Point");
        let x_name = builder.intern_string("x");
        let y_name = builder.intern_string("y");
        let i32_name = builder.intern_string("i32");

        // Create i32 type nodes
        let i32_type1 = builder.build_identifier(i32_name, Span { start: 10, end: 13 });
        let i32_type2 = builder.build_identifier(i32_name, Span { start: 18, end: 21 });

        // Create struct definition
        let fields = vec![(x_name, i32_type1), (y_name, i32_type2)];
        let struct_def = builder.build_struct_def(point_name, fields, Span { start: 0, end: 22 });

        let ast = builder.finish();

        // Verify the struct definition node
        assert_eq!(ast.nodes.tag(struct_def), NodeTag::StructDef);

        // Verify extra_data contains field information
        let mut extra_iter = ast.extra_data(struct_def);
        assert_eq!(extra_iter.next(), Some(2)); // 2 fields
        assert_eq!(extra_iter.next(), Some(x_name.raw())); // x field name
        assert_eq!(extra_iter.next(), Some(i32_type1.raw())); // x field type
        assert_eq!(extra_iter.next(), Some(y_name.raw())); // y field name  
        assert_eq!(extra_iter.next(), Some(i32_type2.raw())); // y field type
        assert_eq!(extra_iter.next(), None); // end of extra data
    }

    #[test]
    fn test_tuple_type() {
        let mut builder = AstBuilder::new();

        // Build tuple type (i32, bool, i64)
        let i32_name = builder.intern_string("i32");
        let bool_name = builder.intern_string("bool");
        let i64_name = builder.intern_string("i64");

        let i32_type = builder.build_identifier(i32_name, Span { start: 1, end: 4 });
        let bool_type = builder.build_identifier(bool_name, Span { start: 6, end: 10 });
        let i64_type = builder.build_identifier(i64_name, Span { start: 12, end: 15 });

        let tuple_type = builder.build_tuple_type(
            vec![i32_type, bool_type, i64_type],
            Span { start: 0, end: 16 },
        );

        let ast = builder.finish();

        // Verify the tuple type node
        assert_eq!(ast.nodes.tag(tuple_type), NodeTag::TupleType);

        // Verify extra_data contains element types
        let mut extra_iter = ast.extra_data(tuple_type);
        assert_eq!(extra_iter.next(), Some(3)); // 3 elements
        assert_eq!(extra_iter.next(), Some(i32_type.raw()));
        assert_eq!(extra_iter.next(), Some(bool_type.raw()));
        assert_eq!(extra_iter.next(), Some(i64_type.raw()));
        assert_eq!(extra_iter.next(), None);
    }

    #[test]
    fn test_array_type() {
        let mut builder = AstBuilder::new();

        // Build array type [i32; 10]
        let i32_name = builder.intern_string("i32");
        let i32_type = builder.build_identifier(i32_name, Span { start: 1, end: 4 });

        let array_type = builder.build_array_type(i32_type, 10, Span { start: 0, end: 9 });

        let ast = builder.finish();

        // Verify the array type node
        assert_eq!(ast.nodes.tag(array_type), NodeTag::ArrayType);

        // Verify extra_data contains element type and size
        let mut extra_iter = ast.extra_data(array_type);
        assert_eq!(extra_iter.next(), Some(i32_type.raw())); // element type
        assert_eq!(extra_iter.next(), Some(10)); // array size
        assert_eq!(extra_iter.next(), None);
    }

    #[test]
    fn test_struct_type_reference() {
        let mut builder = AstBuilder::new();

        // Build struct type reference Point (for type annotations)
        let point_name = builder.intern_string("Point");
        let struct_type = builder.build_struct_type(point_name, Span { start: 0, end: 5 });

        let ast = builder.finish();

        // Verify the struct type reference node
        assert_eq!(ast.nodes.tag(struct_type), NodeTag::StructType);
        let data = ast.nodes.data(struct_type);
        assert_eq!(data.lhs, point_name.raw());
        assert_eq!(data.rhs, 0);
    }

    #[test]
    fn test_comprehensive_type_system() {
        let mut builder = AstBuilder::new();

        // Build a complex example demonstrating all new type features:
        // struct Person {
        //     name: String,
        //     age: i32,
        //     coordinates: (i32, i32),
        //     scores: [i32; 5],
        // }

        // Create string identifiers
        let person_name = builder.intern_string("Person");
        let name_field = builder.intern_string("name");
        let age_field = builder.intern_string("age");
        let coordinates_field = builder.intern_string("coordinates");
        let scores_field = builder.intern_string("scores");
        let string_type_name = builder.intern_string("String");
        let i32_type_name = builder.intern_string("i32");

        // Build basic type nodes
        let string_type = builder.build_identifier(string_type_name, Span { start: 0, end: 6 });
        let i32_type1 = builder.build_identifier(i32_type_name, Span { start: 0, end: 3 });
        let i32_type2 = builder.build_identifier(i32_type_name, Span { start: 0, end: 3 });
        let i32_type3 = builder.build_identifier(i32_type_name, Span { start: 0, end: 3 });
        let i32_type4 = builder.build_identifier(i32_type_name, Span { start: 0, end: 3 });

        // Build tuple type: (i32, i32)
        let tuple_type =
            builder.build_tuple_type(vec![i32_type2, i32_type3], Span { start: 0, end: 10 });

        // Build array type: [i32; 5]
        let array_type = builder.build_array_type(i32_type4, 5, Span { start: 0, end: 8 });

        // Build struct definition
        let fields = vec![
            (name_field, string_type),
            (age_field, i32_type1),
            (coordinates_field, tuple_type),
            (scores_field, array_type),
        ];
        let struct_def = builder.build_struct_def(person_name, fields, Span { start: 0, end: 100 });

        // Build a struct type reference for use in other contexts
        let person_type_ref = builder.build_struct_type(person_name, Span { start: 0, end: 6 });

        let ast = builder.finish();

        // Verify struct definition
        assert_eq!(ast.nodes.tag(struct_def), NodeTag::StructDef);
        assert_eq!(ast.nodes.tag(tuple_type), NodeTag::TupleType);
        assert_eq!(ast.nodes.tag(array_type), NodeTag::ArrayType);
        assert_eq!(ast.nodes.tag(person_type_ref), NodeTag::StructType);

        // Verify struct has 4 fields
        let mut struct_extra = ast.extra_data(struct_def);
        assert_eq!(struct_extra.next(), Some(4)); // 4 fields

        // Verify the tuple type has 2 elements
        let mut tuple_extra = ast.extra_data(tuple_type);
        assert_eq!(tuple_extra.next(), Some(2)); // 2 elements

        // Verify the array type has correct element type and size
        let mut array_extra = ast.extra_data(array_type);
        array_extra.next(); // element type
        assert_eq!(array_extra.next(), Some(5)); // size = 5

        // Test that all nodes maintain the 16-byte size constraint
        assert_eq!(std::mem::size_of::<NodeTag>(), 1);
        assert_eq!(std::mem::size_of::<NodeData>(), 8);
        // Total node size: 1 + 4 + 8 = 13 bytes (16 with alignment)
    }

    #[test]
    fn test_empty_tuple_and_zero_array() {
        let mut builder = AstBuilder::new();

        // Test empty tuple type: ()
        let empty_tuple = builder.build_tuple_type(vec![], Span { start: 0, end: 2 });

        // Test zero-length array: [i32; 0]
        let i32_name = builder.intern_string("i32");
        let i32_type = builder.build_identifier(i32_name, Span { start: 0, end: 3 });
        let zero_array = builder.build_array_type(i32_type, 0, Span { start: 0, end: 8 });

        let ast = builder.finish();

        // Verify empty tuple
        assert_eq!(ast.nodes.tag(empty_tuple), NodeTag::TupleType);
        let mut empty_tuple_extra = ast.extra_data(empty_tuple);
        assert_eq!(empty_tuple_extra.next(), Some(0)); // 0 elements
        assert_eq!(empty_tuple_extra.next(), None);

        // Verify zero-length array
        assert_eq!(ast.nodes.tag(zero_array), NodeTag::ArrayType);
        let mut zero_array_extra = ast.extra_data(zero_array);
        zero_array_extra.next(); // element type
        assert_eq!(zero_array_extra.next(), Some(0)); // size = 0
    }
}
