# Design Decisions: Parser-Semantic Decoupling

## Context

The Rue compiler experienced regressions during a previous attempt at a data-driven parser redesign because the semantic analyzer was tightly coupled to the specific CST structure. This document captures the design decisions made to decouple these components.

## Problem Statement

### Current Issues
1. **Tight Coupling**: Semantic analyzer directly traverses CST nodes, depending on exact structure
2. **Brittle Code**: Parser changes cascade into semantic analysis changes
3. **Limited Evolution**: Can't experiment with different parsing strategies
4. **Performance**: Current pointer-based CST not cache-friendly

### Failed Previous Attempt
- Tried to implement data-driven parser with arena allocation
- Semantic analyzer broke due to assumptions about CST structure
- Too many changes required simultaneously
- Rolled back to avoid extensive rework

## Key Design Decisions

### Decision 1: Add Untyped AST Layer

**Options Considered**:
1. Make HIR untyped, add separate type overlay (like rustc)
2. Add abstraction layer over CST (facades/views)
3. Add new untyped AST between CST and HIR
4. Keep status quo, carefully refactor semantic analyzer

**Decision**: Add new untyped AST between CST and HIR

**Rationale**:
- Least invasive to existing code
- Clear separation of concerns
- Proven pattern in TypeScript, Kotlin
- HIR can remain typed (no refactoring needed)
- Can be done incrementally

### Decision 2: Data-Oriented Design for AST

**Options Considered**:
1. Traditional tree with Box/Rc (like current CST)
2. Arena allocation with typed indices (ECS-style)
3. Flat arrays with parent pointers
4. Zig-inspired struct-of-arrays with uniform nodes

**Decision**: Zig-inspired struct-of-arrays design

**Rationale**:
- **Maximum cache efficiency** (4-5 nodes per cache line vs 1-2)
- **Proven performance** in Zig compiler (one of fastest parsers)
- **Predictable memory patterns** for hardware prefetchers
- **Minimal memory overhead** (16 bytes per node)
- **Zero allocations** for variable-length data

**Design Sketch**:
```rust
pub struct Ast {
    nodes: NodeList,         // All nodes in SoA format
    extra_data: Vec<u32>,    // Variable-length data
    spans: Vec<Span>,        // Parallel array for source locations
    strings: StringInterner, // String deduplication
}

pub struct NodeList {
    tags: Vec<NodeTag>,      // 1 byte discriminants
    tokens: Vec<u32>,        // String/token references
    data: Vec<NodeData>,     // Two u32 fields per node
}

pub struct NodeData {
    lhs: u32,  // Left child or extra_data index
    rhs: u32,  // Right child or literal value
}
```

**Trade-offs**:
- (+) 3-4x better cache utilization than arena approach
- (+) Perfect for linear traversals and filtering
- (-) Less type safety (everything is u32 indices)
- (-) More complex to understand initially

### Decision 3: Consolidate IRs in rue-ir Crate

**Options Considered**:
1. Keep IRs distributed (CST in rue-ast, others in rue-ir)
2. Move all IRs to rue-ir
3. Create new rue-frontend crate for CST/AST
4. One crate per IR

**Decision**: Move all IRs to rue-ir eventually

**Rationale**:
- Conceptual cohesion - all IRs in one place
- Easier to see/design transformations
- Shared utilities (spans, string interning)
- Clear dependency structure

**Migration Plan**:
- Start by adding AST to rue-ir
- Keep CST in rue-ast for now
- Consider moving CST later if beneficial

### Decision 4: Incremental Migration Strategy

**Options Considered**:
1. Big bang - replace everything at once
2. Parallel paths - keep both working
3. Feature flag switching
4. Version-based (v1 vs v2 compiler)

**Decision**: Parallel paths with eventual convergence

**Rationale**:
- Can validate correctness by comparing outputs
- No disruption to development
- Can benchmark both approaches
- Gradual confidence building

**Implementation**:
```rust
// In rue-semantic
pub fn analyze_cst(cst: &CstRoot) -> Result<TypedHir> { /* existing */ }
pub fn analyze_ast(ast: &Ast) -> Result<TypedHir> { /* new */ }

// In compiler pipeline - run both, compare results
let hir_from_cst = analyze_cst(&cst)?;
let ast = lower_cst_to_ast(&cst);
let hir_from_ast = analyze_ast(&ast)?;
assert_eq!(hir_from_cst, hir_from_ast);
```

## Rejected Alternatives

### Why Not Rustc-Style Untyped HIR?

Rustc has HIR as untyped, with types stored separately:
- Would require refactoring all existing HIR consumers
- MIR lowering would need significant changes
- More complex than adding new layer
- Can always do this later if needed

### Why Not Just Views/Facades?

Creating abstract views over CST:
- Still couples semantic analyzer to parser crate
- Views would need maintenance as CST changes
- Doesn't solve performance issues
- Half-measure that doesn't fully decouple

### Why Not Fix Semantic Analyzer In-Place?

Making semantic analyzer more flexible:
- Would require extensive refactoring
- High risk of introducing bugs
- Doesn't address performance concerns
- Still coupled, just less tightly

## Trade-offs

### Benefits
- **Flexibility**: Parser can evolve independently
- **Performance**: Data-oriented design for new components
- **Correctness**: Can validate via parallel paths
- **Incremental**: Low-risk migration

### Costs
- **Complexity**: Additional IR layer to maintain
- **Memory**: Temporary duplication during migration
- **Time**: Multi-phase implementation
- **Documentation**: More concepts to explain

## Implementation Priorities

1. **Correctness over performance** - Ensure AST path produces identical results
2. **Incremental progress** - Each PR should leave compiler working
3. **Maintain tests** - All existing tests must pass
4. **Document as we go** - Update docs with each change

## Success Criteria

The refactoring succeeds when:

1. Parser can be changed without touching semantic analyzer
2. Both CST→HIR and CST→AST→HIR paths produce identical output
3. No performance regression (goal: improvement from data-oriented design)
4. Architecture is documented and understood
5. Future data-driven parser attempt would not face same issues

## Lessons from Previous Attempt

### What Went Wrong
- Tried to change too much at once
- Underestimated coupling between components  
- No incremental validation path
- Insufficient abstraction boundaries

### What We're Doing Differently
- Adding abstraction layer first
- Keeping parallel paths for validation
- Incremental, phase-based approach
- Clear IR boundaries and responsibilities

## Future Considerations

### Phase 2: Data-Oriented All IRs
Once AST proves successful:
- Convert HIR to data-oriented
- Then MIR, then PIR
- One at a time, validated incrementally

### Phase 3: Advanced Optimizations
With data-oriented IRs:
- Parallel compilation passes
- Incremental compilation
- IR serialization/caching
- SIMD processing of nodes

## References

- [Data-Oriented Design and C++](https://www.youtube.com/watch?v=rX0ItVEVjHc) - Mike Acton
- [Cranelift's IR Design](https://github.com/bytecodealliance/wasmtime/blob/main/cranelift/docs/ir.md)
- [Rust Compiler HIR](https://rustc-dev-guide.rust-lang.org/hir.html)
- [Swift's AST Design](https://github.com/apple/swift/blob/main/docs/ASTManual.md)