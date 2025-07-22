# MIR Design Decisions

## Overview

This document captures the key design decisions made for the Rue compiler's MIR (Mid-level Intermediate Representation) layer and the rationale behind them.

## Decision 1: Block Parameters vs Phi Nodes

### Decision
Use block parameters instead of traditional SSA phi nodes.

### Rationale
1. **Conceptual clarity**: Block parameters model control flow joins as function calls, which is more intuitive
2. **Implementation simplicity**: No special phi instruction handling needed
3. **Modern precedent**: Cranelift IR and other modern compilers use this approach
4. **Optimization benefits**: Some optimizations (like jump threading) are simpler without phi nodes

### Trade-offs
- **Pros**: Cleaner semantics, easier to understand, simpler implementation
- **Cons**: Slightly more verbose, requires explicit argument passing at jumps

### Example
```
// Traditional SSA with phi
B3:
  x = phi [B1: x1, B2: x2]
  
// Block parameters
B3(x):
  // x is received as parameter
```

## Decision 2: MIR Placement in Pipeline

### Decision
Insert MIR between HIR and the current Instruction enum.

### Rationale
1. **Clean separation**: HIR remains language-specific, MIR is optimization-focused
2. **Minimal disruption**: Existing HIR and Instruction layers remain mostly unchanged
3. **Optimization opportunity**: MIR is the ideal level for most optimizations
4. **Future flexibility**: Can add more optimization passes without touching other layers

### Alternative Considered
Replacing the Instruction enum entirely with MIR.
- Rejected because: Would require major refactoring of existing code generator

## Decision 3: SSA Construction Strategy

### Decision
Build SSA form directly during HIR lowering rather than converting later.

### Rationale
1. **Efficiency**: Avoids intermediate non-SSA form
2. **Simplicity**: No need for SSA construction algorithms
3. **Natural fit**: HIR's expression-based nature maps well to SSA

### Implementation Strategy
- Each HIR expression generates a new temp
- Variables are tracked as their most recent temp assignment
- Block parameters handle control flow joins

## Decision 4: Type Representation

### Decision
Reuse RueType from the existing type system rather than creating MirType.

### Rationale
1. **Type preservation**: Maintains full type information for optimizations
2. **Code reuse**: Leverages existing type infrastructure
3. **Debugging**: Better error messages with full type information
4. **Future compatibility**: Ready for more complex type systems

### Trade-off
Slightly larger MIR representation, but the benefits outweigh the cost.

## Decision 5: Optimization Pass Architecture

### Decision
Implement optimization passes as separate modules that transform MIR in-place.

### Rationale
1. **Modularity**: Each pass is independent and testable
2. **Composability**: Easy to enable/disable specific optimizations
3. **Debugging**: Can dump MIR after each pass
4. **Standard practice**: Follows LLVM and other compiler designs

### Pass Order (Initial)
1. Dead code elimination
2. Constant propagation
3. Common subexpression elimination

## Decision 6: Control Flow Representation

### Decision
Use explicit basic blocks with terminators, no fall-through.

### Rationale
1. **Explicit control flow**: All jumps are visible in the IR
2. **Analysis friendly**: CFG is trivial to construct
3. **Optimization friendly**: Block reordering is straightforward
4. **Standard practice**: Most modern IRs use this approach

### Terminator Types
- `Goto`: Unconditional jump with arguments
- `Branch`: Conditional jump with arguments for both targets
- `Return`: Function return with optional value

## Decision 7: Memory Model

### Decision
Keep MIR at a high level - no explicit memory operations, only temps and function calls.

### Rationale
1. **Simplicity**: Rue has no heap allocation or pointers currently
2. **Safety**: No memory safety issues at MIR level
3. **Future-proof**: Can add memory operations later if needed
4. **Focus**: Optimizations can focus on computation, not memory

### Implications
- All values are either temps or constants
- No load/store operations in MIR
- Stack allocation handled by lower levels

## Decision 8: Function Call Representation

### Decision
Function calls remain high-level in MIR, with explicit argument and result temps.

### Rationale
1. **Interprocedural analysis**: Easier to analyze function calls
2. **Inlining preparation**: Clean representation for future inlining
3. **ABI abstraction**: Calling convention details left to lower levels

## Decision 9: Constant Representation

### Decision
Constants are materialized as temps via assignment statements.

### Rationale
1. **Uniform treatment**: All values are temps, simplifying analysis
2. **Constant propagation**: Natural representation for the optimization
3. **CSE opportunity**: Duplicate constants can be eliminated

### Example
```
// Instead of: t1 = t0 + 5
t1 = 5          // Constant materialization
t2 = t0 + t1    // Uniform binary operation
```

## Decision 10: Debug Information

### Decision
Preserve source spans through MIR transformations.

### Rationale
1. **Error reporting**: Optimization errors can point to source
2. **Debugging**: Future debugger support needs source mapping
3. **Profiling**: Performance tools need source correlation

### Implementation
Each MIR construct optionally carries a source span that transformations preserve when sensible.

## Future Considerations

### Potential Extensions
1. **Memory operations**: Add load/store when Rue gets references/pointers
2. **Aggregate types**: Support for structs/arrays when added to Rue
3. **Exception handling**: Try/catch representation if added
4. **Parallel constructs**: Threading primitives if needed

### Optimization Opportunities
1. **Inlining**: Inline small functions at MIR level
2. **Loop optimizations**: Unrolling, invariant hoisting
3. **Escape analysis**: Stack allocation optimization
4. **Devirtualization**: If Rue adds dynamic dispatch

## Conclusion

These design decisions create a clean, modern MIR that:
- Is simple to implement and understand
- Provides a good foundation for optimizations
- Integrates well with the existing compiler
- Is extensible for future language features

The use of block parameters instead of phi nodes is a key distinguishing feature that simplifies many aspects of the implementation while maintaining full SSA benefits.