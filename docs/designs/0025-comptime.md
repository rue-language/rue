---
id: 0025
title: Compile-Time Execution (comptime)
status: proposal
tags: [compiler, type-system, generics]
feature-flag: comptime
created: 2026-01-01
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0025: Compile-Time Execution (comptime)

## Status

Proposal

## Summary

Introduce a unified compile-time execution model inspired by Zig, where `comptime` marks expressions and parameters that must be evaluated at compile time, and `type` becomes a first-class comptime-only value. This provides the foundation for generics while also enabling powerful compile-time computation as a feature in its own right.

## Context

Rue currently has basic constant expression evaluation (ADR-0003) that handles arithmetic on literals for compile-time bounds checking. However, this is limited:

1. **No user-visible constant declarations**: Users can't define named compile-time constants
2. **No compile-time functions**: Can't run arbitrary code at compile time
3. **No generics**: No way to parameterize functions or types over other types
4. **No type-level computation**: Can't compute types based on other types

Other languages have approached this differently:

- **Rust**: `const fn` with gradually expanding capabilities, separate generics syntax with `<T>`
- **C++**: `constexpr`/`consteval` with template metaprogramming for generics
- **Zig**: Unified `comptime` model where types are first-class values

Zig's approach is elegant because it uses the **same syntax and semantics** for both compile-time computation and generics. A generic function is simply a function with comptime parameters. This reduces conceptual overhead and makes the language more orthogonal.

### Why Zig's Model?

1. **Unified mental model**: One concept (`comptime`) instead of multiple (const fn, generics, macros)
2. **Types as values**: `type` is a comptime-only type, so `fn foo(comptime T: type)` is just a function that takes a type
3. **Same syntax**: Comptime code looks like runtime code, just with `comptime` annotations
4. **Powerful metaprogramming**: Compile-time loops, conditionals, and function calls enable sophisticated code generation

## Decision

### Core Concepts

#### 1. The `comptime` Keyword

`comptime` is a guarantee that an expression will be evaluated at compile time. If evaluation fails (e.g., due to runtime dependencies), it's a compile error.

```rue
// Comptime block - must evaluate at compile time
const SIZE: i32 = comptime { 1024 * 1024 };

// Comptime parameter - caller must provide a comptime-known value
fn repeat(comptime n: i32, value: i32) -> i32 {
    // n is known at compile time, so this loop can be unrolled
    let mut sum = 0;
    let mut i = 0;
    while i < n {
        sum = sum + value;
        i = i + 1;
    }
    sum
}
```

#### 2. The `type` Type

`type` is a comptime-only type whose values are types themselves:

```rue
fn max(comptime T: type, a: T, b: T) -> T {
    if a > b { a } else { b }
}

fn main() -> i32 {
    max(i32, 10, 20)  // T = i32, returns 20
}
```

The value `i32` has type `type`. Since `type` is comptime-only, it can never exist at runtime.

#### 3. Comptime-Only Values

Some values can only exist at compile time:

| Type | Description |
|------|-------------|
| `type` | Type values (e.g., `i32`, `bool`, `MyStruct`) |
| `comptime_int` | Arbitrary-precision integers (future) |
| `comptime_float` | Arbitrary-precision floats (future) |

Attempting to store these in a runtime variable is a compile error:

```rue
fn main() -> i32 {
    let t: type = i32;  // ERROR: type 'type' cannot exist at runtime
    0
}
```

#### 4. Monomorphization

When a function has comptime type parameters, each unique combination of comptime arguments creates a specialized version:

```rue
fn max(comptime T: type, a: T, b: T) -> T {
    if a > b { a } else { b }
}

fn main() -> i32 {
    let x = max(i32, 1, 2);   // Generates max__i32
    let y = max(i64, 3, 4);   // Generates max__i64
    x
}
```

After monomorphization, AIR contains no generic functions - only concrete specialized versions.

### Syntax

#### Comptime Blocks

```rue
comptime { <expr> }
```

The expression inside must be evaluable at compile time. The result replaces the comptime block.

#### Comptime Parameters

```rue
fn name(comptime param: Type, ...) -> ReturnType { ... }
```

Parameters marked `comptime` must be provided with comptime-known values at every call site.

#### Const Items

```rue
const NAME: Type = <expr>;
```

The expression must be comptime-evaluable. If it's not obviously constant, use `comptime { }`:

```rue
const TABLE_SIZE: i32 = comptime { compute_size() };
```

#### Type Parameters

```rue
fn generic(comptime T: type, value: T) -> T { ... }
```

This is just a comptime parameter whose type is `type`.

### Semantics

#### Evaluation Order

Comptime evaluation happens during semantic analysis (Sema), after parsing and before code generation:

