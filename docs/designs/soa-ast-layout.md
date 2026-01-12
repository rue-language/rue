# SOA AST Layout Design

## Overview

This document describes the Struct-of-Arrays (SOA) layout for Rue's AST, inspired by Zig's approach (PR #7920).

## Motivation

The current AST uses a traditional tree structure with `Box<Expr>` for recursive nodes. This causes:
- Many small heap allocations (one per node)
- Poor cache locality during traversal
- Deep cloning required for `--emit ast`
- Memory fragmentation

The SOA layout provides:
- Single allocation for entire AST
- Better cache locality (sequential arrays)
- Cheap cloning (Arc wrapper around arrays)
- Index-based references (like RIR)

## Design Principles

Following Zig's proven design:

1. **Fixed-size nodes**: Each node is a fixed-size structure (tag + main_token + lhs + rhs)
2. **Parallel arrays**: Node data stored in separate arrays (tags, data, extra)
3. **Index-based references**: Nodes reference children by u32 index, not pointers
4. **Extra data array**: Nodes with >2 children store additional indices in extra_data
5. **Token storage**: Tokens stored separately for efficient re-tokenization

## Core Structures

```rust
/// Node index - references a node in the AST
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeIndex(pub u32);

/// Special node index representing "null" or "no node"
pub const NULL_NODE: NodeIndex = NodeIndex(u32::MAX);

/// The SOA-based AST structure
pub struct Ast {
    /// Node tags (what kind of node)
    tags: Vec<NodeTag>,

    /// Node data (main_token + lhs + rhs indices)
    data: Vec<NodeData>,

    /// Extra data for nodes with >2 children
    /// Stores additional NodeIndex values and other data
    extra: Vec<u32>,

    /// Root nodes (top-level items)
    items: Vec<NodeIndex>,
}

/// Fixed-size node data (12 bytes)
#[derive(Debug, Clone, Copy)]
pub struct NodeData {
    /// Primary token for this node (for spans and error reporting)
    pub main_token: u32,

    /// Left child or first data slot
    /// Interpretation depends on NodeTag
    pub lhs: u32,

    /// Right child or second data slot
    /// Interpretation depends on NodeTag
    pub rhs: u32,
}
```

## Node Tag Categories

Nodes are categorized by how they use lhs/rhs:

### Category A: Simple Literals (0 children)
- `main_token`: the literal token
- `lhs`: value data (for integers) or string index
- `rhs`: unused (0)

Examples: `IntLit`, `BoolLit`, `StringLit`, `UnitLit`, `Ident`

### Category B: Unary Nodes (1 child)
- `main_token`: the operator token
- `lhs`: child node index
- `rhs`: unused (0) or flags

Examples: `UnaryExpr`, `ParenExpr`, `ReturnExpr`, `BreakExpr`, `ComptimeBlockExpr`

### Category C: Binary Nodes (2 children)
- `main_token`: the operator token or primary keyword
- `lhs`: left child node index
- `rhs`: right child node index

Examples: `BinaryExpr`, `FieldExpr`, `WhileExpr`, `AssignStmt`

### Category D: Multi-child Nodes (3+ children)
- `main_token`: primary token
- `lhs`: first child or extra_data index
- `rhs`: second child or count

Examples: `BlockExpr`, `CallExpr`, `StructLitExpr`, `IfExpr`, `FunctionDecl`

## Node Tag Enum

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeTag {
    // ===== Items (top-level declarations) =====
    Function,           // lhs=extra(params), rhs=body_expr
    StructDecl,         // lhs=extra(fields), rhs=extra(methods)
    EnumDecl,           // lhs=extra(variants), rhs=0
    DropFn,             // lhs=type_name, rhs=body_expr
    ConstDecl,          // lhs=type_expr|NULL, rhs=init_expr

    // ===== Expressions =====
    // Literals
    IntLit,             // lhs=value_lo, rhs=value_hi (u64 split into two u32s)
    StringLit,          // lhs=string_index, rhs=0
    BoolLit,            // lhs=0|1, rhs=0
    UnitLit,            // lhs=0, rhs=0

    // Identifiers and paths
    Ident,              // lhs=name_spur, rhs=0
    Path,               // lhs=type_name, rhs=variant_name (EnumVariant::Foo)

    // Unary operations
    UnaryExpr,          // lhs=operand, rhs=op_kind
    ParenExpr,          // lhs=inner_expr, rhs=0

    // Binary operations
    BinaryExpr,         // lhs=left_expr, rhs=right_expr, main_token=operator

    // Control flow
    IfExpr,             // lhs=cond_expr, rhs=extra(then_block, else_block?)
    MatchExpr,          // lhs=scrutinee, rhs=extra(arms)
    WhileExpr,          // lhs=cond_expr, rhs=body_block
    LoopExpr,           // lhs=body_block, rhs=0
    BreakExpr,          // lhs=0, rhs=0
    ContinueExpr,       // lhs=0, rhs=0
    ReturnExpr,         // lhs=value_expr|NULL, rhs=0

    // Blocks and statements
    BlockExpr,          // lhs=extra(statements), rhs=final_expr
    LetStmt,            // lhs=pattern, rhs=init_expr, extra(flags, type?)
    AssignStmt,         // lhs=target, rhs=value_expr
    ExprStmt,           // lhs=expr, rhs=0

    // Function calls
    Call,               // lhs=callee_name, rhs=extra(args)
    MethodCall,         // lhs=receiver, rhs=extra(method_name, args)
    IntrinsicCall,      // lhs=intrinsic_name, rhs=extra(args)
    AssocFnCall,        // lhs=type_name, rhs=extra(fn_name, args)

    // Struct operations
    StructLit,          // lhs=struct_name, rhs=extra(field_inits)
    FieldExpr,          // lhs=base_expr, rhs=field_name
    FieldInit,          // lhs=field_name, rhs=value_expr

    // Array operations
    ArrayLit,           // lhs=extra(elements), rhs=element_count
    IndexExpr,          // lhs=base_expr, rhs=index_expr

    // Special
    SelfExpr,           // lhs=0, rhs=0
    ComptimeBlockExpr,  // lhs=inner_expr, rhs=0
    CheckedBlockExpr,   // lhs=inner_expr, rhs=0
    TypeLit,            // lhs=type_expr, rhs=0

    // ===== Type Expressions =====
    TypeNamed,          // lhs=name, rhs=0
    TypeUnit,           // lhs=0, rhs=0
    TypeNever,          // lhs=0, rhs=0
    TypeArray,          // lhs=element_type, rhs=length
    TypeAnonStruct,     // lhs=extra(fields), rhs=extra(methods)
    TypePointerConst,   // lhs=pointee_type, rhs=0
    TypePointerMut,     // lhs=pointee_type, rhs=0

    // ===== Patterns =====
    PatternWildcard,    // lhs=0, rhs=0
    PatternInt,         // lhs=value_lo, rhs=value_hi
    PatternBool,        // lhs=0|1, rhs=0
    PatternPath,        // lhs=type_name, rhs=variant_name

    // ===== Other Nodes =====
    Param,              // lhs=name, rhs=type_expr, extra(flags)
    Method,             // lhs=extra(params), rhs=body_expr, extra(name, return_type?)
    MatchArm,           // lhs=pattern, rhs=body_expr
    CallArg,            // lhs=expr, rhs=flags (normal/inout/borrow)

    // Error recovery
    ErrorNode,          // lhs=0, rhs=0
}
```

## Extra Data Encoding

The `extra` array stores variable-length data. Each multi-child node documents its extra_data layout:

### BlockExpr
```
extra[lhs..lhs+N]: statement node indices
rhs: final expression node index
```

### CallExpr / MethodCall
```
extra[rhs]: arg_count
extra[rhs+1..rhs+1+arg_count]: CallArg node indices
```

### IfExpr
```
extra[rhs]: then_block node index
extra[rhs+1]: else_block node index (or NULL_NODE)
```

### FunctionDecl
```
extra[lhs]: param_count
extra[lhs+1..lhs+1+param_count]: Param node indices
extra[lhs+1+param_count]: return_type node index (or NULL_NODE)
extra[lhs+2+param_count]: directive_count
extra[lhs+3+param_count..]: Directive data
```

### StructDecl
```
extra[lhs]: field_count
extra[lhs+1..lhs+1+field_count]: FieldDecl node indices
extra[rhs]: method_count
extra[rhs+1..rhs+1+method_count]: Method node indices
```

## Token Storage

Tokens are stored separately and referenced by index (main_token field):

```rust
pub struct TokenData {
    /// Token kind (u8 fits in single byte)
    pub tag: TokenTag,
    /// Byte offset in source (u32 = 4GB max file size)
    pub start: u32,
}
```

Tokens can be cheaply re-tokenized to get their end position, so we only store the start offset (Zig's approach).

## Access Patterns

### Building the AST (Parser)

```rust
struct AstBuilder {
    ast: Ast,
}

impl AstBuilder {
    fn add_int_lit(&mut self, token: u32, value: u64) -> NodeIndex {
        let idx = NodeIndex(self.ast.tags.len() as u32);
        self.ast.tags.push(NodeTag::IntLit);
        self.ast.data.push(NodeData {
            main_token: token,
            lhs: (value & 0xFFFFFFFF) as u32,        // low 32 bits
            rhs: ((value >> 32) & 0xFFFFFFFF) as u32, // high 32 bits
        });
        idx
    }

    fn add_binary_expr(&mut self, token: u32, left: NodeIndex, right: NodeIndex) -> NodeIndex {
        let idx = NodeIndex(self.ast.tags.len() as u32);
        self.ast.tags.push(NodeTag::BinaryExpr);
        self.ast.data.push(NodeData {
            main_token: token,
            lhs: left.0,
            rhs: right.0,
        });
        idx
    }

    fn add_block_expr(&mut self, token: u32, stmts: &[NodeIndex], final_expr: NodeIndex) -> NodeIndex {
        let idx = NodeIndex(self.ast.tags.len() as u32);
        let extra_start = self.ast.extra.len() as u32;

        // Store statements in extra
        for &stmt in stmts {
            self.ast.extra.push(stmt.0);
        }

        self.ast.tags.push(NodeTag::BlockExpr);
        self.ast.data.push(NodeData {
            main_token: token,
            lhs: extra_start,
            rhs: final_expr.0,
        });
        idx
    }
}
```

### Reading the AST (AstGen, Printer)

```rust
impl Ast {
    pub fn node_tag(&self, idx: NodeIndex) -> NodeTag {
        self.tags[idx.0 as usize]
    }

    pub fn node_data(&self, idx: NodeIndex) -> NodeData {
        self.data[idx.0 as usize]
    }

    pub fn int_value(&self, idx: NodeIndex) -> u64 {
        debug_assert_eq!(self.node_tag(idx), NodeTag::IntLit);
        let data = self.node_data(idx);
        (data.lhs as u64) | ((data.rhs as u64) << 32)
    }

    pub fn binary_operands(&self, idx: NodeIndex) -> (NodeIndex, NodeIndex) {
        debug_assert_eq!(self.node_tag(idx), NodeTag::BinaryExpr);
        let data = self.node_data(idx);
        (NodeIndex(data.lhs), NodeIndex(data.rhs))
    }

    pub fn block_statements(&self, idx: NodeIndex) -> &[u32] {
        debug_assert_eq!(self.node_tag(idx), NodeTag::BlockExpr);
        let data = self.node_data(idx);
        let start = data.lhs as usize;

        // Find end by looking for the next non-statement node
        // (In practice, we'd store the count in extra)
        // Better: store count at extra[start]
        let count = self.extra[start] as usize;
        &self.extra[start+1..start+1+count]
    }

    pub fn block_final_expr(&self, idx: NodeIndex) -> NodeIndex {
        debug_assert_eq!(self.node_tag(idx), NodeTag::BlockExpr);
        let data = self.node_data(idx);
        NodeIndex(data.rhs)
    }
}
```

## Migration Strategy

Phase 2 will implement these structures alongside the existing tree-based AST, allowing gradual migration:

1. Add SOA structures to `ast.rs`
2. Add `AstBuilder` API
3. Update parser to build both representations (temporarily)
4. Verify both produce identical semantics
5. Phase 3: Update consumers to read from SOA
6. Phase 4: Remove old tree-based structures

## Performance Expectations

Based on Zig's results (PR #7920):
- **Memory reduction**: 15-20% (fewer allocations, better packing)
- **Parse speed**: 10-15% faster (better cache locality)
- **Clone cost**: Near-zero (just Arc clone, not deep copy)

## Open Questions

1. **Span tracking**: Should we store full Span (start+end) or just main_token and re-tokenize?
   - Zig stores only start offset
   - Rue currently stores full spans
   - **Decision**: Start with main_token only, add Span if needed

2. **String interning**: Should strings be in a separate table or inline?
   - Currently use `lasso::Spur` (already interned)
   - **Decision**: Keep using Spur, store raw u32 in lhs

3. **Directives**: How to encode `@allow(unused)` etc?
   - Store as extra_data with count+list
   - **Decision**: Extra data with [count, directive_node, directive_node, ...]

4. **Visibility and flags**: Pack into unused bits?
   - Could pack flags into high bits of main_token or lhs
   - **Decision**: Start simple, optimize later if needed

## Next Steps (Phase 2)

Once this design is approved:
1. Implement `NodeTag`, `NodeData`, `Ast` structures in `ast.rs`
2. Add `AstBuilder` API for constructing SOA
3. Add accessor methods for reading SOA
4. Write unit tests for encoding/decoding each node type
5. Keep existing tree-based AST temporarily for comparison
