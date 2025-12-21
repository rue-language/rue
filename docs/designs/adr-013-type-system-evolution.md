# ADR-013: Type System Evolution Overview

## Status

Proposed

## Context

Rue aims to be a systems programming language that is:
- **Memory safe** without garbage collection
- **Higher level** than Rust (less annotation burden)
- **Lower level** than Go (direct hardware control, no runtime)

The current type system has:
- Primitive types (integers, bool, unit, never)
- Structs (nominal, with named fields)
- Fixed-size arrays `[T; N]`
- Variable-level mutability (`let mut`)
- No generics or type parameters
- No lifetime annotations

To achieve memory safety without Rust's complexity, we need to evolve the type system while keeping it approachable.

## Decision

We will evolve Rue's type system in phases, with these key design principles:

### 1. Explicit Ownership Modes

Instead of Rust's implicit Copy vs Move distinction (determined by trait implementation), Rue will have **explicit ownership mode declarations** on types:

| Mode | Copying | Aliasing | Must Consume? | Use Case |
|------|---------|----------|---------------|----------|
| `value` | Deep copy | No | No | Small data, primitives |
| `move` | Transfer | No | No (affine) | Buffers, unique handles |
| `linear` | Transfer | No | **Yes** | Files, locks, transactions |
| `rc` | Ref increment | Shared | No | Graphs, shared config |

This is "Rust on steroids" - more explicit, more options, but each mode is simpler to understand in isolation.

### 2. Mutable Value Semantics (MVS)

Adopt the Val/Hylo approach:
- Values are independent by default (value semantics)
- `inout` parameters for mutation without copies
- Law of exclusivity (one mutable access at a time)
- No lifetime annotations - the compiler enforces safety through simpler rules

This gives us Rust-like safety guarantees without the borrow checker's learning curve.

### 3. Comptime-Implemented Generics

Rather than a separate generics system with trait bounds, implement generics via compile-time evaluation:

```rue
// Surface syntax looks like generics
fn max<T>(a: T, b: T) -> T { ... }

// Implemented as comptime
fn max(comptime T: type, a: T, b: T) -> T { ... }
```

Advantages:
- One mechanism instead of two (no separate "const generics")
- Trait resolution is essentially an interpreter anyway
- More powerful (full language available at comptime)
- Simpler mental model

### 4. Generators as Syntax Sugar

For iterators and lazy sequences, use generator syntax that compiles to state machines:

```rue
fn* range(start: i32, end: i32) -> i32 {
    let i = start;
    while i < end {
        yield i;
        i = i + 1;
    }
}
```

This provides practical async/iteration support without requiring a full effect system.

### 5. Defer Full Effect System

Effects (allocation, panic, IO) will not be tracked in the type system initially. We may add opt-in effect restrictions later (e.g., `!Alloc`, `!Panic`) as an advanced feature.

## Implementation Sequence

The features should be implemented in this order, as each builds on the previous:

### Phase 1: Ownership Modes (ADR-014)
- Add `value`, `move`, `linear`, `rc` keywords for type declarations
- Implement move semantics and use-after-move checking
- Implement linear type must-use checking
- Implement reference counting runtime support

**Prerequisite:** Current type system
**Enables:** Safe resource management, foundation for MVS

### Phase 2: Mutable Value Semantics (ADR-015)
- Add `inout` parameter passing mode
- Implement exclusivity checking (no aliased mutation)
- Optimize away copies where possible

**Prerequisite:** Ownership modes (to know how types behave)
**Enables:** Efficient mutation without borrow checker complexity

### Phase 3: Comptime (ADR-016)
- Implement comptime evaluation (interpreter or compile to host)
- Add `comptime` keyword for compile-time parameters
- Implement type-as-value semantics
- Add comptime blocks and assertions

**Prerequisite:** MVS (comptime needs stable semantics to evaluate)
**Enables:** Generics, type-level computation, array bounds

### Phase 4: Generators (ADR-017)
- Add `fn*` generator function syntax
- Add `yield` expression
- Implement state machine transformation
- Design iterator interface using comptime generics

**Prerequisite:** Comptime (for generic iterator types)
**Enables:** Lazy iteration, structured concurrency foundation

## Consequences

### Positive

- **Memory safety without GC**: Ownership modes + MVS provide static safety guarantees
- **Lower annotation burden than Rust**: No lifetimes, simpler borrow rules
- **Explicit control**: Programmers choose the right semantics for each type
- **Powerful abstraction**: Comptime provides full generics capability
- **Practical iteration**: Generators make iterators ergonomic

### Negative

- **Learning curve**: Four ownership modes is more than Rust's two
- **Implementation complexity**: Comptime requires an interpreter
- **Limited effect tracking**: No static guarantees about allocation/panic

### Risks

- **Comptime complexity**: Could become as complex as Rust's trait system if not careful
- **Mode proliferation**: Need to resist adding more ownership modes
- **Deferred effects**: May need to retrofit effect tracking later

## Related ADRs

- ADR-014: Ownership Modes
- ADR-015: Mutable Value Semantics
- ADR-016: Comptime and Type-Level Computation
- ADR-017: Generators and Iterators

## References

- [Val Language](https://www.val-lang.dev/) - Mutable value semantics
- [Hylo](https://github.com/hylo-lang/hylo) - Val's successor
- [Austral](https://austral-lang.org/) - Linear types for systems programming
- [Zig](https://ziglang.org/) - Comptime approach
- [Koka](https://koka-lang.github.io/) - Effect system (for future reference)
