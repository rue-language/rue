---
id: 0008
title: Affine Types and Mutable Value Semantics
status: accepted
tags: [types, semantics, ownership]
feature-flag: affine-mvs
created: 2025-12-24
accepted: 2025-12-24
implemented:
spec-sections: ["3.8"]
superseded-by:
---

# ADR-0008: Affine Types and Mutable Value Semantics

## Status

Accepted

## Summary

Introduce an affine type system with mutable value semantics (MVS) to provide memory safety without garbage collection and without a borrow checker. Types are affine by default (can be dropped, cannot be implicitly copied), with opt-in `linear` types that must be consumed, opt-in `Copy` for implicit bitwise copying, and opt-in `Handle` for explicit logical duplication. Mutation is handled via `inout` parameters with projection semantics for collections.

## Context

Rue aims to be a systems programming language with memory safety and higher-level ergonomics than Rust or Zig. The key insight is that we can achieve safety through a different path than Rust's borrow checker:

### The Problem with Borrow Checking

Rust's approach gives excellent safety guarantees but comes with costs:
- Complex lifetime annotations
- Fighting the borrow checker on legitimate patterns
- Steep learning curve
- "Puzzle game" feel when refactoring

### Mutable Value Semantics Alternative

Languages like Val/Hylo and Swift have pioneered mutable value semantics (MVS):
- Everything is a value, passed by logical copy
- Mutation is always local - you own what you mutate
- No references/pointers in the surface language
- Compiler optimizes copies away via copy-on-write or in-place mutation

The key property: **no aliasing**. When you have a value, you have the only handle to it.

### Why Affine Types Complement MVS

MVS eliminates aliasing, but we still need to answer:
1. When can values be implicitly duplicated?
2. When must values be explicitly consumed (not just dropped)?
3. How do we handle resources that require cleanup?

Affine types provide the answer:
- **Affine** (default): Use at most once, can be dropped
- **Linear** (opt-in): Use exactly once, must be consumed
- **Copy** (opt-in): Can be implicitly duplicated (bitwise)
- **Handle** (opt-in): Can be explicitly duplicated (logical copy)

### Interaction with HM Inference

A concern with linear/affine types is their interaction with Hindley-Milner type inference (ADR-0007). The key insight is that with MVS, linearity becomes a **type property** rather than a **usage pattern**:

- No aliasing means no complex alias analysis
- Each value has exactly one owner at any point
- Linearity checking is a simple "was this consumed?" check
- HM infers types normally; linearity is checked separately

This sidesteps the classic HM + linearity tension where you'd need to infer usage patterns from code flow.

## Decision

### Ownership Model

#### Affine by Default

All types are affine by default:
- Can be used zero or one times
- Can be implicitly dropped (destructor runs)
- Cannot be implicitly copied

```rue
struct Point { x: i32, y: i32 }

fn consume(p: Point) { ... }

fn main() {
    let p = Point { x: 1, y: 2 };
    consume(p);     // p is moved
    // consume(p);  // ERROR: p already moved
}
```

#### Opt-in Linear Types

Mark types with `linear` to require explicit consumption:

```rue
linear struct Transaction {
    connection: DbConnection,
}

fn begin() -> Transaction { ... }
fn commit(t: Transaction) { ... }
fn rollback(t: Transaction) { ... }

fn main() {
    let t = begin();
    // ... do work ...
    commit(t);      // OK: t is consumed

    let t2 = begin();
    // ERROR: linear value dropped without being consumed
}
```

Linear types are useful for:
- Resources that must be explicitly closed/released
- Protocol enforcement (state machines)
- Results that must be checked

#### Opt-in Copy (Bitwise)

The `@copy` directive enables implicit bitwise copying:

```rue
@copy
struct Point { x: i32, y: i32 }

fn use_point(p: Point) { ... }

fn main() {
    let p = Point { x: 1, y: 2 };
    use_point(p);   // p is copied
    use_point(p);   // p is copied again - OK!
}
```

Copy semantics:
- Bitwise copy (memcpy)
- Must be "plain old data" - no heap allocations, no file handles
- Implicit at use sites
- Small types only (recommendation, not enforced)

