# Design Decisions for Data-Driven HIR

## Background: Zig's Compilation Model
Based on Mitchell Hashimoto's analysis:
- **No CST**: Parser directly builds AST
- **AST**: Struct-of-arrays, ultra-compact
- **ZIR**: Untyped instruction stream (struct-of-arrays)
- **AIR**: Typed instruction stream (struct-of-arrays)
- **Key insight**: Everything is instructions, not trees

## Decision 1: HIR as Instruction Stream
**Choice**: Transform HIR from tree to instruction stream
**Rationale**: 
- Matches Zig's ZIR concept
- Better cache locality (sequential access)
- Easier to optimize (just reorder instructions)
- Natural fit for MIR lowering (already instruction-based)

## Decision 2: Struct-of-Arrays Layout
**Choice**: Use same struct-of-arrays pattern as AST
**Rationale**:
- Proven pattern from AST implementation
- ~4 instructions per cache line
- Consistent with rest of compiler
- Easy parallel processing

## Decision 3: Instruction Encoding
**Choice**: Tag + Data format (like AST nodes)
```rust
pub struct InstData {
    lhs: u32,  // First operand (index/value)
    rhs: u32,  // Second operand (index/value)
}
```
**Rationale**:
- 9 bytes per instruction (1 tag + 8 data)
- Fits most operations in fixed size
- Extra data array for complex instructions
- Same pattern as AST NodeData

## Decision 4: Type Storage
**Choice**: Parallel array for types (not embedded)
```rust
types: Vec<RueType>  // types[i] is type of instruction i
```
**Rationale**:
- Types often not needed during traversal
- Better cache usage when types skipped
- Can be None for statements
- Matches Zig's approach

## Decision 5: Incremental Migration
**Choice**: Keep CST temporarily, migrate in phases
**Phase 1**: Create data-driven HIR (this session)
**Phase 2**: Port TypeChecker to use AST instead of CST
**Phase 3**: Eliminate CST, parse directly to AST
**Rationale**:
- Less disruptive
- Can validate each phase
- Maintains working compiler throughout

## Decision 6: Instruction Set Design
**Choice**: Higher-level than MIR, lower than AST
**Examples**:
- `Let` instruction (not separate alloc/store)
- `If` instruction (not jumps)
- `Call` instruction (not stack manipulation)
**Rationale**:
- Preserves semantic information
- Easier optimization
- Natural lowering to MIR

## Rejected Alternatives

### Arena Allocators
**Rejected**: Using arena allocators with indices
**Reason**: Struct-of-arrays is more cache-efficient

### Tree-Based HIR with Indices
**Rejected**: Keeping tree structure but using indices
**Reason**: Instruction stream better for optimization

### Embedding Types in Instructions
**Rejected**: Storing RueType inline in instruction data
**Reason**: Wastes space, hurts cache usage

### Direct AST to MIR
**Rejected**: Skipping HIR entirely
**Reason**: HIR provides important optimization layer