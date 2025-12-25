---
id: 0010
title: Destructors
status: proposal
tags: [types, semantics, ownership, memory]
feature-flag: destructors
created: 2025-12-24
accepted:
implemented:
spec-sections: ["3.9"]
superseded-by:
---

# ADR-0010: Destructors

## Status

Proposal

## Summary

Add destructor support to Rue, enabling types to define cleanup logic that runs automatically when values go out of scope. Destructors are required for heap-allocated types like mutable strings where `free()` must be called. We begin with compiler-synthesized destructors for built-in types, with a path to user-defined destructors via the `drop` keyword.

## Context

### Why Destructors Now?

Rue's affine type system (ADR-0008) establishes that types are affine by default: they can be dropped. But currently "dropped" just means "memory is deallocated"—there's no mechanism to run cleanup code.

Mutable strings are the immediate motivator. A mutable string owns a heap-allocated buffer:

```rue
struct String {
    ptr: RawPtr,    // Points to heap allocation
    len: u64,
    capacity: u64,
}
```

When a string goes out of scope, we must call `free(ptr)`. Without destructors, this memory leaks.

### What Exists Today

The compiler already has most infrastructure for destructors:

1. **Affine type tracking**: We know which values are live and when they're consumed
2. **Scope management**: `push_scope()`/`pop_scope()` track variable lifetimes
3. **CFG representation**: Designed for "drop elaboration" (noted in rue-cfg comments)
4. **Move tracking**: We know when values transfer ownership

What's missing:
- Drop instructions in the IR
- CFG pass to insert drops at scope exits
- Codegen to emit drop calls
- Runtime support for built-in type cleanup

### Design Philosophy

We want a path from "built-in types have compiler-synthesized destructors" to "users can define their own destructors." This mirrors the progression:

1. **V1**: Built-in types (String, Vec) have hardcoded cleanup
2. **V2**: User-defined `drop()` method support (provisional syntax)
3. **V3**: Finalize drop API (future ADR, may integrate with traits or other mechanisms)

## Decision

### Drop Semantics

When a value's owning binding goes out of scope for the last time (not moved elsewhere), its destructor runs exactly once:

```rue
fn example() {
    let s = String::new("hello");  // String is allocated
    // ... use s ...
}  // s goes out of scope, destructor runs, memory freed
```

Drop order: **reverse declaration order** (last declared, first dropped):

```rue
fn example() {
    let a = String::new("first");
    let b = String::new("second");
}  // b dropped, then a dropped
```

This matches Rust and C++ (LIFO). It's required for correctness when values may reference each other.

### Types That Need Destructors

**Built-in types that will need destructors (when added):**
- `String` (mutable strings, planned) — frees heap buffer
- `Vec<T>` (future) — drops elements, then frees buffer
- `Box<T>` (future) — drops pointee, then frees memory

Note: None of these types exist yet. This ADR provides the infrastructure they'll need.

**Types without destructors (trivially droppable):**
- All primitives: `i8`..`i64`, `u8`..`u64`, `bool`, `()`
- Arrays of trivially droppable types
- `@copy` structs (implicitly trivially droppable)

**User-defined types:**
- Structs with fields that have destructors need synthesized destructors
- Structs can opt-in to custom destructors (Phase 3)

### Synthesized Destructors

For structs containing non-trivially-droppable fields, the compiler synthesizes a destructor:

```rue
struct Person {
    name: String,  // needs drop
    age: i32,      // trivial
}

// Compiler synthesizes (conceptually):
fn drop_Person(self: Person) {
    drop(self.name);  // String destructor
    // age is trivial, no drop needed
}
```

Fields are dropped in **declaration order** (matches C++, Rust).

### Explicit Drop (V2)

In Phase 3, users can define custom destructors:

```rue
struct FileHandle {
    fd: i32,
}

drop fn FileHandle(self) {
    close(self.fd);
}
```

The syntax `drop fn TypeName(self)` is chosen because:
- `drop` as keyword clearly indicates purpose
- `fn` signals it's a function definition
- Takes `self` by value (consuming)
- No return type (implicitly `()`)