Built-in Copy types: all integer types, `bool`, `char` (when added), tuples/arrays of Copy types.

Note: `@copy` uses the directive system (like `@dbg`). When a full trait system lands, this may migrate to `derive Copy` syntax.

#### Opt-in Handle (Logical Copy)

The `@handle` directive marks types that can be explicitly duplicated:

```rue
@handle
struct Rc<T> {
    ptr: RawPtr,
    // ...
}

// The type must provide a .handle() method
fn Rc_handle(self: Rc<T>) -> Rc<T> {
    // Increment reference count, return new handle
}

fn main() {
    let a: Rc<Data> = Rc::new(data);
    let b = a.handle();  // Explicit: creates new handle
    // Both a and b are valid
}
```

Handle semantics:
- Custom duplication logic (reference counting, interning, etc.)
- Explicit `.handle()` call required
- Can be expensive; explicitness makes cost visible

Why "Handle"? It evokes "getting another handle to the same resource" - appropriate for reference-counted types, interned strings, shared resources.

Note: `@handle` uses the directive system. The type must provide a `.handle()` method. When traits land, this may migrate to `impl Handle for T`.

### Mutation with Inout

#### Basic Inout

The `inout` keyword marks parameters that are mutated and returned to the caller:

```rue
fn increment(x: inout i32) {
    x = x + 1;
}

fn main() {
    var n = 5;
    increment(inout n);  // n is now 6
}
```

Semantics:
- Caller retains ownership, grants temporary exclusive access
- Callee can read and write the value
- Value is "returned" to caller when function returns
- Call site uses `inout` to mirror the declaration (explicit, self-documenting)

#### Inout is Not a Reference

Unlike Rust's `&mut`, `inout` does not create a reference type. It's a calling convention:

```rue
// Rust: takes a reference type &mut i32
fn increment(x: &mut i32) { *x += 1; }

// Rue: inout is a calling convention, not a type
fn increment(x: inout i32) { x = x + 1; }
```

The difference:
- No reference types in Rue's type system
- No lifetimes needed
- Cannot store an inout "reference" - it's not a value

### Projection Semantics for Collections

#### Array Access

Array indexing uses projection semantics (following Hylo):

```rue
fn main() {
    var arr = [1, 2, 3];

    // Read: copies out (i32 is Copy)
    let x = arr[0];

    // Write: mutates in place
    arr[1] = 10;

    // Compound assignment: inout projection
    arr[2] += 1;
}
```

For read access:
- Copy types: value is copied out
- Non-Copy types: ERROR - cannot move out of indexed position

For write/compound access:
- Compiler sees as inout access to the whole array
- Mutation happens in place

#### Law of Exclusivity

Following Hylo, enforce the law of exclusivity statically:
- You can have **either** one inout access **or** multiple read accesses
- Never both simultaneously

```rue
var arr = [1, 2, 3];

// OK: multiple reads
let sum = arr[0] + arr[1];

// OK: single write
arr[0] = 10;

// ERROR: overlapping inout access
swap(&arr[0], &arr[1]);  // Both are inout to same array
```

The solution for swap:

```rue
fn swap_indices(arr: inout Array<i32>, i: usize, j: usize) {
    let tmp = arr[i];
    arr[i] = arr[j];
    arr[j] = tmp;
}
```

#### No Moving Out of Indices

For non-Copy types, you cannot move out of an indexed position:

```rue
struct BigThing { ... }  // not Copy

var arr: Array<BigThing> = [...];

let x = arr[0];           // ERROR: cannot move out of indexed position
let x = arr.take(0);      // OK: explicit removal
let x = arr.swap(0, new); // OK: explicit swap
```

This keeps the mental model simple: arrays always contain valid elements.

### Syntax Summary

```rue
// Affine by default
struct Point { x: i32, y: i32 }

// Opt-in Copy (directive)
@copy
struct Point { x: i32, y: i32 }

// Opt-in Handle (directive + method)
@handle
struct Rc<T> { ... }
fn Rc_handle(self: Rc<T>) -> Rc<T> { ... }

// Opt-in Linear (keyword)
linear struct MustUse { ... }

// Inout parameters
fn mutate(x: inout i32) { ... }

// Call site
mutate(inout value);
```

