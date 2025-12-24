---
id: 0007
title: Hindley-Milner Type Inference
status: proposal
tags: [types, compiler]
feature-flag: hm-inference
created: 2025-12-24
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0007: Hindley-Milner Type Inference

## Status

Proposal

## Summary

Replace the current bidirectional type checking with constraint-based Hindley-Milner type inference. This eliminates ad-hoc "peek" heuristics and default-to-i32 fallbacks, providing consistent, predictable type inference across all expressions.

## Context

The current type system (ADR-0002) uses bidirectional type checking with several limitations:

### Current Limitations

1. **Default i32 fallback**: Integer literals without context default to `i32`:
   ```rust
   // In TypeExpectation::integer_type()
   _ => Type::I32  // The hack
   ```
   This appears in 5+ places in sema.rs (lines 195, 811, 2583, 2907, 2949).

2. **Inconsistent inference**: The `peek_type` function (lines 2895-2987) tries to guess types without full analysis, but can't handle complex cases:
   ```rue
   let x = 1 + 2;  // Both literals -> defaults to i32, can't infer from later use
   ```

3. **Order-dependent inference**: The type of `1 < x` depends on whether x's type is already known. If x is also a literal expression, inference fails and falls back to i32.

4. **No postponed decisions**: The single-pass design can't postpone type decisions when information comes later in the expression.

### Why Hindley-Milner?

HM inference:
- Handles **all** type decisions uniformly through constraint solving
- Is **principal**: finds the most general type
- Is **decidable**: Algorithm W has proven termination
- Scales to **generics/polymorphism** if we add them later
- Is **well-understood**: decades of research and implementations

Note: We're implementing HM for **inference consistency**, not for polymorphism. Rue currently has no generics, and this ADR doesn't add them.

## Decision

### Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                          RIR (input)                            │
└─────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│  Phase 1: Constraint Generation                                 │
│  - Walk RIR, assign type variables to unknowns                  │
│  - Generate constraints: τ₁ = τ₂, τ ∈ {integers}               │
│  - Build substitution environment                               │
└─────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│  Phase 2: Constraint Solving (Unification)                      │
│  - Apply Algorithm W/J unification                              │
│  - Resolve type variables to concrete types                     │
│  - Detect and report type errors                                │
└─────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│  Phase 3: AIR Generation                                        │
│  - Walk RIR again with resolved types                           │
│  - Emit typed AIR instructions                                  │
│  - All types are now concrete                                   │
└─────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│                          AIR (output)                           │
└─────────────────────────────────────────────────────────────────┘
```

### Type Language

Extend the internal type representation (NOT the user-facing `Type` enum):

```rust
/// Internal type representation during inference.
/// Separate from the final Type enum to support type variables.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InferType {
    /// Concrete type (maps to Type enum)
    Concrete(Type),
    /// Type variable (unknown, to be solved)
    Var(TypeVarId),
    /// Integer literal type - can unify with any integer type.
    /// Defaults to i32 if unconstrained at the end.
    IntLiteral,
}

/// Unique identifier for type variables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TypeVarId(u32);
```

### Constraint System

```rust
/// A type constraint generated during analysis.
#[derive(Debug, Clone)]
enum Constraint {
    /// Two types must be equal: τ₁ = τ₂
    Equal(InferType, InferType, Span),

    /// Type must be a signed integer: τ ∈ {i8, i16, i32, i64}
    /// Used for unary negation which requires signed types.
    IsSigned(InferType, Span),
}
```

Note: We don't need `IsInteger` because integer literals use the `IntLiteral` type directly, which encodes "must be an integer" in its unification rules.

### Algorithm

**Phase 1: Constraint Generation**

Walk the RIR and for each expression:
1. If type is known (annotation, parameter, etc.) → use `Concrete(Type)`
2. If it's an integer literal → use `IntLiteral`
3. If type is unknown (unannotated let, etc.) → create fresh type variable `Var(id)`
4. Generate `Equal` constraints based on how expressions combine

Example:
```rue
fn foo(x: i64) -> i64 {
    let y = 1 + x;  // y's type unknown, 1 is IntLiteral
    y
}
```

Generates:
```
type(1) = IntLiteral
type(x) = Concrete(i64)
β = fresh()              // type of y

Equal(IntLiteral, i64)   // 1 + x requires same types
Equal(β, i64)            // y = result of addition
Equal(β, i64)            // returned value must match return type
```

**Phase 2: Unification**

Apply unification with special rules for `IntLiteral`:

1. Process constraints in order
2. For `Equal(τ₁, τ₂)`:
   - If both `Concrete`: check equality, error if different
   - If one is `Var`: substitute the variable
   - If both `Var`: unify (pick one)
   - If one is `IntLiteral` and other is `Concrete(integer)`: `IntLiteral` becomes that integer type
   - If one is `IntLiteral` and other is `Concrete(non-integer)`: error
   - If both are `IntLiteral`: they stay as `IntLiteral` (will be resolved later)
3. Apply substitutions transitively

**Integer defaulting** (the key improvement):
- After all constraints are processed, any remaining `IntLiteral` types default to `i32`
- This happens **once**, **at the end**, not scattered throughout

**Phase 3: AIR Generation**

Walk RIR again with resolved substitution:
- Look up each expression's type variable
- Apply substitution to get concrete type
- Emit AIR with concrete types

### Changes to sema.rs

The main `Sema` struct gains new fields:

```rust
struct Sema<'a> {
    // Existing fields...
    rir: &'a Rir,
    interner: &'a Interner,
    // ...

    // New HM inference fields
    /// Next type variable ID
    next_type_var: u32,
    /// Type variable assignments
    substitution: HashMap<TypeVarId, InferType>,
    /// Pending constraints
    constraints: Vec<Constraint>,
    /// Expression to type variable mapping
    expr_types: HashMap<InstRef, TypeVarId>,
}
```

The `analyze_inst` function splits into:
1. `generate_constraints` - Phase 1
2. (Unification happens between phases)
3. `emit_air` - Phase 3

### Error Messages

With full constraint context, we can provide better error messages:

**Before:**
```
error: type mismatch: expected i32, found i64
  --> file.rue:3:5