```
Source → Lexer → Parser → RIR → Sema (comptime eval here) → AIR → Codegen
```

When Sema encounters a comptime context:
1. It evaluates the expression using the comptime interpreter
2. If successful, the result replaces the original expression
3. If unsuccessful (runtime dependency), emit a compile error

#### Comptime Context Propagation

Inside a comptime context, everything is comptime:

```rue
comptime {
    let x = 1 + 2;      // Comptime evaluation
    let y = x * 3;      // Also comptime
    foo(x, y)           // foo must be callable at comptime
}
```

#### What Can Run at Comptime

Initially (Phase 1-2):
- Arithmetic operations
- Comparisons
- Logical operations
- Variable bindings
- Control flow (if/else, while, loops)
- Function calls (to "pure" functions)

Future extensions:
- Struct/array construction
- Pattern matching
- More complex control flow

#### What Cannot Run at Comptime

- I/O operations
- System calls
- Accessing runtime memory
- Calling functions with side effects
- Operations that would panic (result in compile error instead)

#### Error Handling

Comptime errors are compile errors:

```rue
const X: i32 = comptime { 1 / 0 };  // Compile error: division by zero
```

### Type System Integration

#### ConstValue Extension

The existing `ConstValue` enum will be extended:

```rust
pub enum ConstValue {
    Integer(i64),
    Bool(bool),
    Type(TypeId),      // NEW: For type values
    Unit,              // NEW: For ()
    // Future: Array, Struct, etc.
}
```

#### Type as a Type

A new `Type::ComptimeType` variant represents the `type` type:

```rust
pub enum Type {
    // ... existing variants ...
    ComptimeType,  // The type of types (e.g., `i32` has type `type`)
}
```

#### Comptime Context Tracking

Sema tracks whether it's in a comptime context:

```rust
struct Sema<'a> {
    // ... existing fields ...
    comptime_depth: u32,  // 0 = runtime, >0 = comptime
}
```

Operations check this to know if they must be comptime-evaluable.

### Monomorphization Strategy

When Sema encounters a call to a generic function:

1. **Evaluate comptime args**: All comptime arguments are evaluated to ConstValue
2. **Generate key**: Create a unique key from (function_name, comptime_args)
3. **Check cache**: If this specialization exists, use it
4. **Specialize**: Otherwise, create a new specialized function:
   - Clone the RIR function
   - Substitute comptime parameters with their concrete values
   - Analyze the specialized body
   - Store in specialization cache
5. **Emit call**: The call becomes a call to the specialized function

### Anonymous Struct Types (Future)

Comptime functions can return types, enabling patterns like:

```rue
fn Pair(comptime T: type, comptime U: type) -> type {
    struct {
        first: T,
        second: U,
    }
}

fn main() -> i32 {
    let p: Pair(i32, bool) = Pair(i32, bool) { first: 42, second: true };
    p.first
}
```

This requires:
- Anonymous struct type syntax
- Comptime struct construction
- Type equality based on structural equivalence

This is deferred to Phase 4.

## Implementation Phases

Epic: rue-33lf

### Phase 1: Comptime Expressions (rue-3xoq)

**Goal**: `comptime { expr }` syntax with basic expression evaluation.

- [ ] Add `comptime` keyword to lexer
- [ ] Add `ComptimeBlock` AST/RIR node
- [ ] Add comptime context tracking in Sema
- [ ] Extend `try_evaluate_const()` to handle comptime blocks
- [ ] Gate behind preview flag `comptime`
- [ ] Add spec tests for comptime expressions

**Deliverable**: Users can write `const X: i32 = comptime { 1 + 2 * 3 };`

### Phase 2: Comptime Parameters (Value) (rue-ya9i)

**Goal**: Functions can take comptime value parameters.

- [ ] Add `comptime` parameter modifier to parser
- [ ] Track comptime parameters in function signatures
- [ ] Validate comptime args are comptime-known at call sites
- [ ] Implement function specialization for comptime value params
- [ ] Add spec tests

**Deliverable**: Users can write `fn repeat(comptime n: i32, x: i32) -> i32`

### Phase 3: Type Parameters (rue-fbwv)

**Goal**: The `type` type and comptime type parameters.

- [ ] Add `Type::ComptimeType` variant
- [ ] Add `ConstValue::Type(TypeId)` variant
- [ ] Parse `type` as a type name
- [ ] Implement type parameter substitution in specialization
- [ ] Add spec tests for generic functions

**Deliverable**: Users can write `fn max(comptime T: type, a: T, b: T) -> T`

### Phase 4: Comptime Type Construction (rue-ak9z)

**Goal**: Comptime functions can construct and return types.

