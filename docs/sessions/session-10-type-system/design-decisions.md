# Session 10: Type System - Design Decisions

## Overview

This session introduces a type system to Rue, extending the language beyond its current limitation of only supporting 64-bit integers. This represents a fundamental enhancement to the language's expressiveness while maintaining its minimalist philosophy.

## Key Design Decisions

### 1. Type Annotation Syntax

**Decision**: Adopt Rust-like type annotation syntax

**Rationale**:
- Consistency with Rue's existing Rust-inspired syntax
- Familiar to Rust developers
- Clear visual separation between name and type using `:`
- Well-established syntax for function return types using `->`

**Examples**:
```rue
let x: i32 = 42;
let flag: bool = true;
fn add(a: i32, b: i32) -> i32 { a + b }
```

**Alternatives Considered**:
- C-style prefix types: `i32 x = 42;` - Rejected for inconsistency with existing syntax
- TypeScript-style: Already what we chose
- Type inference only: do not want whole program inference

### 2. Initial Type Set

**Decision**: Start with a minimal set of types: `i32`, `i64`, `bool`, and `()`

**Rationale**:
- `i32`: Common integer size, demonstrates multiple numeric types
- `i64`: Maintains backward compatibility with existing code
- `bool`: Essential for conditional logic beyond integer comparisons
- `()`: Unit type for procedures (functions with no meaningful return)

**Future Types** (not in initial implementation):
- Arrays: `[T; N]`
- Strings: `str`
- Floating point: `f32`, `f64`
- Unsigned integers: `u32`, `u64`

### 3. Type Inference Strategy

**Decision**: Require explicit type annotations, minimal inference

**Rationale**:
- Same as Rust: no global anayses needed
- Specifying types and inferring bodies leads to good error messages

**Inference Rules**:
- Numeric literals default to `i32` unless annotated
- Boolean literals (`true`/`false`) are always `bool`
- Expression types derived from operands
- No bidirectional type inference (yet)

### 4. Backward Compatibility

**Decision**: break compatibility

**Rationale**: There are like 3 rue programs in existence

**Migration Strategy**:
```rue
// Old style
let x = 42;  // Defaults to i32

// New style
let x: i64 = 42;  // Explicitly i64
```

### 5. Type Checking Rules

**Decision**: Strict type checking with no implicit conversions

**Rationale**:
- Prevents subtle bugs
- Makes type errors explicit
- Simpler to implement
- Educational value in understanding types

**Examples**:
```rue
let x: i32 = 42;
let y: i64 = 100;
let z = x + y;  // Error: cannot add i32 and i64
```

### 6. Boolean Representation

**Decision**: Represent booleans as 0 (false) and 1 (true) at runtime

**Rationale**:
- Simple mapping to assembly
- Compatible with conditional jumps
- Minimal code generation changes
- Standard practice in many languages

### 7. main returns

The main function can return any of our types:

- `()`: always returns 0
- `i32` or `i64`: returns numeric value
- `bool`: returns 0 for false, and 1 for true

The support for `bool` is kind of funny, because it's a bit backwards from what
I might expect. I'm going to give it a try and see how it goes.

### 8. Implementation Phases

**Decision**: Implement in three phases

**Phase 1**: Parser and AST support
- Update grammar
- Extend AST nodes
- Parse type annotations

**Phase 2**: Semantic analysis
- Type checking
- Error reporting
- Symbol table updates

**Phase 3**: Code generation
- Type-specific instructions
- Boolean handling
- Testing and validation

## Design Principles

1. **Minimalism**: Add only essential type system features
2. **Explicitness**: Prefer explicit over implicit
3. **Teachability**: Keep concepts simple and clear
4. **Extensibility**: Design for future type additions

## Rejected Alternatives

1. **Full type inference** - Too complex for Rue's goals
2. **Implicit conversions** - Source of bugs, against explicitness
3. **Generics** - Out of scope for minimal language
4. **Structural types** - Would require major redesign