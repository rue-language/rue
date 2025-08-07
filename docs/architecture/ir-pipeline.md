# Rue Compiler IR Pipeline Architecture

## Overview

The Rue compiler uses a multi-stage intermediate representation (IR) pipeline to transform source code into executable binaries. This document describes the architecture, rationale, and design decisions for each IR layer.

## IR Pipeline Stages

```
Source Code
    ↓ [Lexer]
Token Stream
    ↓ [Parser]
CST (Concrete Syntax Tree)
    ↓ [AST Builder]
AST (Abstract Syntax Tree) - untyped
    ↓ [Type Checker]
HIR (High-level IR) - typed
    ↓ [MIR Builder]
MIR (Mid-level IR)
    ↓ [MIR to PIR]
PIR (Platform IR)
    ↓ [Code Generator]
Machine Code
```

## Rationale for Each Layer

### CST (Concrete Syntax Tree)
**Purpose**: Preserve complete syntactic information from source code.

**Contains**:
- All tokens including keywords, operators, delimiters
- Trivia (whitespace, comments)
- Syntactic constructs (parentheses, semicolons)
- Exact source positions

**Used for**:
- Error reporting with precise locations
- Code formatting and refactoring tools
- Syntax highlighting in IDEs
- Incremental parsing

### AST (Abstract Syntax Tree) - Untyped
**Purpose**: Provide a semantic view of the program structure without type information.

**Contains**:
- Program structure (functions, statements, expressions)
- Semantic information (variable names, operators)
- Source spans for error reporting
- No syntactic noise (no parentheses, semicolons, trivia)

**Benefits**:
- **Decouples parser from semantic analysis** - Parser can be redesigned without breaking type checker
- **Simplifies semantic analysis** - Works with semantic concepts, not parse details
- **Enables parallel processing** - Can parse multiple files before type checking
- **Better error recovery** - Can build partial AST even with parse errors

**Design**: Zig-inspired ultra-compact representation
```rust
pub struct Ast {
    nodes: NodeList,         // Struct-of-arrays for cache efficiency
    extra_data: Vec<u32>,    // Variable-length node data
    spans: Vec<Span>,        // Source locations (parallel array)
    strings: StringInterner, // Deduplicated strings
}

pub struct NodeList {
    tags: Vec<NodeTag>,      // 1 byte per node
    tokens: Vec<u32>,        // Main token/string ID
    data: Vec<NodeData>,     // 8 bytes - two u32 fields
}
```

This design achieves:
- **16 bytes per node** (vs 40-80 bytes in traditional ASTs)
- **4-5 nodes per cache line** (vs 1-2 nodes)
- **Linear memory access** patterns for traversal
- **Zero allocations** for child lists (uses extra_data)

### HIR (High-level Intermediate Representation) - Typed
**Purpose**: Represent the fully type-checked, semantically analyzed program.

**Contains**:
- Complete type information on every expression
- Resolved names and scopes
- Type-checked function bodies
- Semantic validations complete

**Used for**:
- Type-based optimizations
- Borrow checking (future)
- Lowering to execution model

### MIR (Mid-level Intermediate Representation)
**Purpose**: Represent the program in terms of basic blocks and control flow.

**Contains**:
- Control flow graph
- Basic blocks with terminators
- SSA-like representation with virtual registers
- Explicit control flow (no nested expressions)

**Used for**:
- Control flow optimizations
- Dead code elimination
- Constant propagation
- Register allocation preparation

### PIR (Platform Intermediate Representation)
**Purpose**: Platform-specific but architecture-independent representation.

**Contains**:
- Platform calling conventions
- Stack frame layout
- But still uses virtual registers

**Used for**:
- Final register allocation
- Instruction selection
- Platform-specific optimizations

## Key Design Decisions

### 1. Why Separate AST from CST?

Many compilers combine these (e.g., early Rust compiler), but separation provides:

- **Parser flexibility**: Can experiment with different parsing strategies (recursive descent, Pratt parsing, data-driven) without affecting downstream passes
- **Tool support**: CST retains information needed for formatting, refactoring
- **Error recovery**: Can build valid AST from partially invalid CST
- **Performance**: AST can be data-oriented while CST preserves source structure

### 2. Why Untyped AST Before Typed HIR?

Following the pattern of mature compilers (rustc, Swift, Kotlin):

- **Incremental compilation**: Can cache AST and only re-type-check changed functions
- **Better diagnostics**: Can analyze program structure even with type errors
- **Parallel compilation**: Parse all files to AST, then type-check in parallel
- **Language evolution**: New syntax features often desugar to existing AST nodes

### 3. Data-Oriented Design for New IRs

Starting with AST, new IRs use arena allocation and indices:

```rust
// Instead of Box<Expr> or Rc<Expr>
pub struct ExprId(u32);

// Expressions stored contiguously
pub struct Arena<T> {
    items: Vec<T>,
}
```

Benefits:
- **Cache efficiency**: Similar nodes stored together
- **Reduced allocations**: Bulk allocation in arenas
- **Parallel processing**: No shared mutable state
- **Serialization**: Can easily save/load IR

## Migration Strategy

### Phase 1: Add AST Layer (Current)
- Keep existing CST → HIR path working
- Add new CST → AST → HIR path
- Run both in parallel during development

### Phase 2: Validate Equivalence
- Ensure both paths produce identical HIR
- Benchmark performance differences
- Fix any semantic discrepancies

### Phase 3: Switch Default Path
- Make AST path the default
- Keep old path for comparison/debugging

### Phase 4: Remove Old Path
- Delete direct CST → HIR code
- Simplify semantic analyzer

### Future: Data-Oriented All IRs
- Gradually convert HIR, MIR, PIR to data-oriented design
- One IR at a time to minimize disruption

## IR Consolidation

All IRs will eventually live in the `rue-ir` crate:

```
rue-ir/
  src/
    cst.rs   // Future: move from rue-ast
    ast.rs   // Untyped AST (new)
    hir.rs   // Typed HIR (existing)
    mir.rs   // MIR (existing)  
    pir.rs   // PIR (existing)
    types.rs // Type definitions (existing)
```

This provides:
- Single source of truth for IR definitions
- Clear module boundaries
- Easier cross-IR transformations
- Better documentation organization

## Comparison with Other Compilers

### Rust (rustc)
- Token Stream → AST → HIR (untyped) → THIR (typed) → MIR
- Similar separation of parsing from type checking

### Swift
- Parse Tree → AST (untyped) → AST + Types → SIL
- Types added as overlay on AST

### TypeScript  
- Source → AST → Type Checker overlays types → JS Emit
- Very clear separation of syntax and semantics

### Clang
- Tokens → AST (with mutable type fields) → LLVM IR
- Types filled in during semantic analysis

## Success Metrics

The IR pipeline architecture succeeds when:

1. **Parser changes don't break semantic analysis** - Can redesign parser internals freely
2. **Each IR has a clear purpose** - No overlap or confusion about responsibilities
3. **Transformations are simple** - Each lowering step is straightforward
4. **Performance improves** - Data-oriented design provides measurable speedup
5. **Debugging is easier** - Can inspect each IR independently

## References

- [Rust Compiler Development Guide - HIR](https://rustc-dev-guide.rust-lang.org/hir.html)
- [Swift Compiler Architecture](https://github.com/apple/swift/blob/main/docs/CompilerArchitecture.md)
- [TypeScript Compiler Internals](https://github.com/microsoft/TypeScript/wiki/Architectural-Overview)
- [Data-Oriented Design by Richard Fabian](https://www.dataorienteddesign.com/dodbook/)