# Technical Context for HIR Refactoring

## Current Architecture Understanding

### AST Structure (Already Data-Driven)
Located in `crates/rue-ir/src/ast.rs`:
```rust
pub struct Ast {
    nodes: NodeList,         // Struct-of-arrays
    extra_data: Vec<u32>,    // Variable-length data
    spans: Vec<Span>,        // Source locations
    strings: StringPool,     // Interned strings
    root: NodeIndex,         // Root node index
}

pub struct NodeList {
    tags: Vec<NodeTag>,      // 1 byte per node
    tokens: Vec<u32>,        // Main token/string ID
    data: Vec<NodeData>,     // 8 bytes - two u32 fields
}
```

### Current HIR Structure (Tree-Based)
Located in `crates/rue-ir/src/hir.rs`:
```rust
pub enum HirExpr {
    Literal { value: HirLiteral, ty: RueType, span: Span },
    Binary { 
        op: BinOp, 
        lhs: Box<HirExpr>,  // Heap allocation!
        rhs: Box<HirExpr>,  // Heap allocation!
        ty: RueType, 
        span: Span 
    },
    If {
        cond: Box<HirExpr>,
        then_block: HirBlock,
        else_block: Option<HirBlock>,
        ty: RueType,
        span: Span,
    },
    // ... more variants with Box<> allocations
}
```

### Current TypeChecker Flow
Located in `crates/rue-semantic/src/type_checker.rs`:
1. Takes CST as input
2. Builds tree-based HIR with Box allocations
3. Returns complete HIR tree

### MIR Structure (Already Instruction-Based)
Located in `crates/rue-lowering/src/mir.rs`:
- Already uses basic blocks and instructions
- Natural fit for instruction-based HIR

## Key Insights from Zig

### No CST
Zig's parser directly builds AST without intermediate CST.

### Instruction-Based IR
From Mitchell's articles:
- ZIR is an instruction stream, not a tree
- Each instruction has opcode + data
- Types stored separately (parallel array)
- Extra data for complex instructions

### Memory Layout
Everything uses struct-of-arrays for cache efficiency:
- Sequential access patterns
- Multiple instructions per cache line
- No pointer chasing

## Critical Code Locations

### Files That Need Modification
1. **Create New**:
   - `crates/rue-ir/src/hir2.rs` - New HIR definition
   - `crates/rue-ir/src/hir2_builder.rs` - HIR construction

2. **Modify Existing**:
   - `crates/rue-semantic/src/type_checker.rs` - Output instructions
   - `crates/rue-semantic/src/lib.rs` - Export new analyze function
   - `crates/rue-lowering/src/lib.rs` - Accept new HIR format
   - `crates/rue-compiler/src/pipeline.rs` - Use new pipeline

3. **Update Tests**:
   - `crates/rue-semantic/src/*_test.rs` - All test files
   - These currently expect tree-based HIR

### Current Test Count
67 tests in rue-semantic that need to work with new HIR:
- 36 aggregate type tests
- 8 HIR validation tests  
- 8 type preservation tests
- 7 control flow tests
- 5 constraint inference tests
- 3 HIR builder tests

## Migration Strategy Details

### Parallel Implementation
Keep both HIR formats during migration:
```rust
// In rue-semantic/src/lib.rs
pub fn analyze_cst(cst: &CstRoot) -> Result<AnalysisResult, Error> {
    // Current implementation
}

pub fn analyze_cst_v2(cst: &CstRoot) -> Result<AnalysisResultV2, Error> {
    // New instruction-based implementation
}
```

### Instruction Encoding Examples

**Binary Operation**:
```rust
// Instruction: Binary
// tag = InstTag::Binary
// data.lhs = left operand instruction index
// data.rhs = right operand instruction index  
// extra_data[0] = BinOp enum value
// types[i] = result type
```

**Function Call**:
```rust
// Instruction: Call
// tag = InstTag::Call
// data.lhs = function name string index
// data.rhs = extra_data index for arguments
// extra_data[idx..idx+n] = argument instruction indices
// types[i] = return type
```

**If Expression**:
```rust
// Instruction: If
// tag = InstTag::If
// data.lhs = condition instruction index
// data.rhs = extra_data index
// extra_data[idx] = then block index
// extra_data[idx+1] = else block index (or 0)
// types[i] = result type (unified type of branches)
```

## Performance Expectations

### Current HIR (Tree-Based)
- Multiple heap allocations per expression
- Poor cache locality (pointer chasing)
- Difficult to parallelize

### New HIR (Instruction-Based)
- Zero heap allocations (except strings)
- Sequential memory access
- ~4 instructions per cache line
- Easy parallel processing

### Measurement Points
1. Memory usage (expect 4x reduction)
2. Cache misses (expect 50% reduction)
3. Traversal speed (expect 2x improvement)
4. Compilation time (expect 20% improvement)

## Gotchas and Edge Cases

1. **Instruction Ordering**: Unlike tree, order matters
2. **Block Scoping**: Need to track instruction ranges
3. **Type Information**: Must maintain parallel array
4. **Span Tracking**: Keep source location info
5. **String Interning**: Share pool with AST
6. **Extra Data**: Variable-length data needs careful indexing

## Validation Requirements

1. **Correctness**: Output identical MIR
2. **Performance**: Measurable improvement
3. **Maintainability**: Simpler optimization passes
4. **Debuggability**: Preserve source locations