```

**After:**
```
error: type mismatch
  --> file.rue:3:5
   |
 3 |     let y = 1 + x;
   |             ^ this has type i64 (from parameter x: i64)
   |
note: literal 1 was inferred as i64 to match the other operand
```

### Backwards Compatibility

This is an internal refactor. The surface language and semantics are unchanged:
- Same types: i8, i16, i32, i64, u8, u16, u32, u64, bool, etc.
- Same inference behavior (integer literals default to i32 when unconstrained)
- Same error messages (or better)
- Same AIR output

Code that compiled before will compile after with the same types.

## Implementation Phases

- [ ] **Phase 1: Type variable infrastructure** - tree1-205.1
  - Add `InferType`, `TypeVarId` types
  - Add type variable allocation and substitution
  - Add constraint types
  - Unit tests for unification algorithm

- [ ] **Phase 2: Constraint generation** - tree1-205.2
  - Add `generate_constraints` that walks RIR
  - Generate constraints for all expression types
  - Preserve span information for error reporting
  - Tests for constraint generation

- [ ] **Phase 3: Unification** - tree1-205.3
  - Implement Algorithm W unification
  - Handle `IntLiteral` special unification rules
  - Apply integer defaulting at the end
  - Error collection and reporting with `Type::Error` recovery
  - Tests for unification edge cases

- [ ] **Phase 4: AIR emission** - tree1-205.4
  - Split current `analyze_inst` into constraint gen + emission
  - Emit AIR with resolved types
  - Verify all existing spec tests pass
  - Remove legacy peek_type and integer_type hacks

- [ ] **Phase 5: Cleanup and stabilization** - tree1-205.5
  - Remove TypeExpectation (no longer needed)
  - Simplify error handling
  - Update documentation
  - Performance testing

## Consequences

### Positive

- **Consistent inference**: No more scattered i32 defaults
- **Principled approach**: Well-understood algorithm with proven properties
- **Better errors**: Full constraint context for diagnostics
- **Foundation for generics**: If we add polymorphism later, the infrastructure exists
- **Simpler mental model**: One algorithm handles all inference

### Negative

- **Two-pass analysis**: Constraint generation + AIR emission (vs current single pass)
- **Memory overhead**: Store constraints and type variable mappings
- **Complexity**: More code in sema.rs initially
- **Learning curve**: HM inference is less intuitive than bidirectional checking

### Neutral

- **Same behavior**: No user-visible semantic changes
- **Same performance**: Two passes, but each is simpler

## Design Decisions

1. **IntLiteral type for integer literals**: Integer literals get a special `IntLiteral` type rather than a type variable with an `IsInteger` constraint. `IntLiteral` can unify with any concrete integer type (i8, i16, i32, i64, u8, u16, u32, u64), and multiple `IntLiteral` types unify with each other (staying as `IntLiteral` until a concrete type forces resolution). Unconstrained `IntLiteral` types default to `i32` at the end. This is more principled - it models exactly what an integer literal is: a value that can become any integer type.

2. **No mixed-width widening**: `1i32 + 2i64` remains an error. Inference never widens types implicitly - explicit conversion is required. This matches Rust's behavior and avoids surprising implicit conversions.

3. **Error recovery**: When unification fails, substitute `Type::Error` for the failing type variable and continue processing remaining constraints. This provides better diagnostics by catching multiple errors in one pass, matching the current behavior.

## Future Work

- **Polymorphic functions**: `fn identity<T>(x: T) -> T` - this ADR lays groundwork
- **Type aliases**: `type Int = i32` - straightforward extension
- **Trait bounds**: `fn print<T: Display>(x: T)` - requires trait system first
- **Associated types**: Complex, depends on traits

## References

- [Principal Type Schemes for Functional Programs](https://dl.acm.org/doi/10.1145/582153.582176) - Damas & Milner, 1982
- [ADR-0002: Single-Pass Bidirectional Types](0002-single-pass-bidirectional-types.md) - Current system
- [Typing Haskell in Haskell](https://web.cecs.pdx.edu/~mpj/thih/) - Jones, 1999
- [Bidirectional Typing](https://arxiv.org/abs/1908.05839) - Dunfield & Krishnaswami
- Rust's type inference implementation (similar hybrid approach)