### Type System Integration

#### Directive Hierarchy

```
         +----------+
         | @handle  |  (explicit .handle())
         +----------+
              ^
              |
         +----------+
         | @copy    |  (implicit bitwise copy)
         +----------+
```

`@copy` implies `@handle` - anything that can be copied can provide a handle via copying.

#### Linear Interaction

Linear types:
- Cannot be `@copy` (implicit copy defeats the point)
- Can be `@handle` if explicit duplication makes sense
- Must have all linear fields consumed for the type to be consumed

```rue
linear struct Transaction { ... }

// ERROR: linear types cannot be @copy
@copy
linear struct Transaction { ... }

// OK: explicit handle might make sense (fork transaction?)
@handle
linear struct Transaction { ... }
```

#### Inference Rules

For function:
```rue
fn example<T>(x: T) -> T { x }
```

HM infers `∀T. T → T`. Linearity checking then verifies:
- If `T` is affine: x used once ✓
- If `T` is linear: x used exactly once ✓
- If `T` is Copy: x can be used freely ✓

The type and linearity are checked separately.

## Implementation Phases

Epic: rue-dfr8

This is a large feature requiring multiple phases. Each phase is a **vertical slice** - a complete, testable feature end-to-end. This allows kicking the tires at each step.

### Phase 1: Affine structs (rue-dfr8.1) ✅ COMPLETE

Make user-defined structs affine by default. Primitives remain implicitly Copy.