Alternative considered: `impl Drop for T` — deferred pending decisions on traits or other abstraction mechanisms.

### IR Representation

#### AIR: Drop Instruction

Add a `Drop` instruction to AIR:

```rust
enum Inst {
    // ... existing ...

    /// Drop a value, running its destructor if any
    Drop {
        value: InstRef,
        ty: Type,
    },
}
```

The type is needed because we may drop a polymorphic value (in the future).

#### CFG: Drop Placement

The CFG builder inserts `Drop` instructions:

1. **Scope exits**: When leaving a block, drop all live bindings in reverse order
2. **Early returns**: Before each `return`, drop all live bindings in all enclosing scopes
3. **Loops with break**: Before `break`, drop bindings declared inside the loop
4. **Conditionals**: Each branch independently drops its bindings

Example CFG transformation:

```rue
fn example() -> i32 {
    let s = String::new("hello");
    if condition {
        return 42;  // Must drop s here
    }
    let t = String::new("world");
    0
}  // Must drop t then s here
```

Becomes:

```
entry:
    s = String::new("hello")
    branch condition -> then, else

then:
    drop(s)
    return 42

else:
    t = String::new("world")
    drop(t)
    drop(s)
    return 0
```

### Codegen

Drop instructions lower to function calls:

```asm
; drop(s) where s: String
mov rdi, [rbp-8]      ; load s.ptr
call __rue_drop_String
```

For trivially droppable types, the drop is a no-op (elided).

### Runtime Support

Drop functions for built-in types will be added to `rue-runtime` as those types are implemented. The naming convention is `__rue_drop_<TypeName>`. For example, when mutable strings land:

```rust
// Example: what a String drop might look like (when String is added)
#[no_mangle]
pub extern "C" fn __rue_drop_String(s: RawString) {
    if !s.ptr.is_null() && s.capacity > 0 {
        __rue_free(s.ptr, s.capacity);
    }
}
```

Until heap-allocated types exist, the runtime won't need any drop functions.

### Copy Types and Drop

**Critical constraint**: `@copy` types cannot have destructors.

If a type is Copy, it can be duplicated via bitwise copy. If it also had a destructor, the destructor would run multiple times (double-free). Therefore:

```rue
@copy
struct Bad {
    ptr: RawPtr,  // ERROR: @copy type with pointer? dangerous
}
```

We enforce: `@copy` structs can only contain `@copy` fields (already in ADR-0008), and `@copy` types are trivially droppable (no destructor).

### Linear Types and Drop

Linear types (from ADR-0008) **must be explicitly consumed**—they cannot be implicitly dropped:

```rue
linear struct MustConsume { ... }

fn bad() {
    let m = MustConsume { ... };
}  // ERROR: linear value dropped without consumption
```

The destructor mechanism skips linear types; they error at implicit drop points instead.

**Open question**: How does consumption ultimately happen? At the end of the chain, something must run the linear value's destructor. Options include special "sink" functions, an explicit `consume` keyword, or allowing destructors on linear types that run when ownership is transferred to a consuming function. See Open Questions.

### Panic Safety

Rue currently aborts on panic rather than unwinding. This means:
- Destructors do not run on panic (no stack unwinding)
- A panic in a destructor simply aborts

If unwinding is added in the future, destructor behavior during unwinding will need a separate ADR.

## Implementation Phases

Epic: rue-wjha

Following spec-first, test-driven development: each phase writes spec paragraphs, then tests that reference them, then implementation to make tests pass.

### Phase 1: Spec and Infrastructure (rue-wjha.7)

**Spec**: Add destructor chapter to specification (section 3.9 or similar):
- When destructors run (scope exit, not moved)
- Drop order (reverse declaration order)
- Trivially droppable types (primitives, `@copy` structs)
- Types with destructors (structs containing non-trivial fields)

**Tests**: Add spec tests with `preview = "destructors"`:
- Trivially droppable types compile and run (no behavioral change)
- Golden tests showing Drop instructions in AIR/CFG output

**Implementation**:
- Add `Drop` instruction to AIR
- Add `needs_drop()` method to `Type`
- Add drop elaboration pass stub in CFG builder

