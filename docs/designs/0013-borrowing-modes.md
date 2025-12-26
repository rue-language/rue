---
id: 0013
title: Borrowing Modes
status: implemented
tags: [types, semantics, ownership, borrowing]
feature-flag:
created: 2025-12-25
accepted: 2025-12-25
implemented: 2025-12-25
spec-sections: ["6.1"]
superseded-by:
---

# ADR-0013: Borrowing Modes

## Status

Implemented

## Summary

Extend Rue's parameter passing model with immutable borrowing (`borrow`), completing the ownership capability matrix alongside existing `inout` (mutable borrow) and move semantics. This enables temporary read-only access without consuming values, paralleling Swift's `borrowing`/`consuming`/`inout` and Hylo's `let`/`sink`/`inout`/`set` conventions.

## Context

### Current State

Rue (ADR-0008) currently has:
- **Move** (default): Values are consumed when passed to functions
- **Copy** (`@copy`): Values are implicitly duplicated (bitwise)
- **Inout** (in progress): Mutable access without ownership transfer

What's missing: **immutable borrowing** - temporary read-only access without consuming the value.

### The Problem

Consider this scenario:

```rue
struct BigData { /* lots of fields */ }

fn print_summary(data: BigData) {
    // Just reads data, doesn't modify it
    print(data.size);
}

fn main() {
    let d = BigData { ... };
    print_summary(d);  // d is MOVED - we can't use it anymore!
    // let result = process(d);  // ERROR: d was moved
}
```