- [x] Parse structs as usual (no syntax change)
- [x] Track moves in semantic analysis
- [x] Detect use-after-move errors for structs
- [x] Primitives (i32, bool, etc.) are implicitly Copy - no move tracking
- [x] Field access is a projection (doesn't move the struct)
- [x] Array indexing is a projection (doesn't move the array)
- [x] Comparison operators don't consume operands

**Testable**: Define a struct, use it twice, get a compile error.

```rue
struct Point { x: i32, y: i32 }
fn main() -> i32 {
    let p = Point { x: 1, y: 2 };
    let q = p;      // p is moved
    let r = p;      // ERROR: use of moved value 'p'
    0
}
```

### Phase 2: @copy directive (rue-dfr8.2)

Allow opting structs into Copy semantics.

- [x] Add `@copy` directive parsing (piggyback on directive system)
- [x] `@copy` structs bypass move tracking
- [x] Validate: `@copy` structs can only contain `@copy` fields

**Testable**: Mark a struct `@copy`, use it multiple times without error.

```rue
@copy
struct Point { x: i32, y: i32 }
fn main() -> i32 {
    let p = Point { x: 1, y: 2 };
    let q = p;      // p is copied
    let r = p;      // OK! p is still valid
    r.x
}
```

### Phase 3: Inout parameters (rue-dfr8.3)

Add mutation-without-ownership-transfer.

- Add `inout` keyword in function parameter position
- Add `inout` syntax at call sites
- Exclusive access checking (can't pass same var as two inout args)
- Codegen: pass by pointer internally

**Testable**: Mutate a value through inout parameter.

```rue
fn increment(x: inout i32) {
    x = x + 1;
}
fn main() -> i32 {
    var n = 41;
    increment(inout n);
    n  // 42
}
```

### Phase 4: Linear types (rue-dfr8.4)

Add must-consume semantics.

- Add `linear` keyword for struct definitions
- Check that linear values are consumed (not just dropped)
- Error on implicit drop of linear values
- Linear types cannot be `@copy`

**Testable**: Dropping a linear value without consuming it is an error.

```rue
linear struct MustUse { value: i32 }
fn consume(m: MustUse) -> i32 { m.value }
fn main() -> i32 {
    let m = MustUse { value: 42 };
    consume(m)  // OK: m is consumed
}
// This would error:
// fn bad() { let m = MustUse { value: 1 }; }  // ERROR: linear value dropped
```

### Phase 5: Projection semantics (rue-dfr8.5) ✅ COMPLETE

Add proper array access rules under affine semantics.

- [x] Array read of Copy type: copies out
- [x] Array read of non-Copy type: ERROR (can't move out of index)
- [x] Array write: inout projection to array
- [x] Law of exclusivity for overlapping projections (via existing inout checks)

**Note**: Compound assignment on array elements (`arr[0] += 5`) is not yet implemented (separate parser enhancement needed). Also, accessing fields of struct elements in arrays (`arr[i].field`) has an ICE in codegen (see rue-oqm6).

**Testable**: Can mutate array elements; can't move out non-Copy elements.

```rue
fn main() -> i32 {
    let mut arr = [1, 2, 3];
    arr[0] = 10;        // OK: inout projection
    let x = arr[2];     // OK: i32 is Copy
    x
}
```

### Phase 6: @handle directive (rue-dfr8.6)

Add explicit duplication for reference-counted types.

- Add `@handle` directive parsing
- Types with `@handle` must provide `.handle()` method
- `.handle()` returns a new owned value
- Useful for: Rc, Arc, interned strings, etc.

**Testable**: Call `.handle()` to explicitly duplicate.

```rue
@handle
struct Counter { count: i32 }
fn Counter_handle(self: Counter) -> Counter {
    Counter { count: self.count }
}
fn main() -> i32 {
    let a = Counter { count: 1 };
    let b = a.handle();  // explicit duplication
    b.count
}
```

### Parallel Track: Mutable Strings

The affine type system (particularly Phases 1-3) enables mutable strings as a parallel workstream. Mutable strings need:
- Affine semantics (Phase 1) - strings own their buffer
- Possibly `@handle` (Phase 6) - for cheap string sharing

A separate ADR for mutable strings can begin implementation after Phase 1 lands.

## Consequences

### Positive

- **Memory safety without GC**: Deterministic cleanup, no runtime overhead
- **No borrow checker**: Simpler mental model than Rust's lifetimes
- **Explicit resource handling**: Linear types force proper cleanup
- **Clear copy semantics**: `Copy` vs `Handle` makes cost visible
- **Predictable mutation**: `inout` makes mutation sites obvious
- **HM compatible**: Type inference works naturally

### Negative

- **Less flexible than Rust**: Some patterns that work with borrows need restructuring
- **Verbose for resources**: Linear types require explicit consumption
- **Copy/Handle distinction**: Two concepts where Rust has Clone
- **Projection complexity**: Law of exclusivity can be surprising

### Neutral

- **Different from Rust**: Developers need to learn new idioms
- **Similar to Swift/Hylo**: Can borrow patterns and documentation

## Open Questions

1. **Copy types with Drop**: With MVS (no aliasing), it might be safe to allow Copy types to have destructors. Needs more thought.

2. **Returning inout**: Can a function return an inout projection to part of its input? Hylo allows this but it's complex.

3. **Method syntax**: How does `self` work with inout? Probably `self`, `inout self`, etc.

4. **Partial moves**: Can you move one field out of a struct and keep using other fields? Probably not in V1.

## Future Work

- **Uniqueness types**: For in-place mutation guarantees (Clean-style)
- **Effect system integration**: Track which functions can panic, do I/O, etc.
- **Region-based memory**: Alternative to linear types for some patterns
- **Compile-time capability tracking**: Which resources a function can access

## References

- [Val/Hylo Language](https://www.hylo-lang.org/) - Primary inspiration for MVS
- [Mutable Value Semantics (paper)](https://www.jot.fm/issues/issue_2022_02/article2.pdf) - Theoretical foundation
- [Linear Types Can Change the World](https://www.cs.cmu.edu/~aldrich/papers/sigplan16-linear.pdf) - Linear types survey
- [Rust Ownership](https://doc.rust-lang.org/book/ch04-00-understanding-ownership.html) - What we're avoiding
- [Swift Value Semantics](https://developer.apple.com/swift/blog/?id=10) - Related approach
- [ADR-0007: Hindley-Milner Inference](0007-hindley-milner-inference.md) - Type inference foundation
