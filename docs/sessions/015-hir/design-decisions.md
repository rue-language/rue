# HIR Design Decisions

## Overview

This document captures the key design decisions made during the implementation of High-level Intermediate Representation (HIR) in the Rue compiler.

## Problem Statement

The original compiler architecture had tight coupling between semantic analysis and code generation:
- Semantic analysis (`rue-semantic`) directly produced typed AST nodes
- Code generation (`rue-codegen`) consumed AST nodes and converted to platform-independent Instructions
- No clean abstraction layer between language-specific and platform-specific concerns
- Difficult to add optimization passes or alternative backends

## Solution: HIR as Intermediate Layer

### Architecture Decision: Separate HIR Crate

**Decision**: HIR lives in the `rue-ir` crate, not in `rue-semantic` or `rue-codegen`.

**Rationale**:
- Creates clean separation between compiler phases
- Allows both semantic analysis and code generation to depend on HIR without circular dependencies
- Enables future use of HIR by multiple backends or optimization passes
- Provides a stable interface that can evolve independently from either producer or consumer

**Alternatives Considered**:
- Put HIR in `rue-semantic`: Would couple HIR to semantic analysis implementation
- Put HIR in `rue-codegen`: Would make HIR target-specific rather than language-specific
- Create separate `rue-hir` crate: Adds complexity without clear benefit over `rue-ir`

### Type Information Strategy

**Decision**: All HIR expressions carry complete type information from semantic analysis.

**Rationale**:
- Code generation needs type information for instruction selection (i32 vs i64 operations)
- Eliminates need for code generator to re-derive types
- Enables type-based optimizations in future optimization passes
- Simplifies code generation logic by having types readily available

**Implementation**:
- Every `HirExpr` variant includes a `ty: RueType` field where needed
- Literals derive types from their values (`HirLiteral::Int32` → `RueType::I32`)
- Helper method `HirExpr::ty()` provides uniform access to type information

### Desugaring Philosophy

**Decision**: HIR removes syntactic details while preserving semantic meaning.

**What is removed**:
- Parentheses (precedence is explicit in tree structure)
- Semicolons (statement boundaries are explicit in vectors)
- Brackets and braces (block structure is explicit in `HirBlock`)
- Token-level details (whitespace, comments)

**What is preserved**:
- All semantic information needed for code generation
- Source location spans for error reporting
- Type information for all expressions
- Control flow structure (if/else, while loops)
- Variable scoping through block structure

**Rationale**:
- Simplifies code generation by eliminating irrelevant syntactic details
- Makes optimization passes easier to implement and reason about
- Reduces memory usage compared to preserving full AST
- Maintains essential information for debugging and error reporting

### Control Flow Representation

**Decision**: Control flow constructs (if, while) are represented as HIR expression variants, not statements.

**Rationale**:
- Reflects Rue's expression-based nature (if/while produce values)
- Simplifies type checking (expressions have types, statements don't)
- Enables easier optimization (expressions can be moved, statements cannot)
- Matches semantic analysis where if/while are analyzed as expressions

**Implementation**:
```rust
HirExpr::If {
    cond: Box<HirExpr>,
    then_block: HirBlock, 
    else_block: Option<HirBlock>,
    ty: RueType,
    span: Span,
}

HirExpr::While {
    cond: Box<HirExpr>,
    body: HirBlock,
    span: Span,  // While always returns unit type
}
```

### Function Call Representation

**Decision**: Function calls are first-class HIR expressions with string names, not references.

**Rationale**:
- Simplifies HIR construction (no need to maintain function reference tables)
- String names are sufficient for code generation
- Matches the current compiler architecture where functions are resolved by name
- Avoids lifetime complexity in HIR structures

**Trade-offs**:
- Pro: Simple, matches existing architecture
- Pro: No lifetime parameters needed in HIR
- Con: Could complicate advanced optimizations that need function metadata
- Con: String matching at code generation time (but performance is not critical yet)

### Source Location Tracking

**Decision**: All HIR nodes include source span information from the lexer.

**Rationale**:
- Enables precise error reporting during code generation
- Essential for IDE features and debugging support
- Minimal memory overhead compared to overall HIR size
- Future-proofs for debug information generation

**Implementation**: Every HIR node includes a `span: Span` field from `rue-lexer`.

### Builder Pattern

**Decision**: HIR construction uses a builder pattern in `rue-semantic/src/hir_builder/`.

**Rationale**:
- Separates HIR construction logic from semantic analysis algorithms
- Provides single point of control for HIR generation
- Enables consistent error handling during lowering
- Makes HIR construction testable independently

**Architecture**: The builder stays in `rue-semantic` because:
- It needs access to semantic analysis state (type checker, scope information)
- HIR construction is logically part of the semantic analysis phase
- Keeps `rue-ir` focused on HIR definitions, not construction logic

## Implementation Results

### Benefits Achieved

1. **Clean Separation**: Code generation no longer depends on AST, only HIR
2. **Better Testing**: HIR round-trip tests validate lowering correctness
3. **Simplified Codegen**: Code generation logic is cleaner with desugared input
4. **Future Extensibility**: Architecture ready for optimization passes and multiple backends
5. **Bug Resolution**: Fixed critical recursive function issues during implementation

### Performance Impact

- **Memory**: HIR adds one additional IR level but removes syntactic details
- **Compilation Speed**: Minimal impact, HIR construction is fast
- **Code Quality**: No regression, same quality x86-64 code generated

### Test Coverage

- 55/55 semantic tests pass (includes HIR generation)
- 11/11 integration tests pass (end-to-end with HIR)
- 10/10 HIR round-trip tests (validates HIR correctness)
- Comprehensive coverage of all language features

## Future Considerations

### Optimization Pass Integration

The HIR design enables future optimization passes:
- Constant folding can operate on `HirLiteral` values
- Dead code elimination can remove unused `HirStatement`s  
- Function inlining can operate on `HirFunction` bodies
- Type-based optimizations can use `HirExpr::ty()` information

### Multiple Backend Support

HIR provides clean abstraction for multiple backends:
- LLVM backend could consume HIR directly
- Cranelift backend could use HIR as input
- Interpreter could execute HIR without code generation
- Alternative target architectures (ARM, RISC-V) could share HIR layer

### Advanced Language Features

HIR design accommodates future language extensions:
- Pattern matching could add new `HirExpr` variants
- Closures could extend `HirFunction` representation
- Generics could add type parameters to HIR nodes
- Module system could add scope information to HIR

## Conclusion

The HIR implementation successfully addresses the original coupling issues while providing a solid foundation for future compiler enhancements. The design decisions prioritize simplicity and correctness while maintaining extensibility for advanced features.