Currently the workarounds are:
1. Make `BigData` a `@copy` type (bad: copies are expensive)
2. Add `inout` (bad: semantic lie - we're not mutating)
3. Return the value from the function (bad: awkward API)

We need a way to say "borrow this temporarily for reading."

### Prior Art

#### Swift (SE-0377)

Swift has three parameter ownership modifiers:
- `consuming` - takes ownership (like Rue's default)
- `borrowing` - temporary read-only access (what we want)
- `inout` - temporary exclusive mutable access (what we have)

```swift
func analyze(_ data: borrowing BigData) { ... }
```

Key insight: `borrowing` parameters can't be copied or escaped implicitly; you must explicitly copy if needed.

#### Hylo

Hylo has four parameter passing conventions:
- `let` (default) - immutable borrow, cannot escape
- `sink` - takes ownership (consuming)
- `inout` - mutable borrow
- `set` - initializes uninitialized storage

```hylo
fun analyze(_ data: let BigData) { ... }
```

Hylo's `let` is exactly what we want: the Rust developer "can understand a `let` parameter as a pass by immutable borrow, with exactly the same guarantees."

#### Rust

Rust distinguishes:
- `&T` - shared immutable reference
- `&mut T` - exclusive mutable reference
- `T` - ownership transfer

With lifetimes tracking how long references are valid.

### Capability Matrix

Drawing from boats' blog posts on pinning, there's a matrix of capabilities for values:

| Capability | Move | @copy | inout | borrow |
|------------|------|-------|-------|--------|
| Can read | yes | yes | yes | yes |
| Can mutate | yes | yes | yes | no |
| Can move out | yes | yes | no | no |
| Can escape | yes | yes | no | no |
| Implicit copy | no | yes | no | no |
| Exclusive access | yes | yes | yes | no |

The key distinction between `borrow` and `inout`:
- `borrow`: shared read-only access (multiple simultaneous borrows OK)
- `inout`: exclusive mutable access (only one at a time)

### Future: Pinning Considerations

The boats' posts on pinning reveal another dimension: **moveability**. A pinned value:
- Cannot be moved in memory
- Useful for self-referential types, async futures, intrusive data structures

This suggests a future extension:

| Capability | Normal | Pinned |
|------------|--------|--------|
| Can relocate in memory | yes | no |
| Can create self-references | dangerous | safe |

For now, we're deferring pinning. But the `borrow`/`inout` distinction lays groundwork: pinned places would be "borrow that also guarantees address stability."

## Decision

### Syntax

Add `borrow` as a parameter passing mode keyword:

```rue
fn analyze(borrow data: BigData) -> i32 {
    data.field1 + data.field2
}

fn main() -> i32 {
    let d = BigData { field1: 10, field2: 32 };
    let result = analyze(borrow d);  // d is borrowed, not moved
    result + d.field1  // d is still valid!
}
```

Like `inout`, the `borrow` keyword appears at both declaration and call site for explicitness.

### Semantics

#### Borrowing Rules

1. **Cannot mutate**: A borrowed value cannot be modified
2. **Cannot move out**: Cannot transfer ownership of a borrowed value
3. **Cannot escape**: Cannot store a borrow beyond the function's scope
4. **Multiple borrows OK**: Can have multiple simultaneous borrows of the same value
5. **No borrow during inout**: Cannot borrow a value while an `inout` reference to it exists

```rue
fn read1(borrow x: BigData) -> i32 { x.field }
fn read2(borrow x: BigData) -> i32 { x.field * 2 }

fn main() -> i32 {
    let d = BigData { field: 21 };

    // OK: multiple simultaneous borrows
    let a = read1(borrow d);
    let b = read2(borrow d);

    // OK: borrow after borrow
    let c = read1(borrow d);

    a + b + c  // d still valid
}
```

#### Law of Exclusivity

Rue enforces exclusivity statically (following Hylo):

```rue
fn mutate(inout x: i32) { x = x + 1; }
fn read(borrow x: i32) -> i32 { x }

fn main() -> i32 {
    let mut n = 10;

    // OK: sequential access
    mutate(inout n);
    let x = read(borrow n);

    // ERROR: cannot borrow while inout is active
    // (This would need to be in the same expression to conflict)

    x
}
```

The key rule: **either** one `inout` **or** any number of `borrow` accesses, never both simultaneously.

#### Field Borrowing

Borrowing a struct allows borrowing its fields:

```rue
struct Pair { a: i32, b: i32 }

fn read_a(borrow p: Pair) -> i32 { p.a }

fn main() -> i32 {
    let p = Pair { a: 1, b: 2 };
    read_a(borrow p)  // Borrows p, accesses p.a
}
```

This is projection: borrowing the whole gives access to the parts.

### Non-Escaping Property

A critical property: borrows cannot escape their scope.

```rue
// ERROR: cannot return a borrow
fn bad(borrow x: BigData) -> BigData {
    x  // ERROR: cannot move out of borrowed value
}

// ERROR: cannot store in a field
struct Holder { data: BigData }
fn also_bad(borrow x: BigData) -> Holder {
    Holder { data: x }  // ERROR: x would escape
}
```

This is why we don't need lifetime annotations like Rust: borrows are always scoped to the function call.

### Copy Types and Borrowing

For `@copy` types, `borrow` is still meaningful but may be optimized:

```rue
@copy
struct Point { x: i32, y: i32 }

fn read_point(borrow p: Point) -> i32 { p.x + p.y }
```

The compiler may pass small `@copy` types by value even when `borrow` is used, as an optimization. The semantics remain the same.

### Return Type Implications

Functions cannot return borrowed values (no lifetime tracking):

```rue
// OK: returns owned value
fn make() -> BigData { BigData { ... } }

// OK: returns copy of a field
fn get_field(borrow x: BigData) -> i32 { x.field }

// ERROR: cannot return borrowed value
fn identity(borrow x: BigData) -> BigData { x }  // ERROR

// ERROR: cannot return borrow mode
fn borrow_identity(borrow x: BigData) -> borrow BigData { x }  // Not valid syntax
```

This is intentional: we avoid Rust's lifetime complexity by making borrows purely scoped.

### Comparison with Alternatives

#### Why not just use `inout` for reads?

Using `inout` for read-only access:
1. **Semantic lie**: The type says "I will mutate this" but you don't
2. **Exclusivity cost**: Can't have multiple readers
3. **API documentation failure**: Callers can't tell if mutation actually happens

#### Why not implicit borrow?

We could automatically borrow when safe (like Hylo's `let` default). Arguments against:
1. **Explicitness**: Call site shows intent clearly
2. **Transition path**: Easier to add implicit later than remove
3. **Consistency**: Matches `inout` style

We may revisit this and make `borrow` the default for non-Copy types in a future version.

### Grammar Changes

```ebnf
param_mode = "inout" | "borrow" ;
param = [ param_mode ] IDENT ":" type ;
call_arg = [ param_mode ] expr ;
```

### Type System Integration

Borrowing is a calling convention, not a type constructor:

```rue
// This is NOT valid - borrow is not a type
let x: borrow BigData = ...;  // ERROR

// Borrow is a parameter mode
fn f(borrow x: BigData) { ... }  // OK
```

This keeps the type system simple and avoids lifetime complexity.

## Implementation Phases

Epic: rue-7lii

- [x] **Phase 1: Parser support** - rue-7lii.1 - Add `borrow` keyword, parse in parameter declarations and call sites
- [x] **Phase 2: Semantic analysis** - rue-7lii.2 - Enforce immutability, prevent move-out, non-escaping check, exclusivity
- [x] **Phase 3: Codegen** - rue-7lii.3 - Pass borrowed values by pointer, generate read-only accesses
- [x] **Phase 4: Specification and tests** - rue-7lii.4 - Add spec sections, comprehensive tests

## Consequences

### Positive

- **Complete ownership story**: Read, mutate, and consume are all expressible
- **Better APIs**: Functions can express "I only read this"
- **Performance**: Large structs passed by reference without copying
- **Safety**: Compiler prevents mutation of borrowed values
- **No lifetimes**: Scoped borrows avoid Rust's complexity
- **Explicit**: Call sites document borrowing intent

### Negative

- **More keywords**: Another parameter mode to learn
- **Verbosity**: Must write `borrow` at call sites
- **No returned borrows**: Can't return views/slices (need different approach)
- **Future constraint**: If we add lifetimes later, migration may be complex

### Neutral

- **Similar to Swift/Hylo**: Developers from those languages will find it familiar
- **Different from Rust**: No `&T` reference types

## Open Questions

1. **Default mode for non-Copy types?** Could make `borrow` the default instead of move, with explicit `take`/`sink`/`consume` for ownership transfer. This matches Hylo. Defer to future ADR.

2. **Method syntax?** How does `self` work with borrowing?
   - `fn method(self)` - takes ownership
   - `fn method(borrow self)` - borrows self
   - `fn method(inout self)` - mutates self

   This seems natural but needs detailed design.

3. **Field borrowing granularity?** Can you borrow different fields simultaneously?
   ```rue
   fn f(borrow a: i32, inout b: i32) { ... }
   let mut s = Struct { a: 1, b: 2 };
   f(borrow s.a, inout s.b);  // Is this OK?
   ```
   Hylo allows this. We should too, but it needs careful analysis.

4. **Interaction with arrays?** Borrowing an array element:
   ```rue
   let arr = [big1, big2, big3];
   analyze(borrow arr[0]);  // Borrow one element?
   ```
   This is projection semantics (ADR-0008 Phase 5).

## Future Work

- **Pinned places**: Add `pinned` mode for address-stable borrows (useful for async, self-referential types)
- **Default borrow**: Consider making `borrow` the default for non-Copy types
- **Returned projections**: Hylo-style subscripts that return projections
- **Partial borrows**: Borrow different fields of a struct simultaneously

## References

- [ADR-0008: Affine Types and Mutable Value Semantics](0008-affine-types-mvs.md) - Foundation this builds on
- [SE-0377: borrowing and consuming parameter modifiers](https://github.com/swiftlang/swift-evolution/blob/main/proposals/0377-parameter-ownership-modifiers.md) - Swift's approach
- [Hylo Language Tour: Functions](https://docs.hylo-lang.org/language-tour/functions-and-methods) - Hylo's `let`/`sink`/`inout`/`set`
- [Pin (boats' blog)](https://without.boats/blog/pin/) - Capability matrix thinking
- [Pinned Places (boats' blog)](https://without.boats/blog/pinned-places/) - Place-based view of capabilities