- [ ] Anonymous struct type syntax
- [ ] Comptime struct construction
- [ ] Structural type equality
- [ ] Add spec tests

**Deliverable**: Users can write `fn Pair(comptime T: type) -> type { struct { ... } }`

## Consequences

### Positive

- **Unified model**: One concept for const evaluation, metaprogramming, and generics
- **Same syntax**: Comptime code uses normal Rue syntax, no special template language
- **Incremental adoption**: Each phase adds value; Phase 1 is useful standalone
- **Type safety**: Types are first-class values but still statically checked
- **Zero runtime cost**: All comptime computation happens at compile time
- **Foundation for stdlib**: Enables generic `Vec<T>`, `HashMap<K,V>`, etc.

### Negative

- **Compile time increase**: More work at compile time, especially with heavy monomorphization
- **Code bloat**: Each specialization generates new code (mitigated by deduplication later)
- **Complexity**: Sema becomes more complex with comptime interpreter
- **Error messages**: Comptime errors can be confusing (which instantiation failed?)
- **Learning curve**: `comptime` is a new concept for users from Rust/C++

### Neutral

- **Different from Rust**: Users expecting `<T>` syntax will need to learn the new model
- **Supersedes ADR-0003**: The comptime infrastructure subsumes constant evaluation

## Open Questions

1. **Comptime variable declarations**: Should `comptime let x = ...` exist, or only const items?
   - *Tentative answer*: Only const items initially. `comptime { let x = ... }` works inside blocks.

2. **Comptime function annotation**: Should functions be explicitly marked `comptime fn`?
   - *Tentative answer*: No. Any function callable at comptime can be, if all inputs are comptime-known.

3. **Recursion limits**: How do we prevent infinite compile-time recursion?
   - *Tentative answer*: Configurable recursion/iteration limits with reasonable defaults.

4. **Comptime strings**: Should string literals be comptime values?
   - *Tentative answer*: Defer to future work. Focus on numeric types and `type` first.

5. **Trait/interface bounds**: How do we express "T must support +"?
   - *Tentative answer*: Out of scope for this ADR. Will need a separate traits/interfaces design.

## Future Work

These are explicitly out of scope for this ADR:

- **Traits/Interfaces**: Constraining type parameters (e.g., `T: Comparable`)
- **Comptime allocations**: Allocating memory at compile time (like Zig's comptime allocator)
- **Comptime I/O**: Reading files at compile time (`@embedFile` equivalent)
- **Comptime reflection**: Introspecting types at compile time (`@typeInfo` equivalent)
- **Comptime strings**: String manipulation at compile time
- **Associated types**: Types defined in trait implementations
- **Higher-kinded types**: Types parameterized over type constructors

## References

- [ADR-0003: Constant Expression Evaluation](0003-constant-evaluation.md) - Foundation this builds on
- [Zig Language Reference: comptime](https://ziglang.org/documentation/master/#comptime) - Primary inspiration
- [Zig's compile-time reflection](https://ziglang.org/documentation/master/#typeInfo) - Future direction
- [Rust const generics](https://doc.rust-lang.org/reference/items/generics.html#const-generics) - Alternative approach
- [C++ constexpr](https://en.cppreference.com/w/cpp/language/constexpr) - Another alternative

## Appendix: Example Code

### Phase 1: Comptime Expressions

```rue
// Compile-time arithmetic
const BUFFER_SIZE: i32 = comptime { 4 * 1024 };
const FLAGS: i32 = comptime { 1 | 2 | 4 };

fn main() -> i32 {
    // Can use comptime inline too
    let x = comptime { 10 * 10 };
    x + BUFFER_SIZE
}
```

### Phase 2: Comptime Value Parameters

```rue
// Compile-time loop unrolling
fn sum_n(comptime n: i32) -> i32 {
    let mut total = 0;
    let mut i = 0;
    while i < n {
        total = total + i;
        i = i + 1;
    }
    total
}

fn main() -> i32 {
    sum_n(5)  // Unrolled at compile time: 0+1+2+3+4 = 10
}
```

### Phase 3: Generic Functions

```rue
fn swap(comptime T: type, a: T, b: T) -> (T, T) {
    (b, a)
}

fn identity(comptime T: type, x: T) -> T {
    x
}

fn main() -> i32 {
    let (y, x) = swap(i32, 1, 2);
    identity(i32, x + y)
}
```

### Phase 4: Type Construction (Future)

```rue
fn Array(comptime T: type, comptime N: i32) -> type {
    struct {
        data: [T; N],
        len: i32,
    }
}

fn main() -> i32 {
    let arr: Array(i32, 10) = Array(i32, 10) {
        data: [0; 10],
        len: 10,
    };
    arr.len
}
```