### Phase 2: Drop Elaboration (rue-wjha.8)

**Spec**: Add paragraphs for:
- Drop at end of block scope
- Drop before early return
- Drop in each branch of conditionals
- Drop before break/continue in loops

**Tests**: Golden tests showing correct Drop placement:
- Simple scope exit
- Early return drops all live bindings
- If/else branches drop their own bindings
- Loop with break drops loop-local bindings

**Implementation**:
- CFG builder inserts `Drop` at scope exits
- Handle early returns, conditionals, loops

### Phase 3: Codegen (rue-wjha.9)

**Spec**: Add paragraphs for:
- Drop calls generated for non-trivial types
- Trivially droppable types elide drop calls

**Tests**:
- Golden tests showing generated assembly calls `__rue_drop_*`
- Tests confirming no drop calls for trivially droppable types

**Implementation**:
- x86_64 backend: emit calls to `__rue_drop_*`
- aarch64 backend: emit calls to `__rue_drop_*`
- Elide drops for trivially droppable types
- Register allocation around drop calls

### Phase 4: User-Defined Destructors (rue-wjha.10)

**Spec**: Add paragraphs for:
- `drop fn TypeName(self)` syntax
- One destructor per type
- Destructor runs after field destructors (or before? decide)

**Tests**:
- Parse `drop fn` syntax
- Error on duplicate destructors
- Error on wrong signature
- User destructor runs at scope exit

**Implementation**:
- Parse `drop fn TypeName(self) { ... }`
- Semantic analysis: validate signature
- Generate drop function, wire into type's destructor

### Phase 5: Integration with Built-in Types (rue-wjha.11)

This phase is deferred until mutable strings or other heap-allocated types land. It will:
- Add `__rue_drop_String` to runtime
- Wire String type to use it
- Verify no memory leaks (valgrind clean)

## Consequences

### Positive

- **Memory safety**: Heap-allocated types clean up automatically
- **RAII pattern**: Enables safe resource management (files, locks, etc.)
- **Path forward**: Built-in to user-defined to trait-based is incremental
- **Predictable**: Drop order is deterministic and documented

### Negative

- **Complexity**: Drop elaboration touches many parts of the compiler
- **Performance**: Drop calls have overhead (mitigated by elision for trivial types)
- **Multi-backend**: Must implement in both x86_64 and aarch64 backends

### Neutral

- **Different from Rust**: We use `drop fn` syntax instead of `impl Drop`
- **Simpler than Rust**: No `Drop` trait until we have traits

## Open Questions

1. **Allocator story**: How do we hook into malloc/free? System allocator? Custom?

2. **Generic drops**: When we have generics, how do we call drop on `T`? Monomorphization? vtable?

3. **Drop during assignment**: Does `x = new_value` drop the old value? (Probably yes.)

4. **Partial initialization**: What if struct construction panics mid-way? (All constructed fields should drop.)

5. **Arrays with destructors**: `[String; 10]` needs to drop 10 strings. How do we track which elements were initialized?

6. **Linear type consumption**: How does consumption ultimately happen for linear types? At the end of the ownership chain, something must finalize the value. Options: (a) special "sink" functions that are allowed to drop linear values, (b) explicit `consume t;` statement, (c) linear types can have destructors that run when passed to a consuming function.

## Future Work

- **Finalize drop API**: The `drop fn` syntax is provisional. A future ADR will decide the final API, potentially integrating with traits or other abstraction mechanisms if they land.
- **Drop flags**: Runtime tracking of whether a value needs drop (for conditional moves)
- **Async drop**: When we have async, dropping across await points
- **Copy types with Drop**: With MVS (no aliasing), maybe possible? Needs research.

## References

- [ADR-0008: Affine Types and MVS](0008-affine-types-mvs.md) — Ownership foundation
- [Rust Drop trait](https://doc.rust-lang.org/std/ops/trait.Drop.html) — Inspiration
- [C++ Destructors](https://en.cppreference.com/w/cpp/language/destructor) — Drop order rules
- [Hylo Deinitialization](https://github.com/hylo-lang/hylo) — MVS approach to cleanup
