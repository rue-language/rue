---
id: 0029
title: Anonymous Struct Methods (Zig-Style)
status: proposal
tags: [types, methods, comptime, generics]
feature-flag: anon_struct_methods
created: 2026-01-03
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0029: Anonymous Struct Methods (Zig-Style)

## Status

Proposal

## Summary

Enable method definitions inside anonymous struct type expressions, following Zig's approach. This allows generic data structures built with comptime to have methods, completing the foundation for `Vec<T>`, `HashMap<K,V>`, and similar standard library types.

## Context

### The Problem

Currently, anonymous struct types created via comptime cannot have methods attached:

```rue
fn Vec(comptime T: type) -> type {
    struct { ptr: u64, len: u64, cap: u64 }
}

fn main() -> i32 {
    let v: Vec(i32) = Vec(i32) { ptr: 0, len: 0, cap: 0 };
    // v.push(42);  // ERROR: no method 'push' on this type
    0
}
```

This severely limits the usefulness of comptime type construction for building generic collections.

### Current State

- **Named struct methods (ADR-0009)**: Fully implemented via `impl` blocks
- **Comptime Phase 1-3 (ADR-0025)**: Implemented - type parameters and monomorphization work
- **Comptime Phase 4**: Anonymous structs can be created and instantiated, but cannot have methods
- **Method lookup**: Uses `(struct_name_symbol, method_name_symbol)` key, which doesn't work for anonymous structs with generated names like `__anon_struct_<id>`

### Alternatives Considered

1. **Free functions with type parameters**: `fn vec_push(comptime T: type, v: borrow Vec(T), item: T)`
   - Already works today
   - Verbose and less ergonomic
   - No method chaining

2. **Rust-style impl blocks on type constructors**: `impl Vec(T) { fn push(...) }`
   - Requires new syntax for referencing type constructors
   - Timing issues: impl blocks processed at declaration time, but anonymous structs created during analysis
   - More complex to implement

3. **Zig-style inline methods** (chosen): Methods defined inside the struct literal
   - Natural fit for Rue's Zig-inspired comptime model
   - Methods travel with the type definition
   - No timing issues - methods are part of the type expression
   - Matches user expectation from Zig

## Decision

### Syntax

Allow function definitions inside anonymous struct type expressions:

```rue
fn Vec(comptime T: type) -> type {
    struct {
        ptr: u64,
        len: u64,
        cap: u64,

        fn new() -> Self {
            Self { ptr: 0, len: 0, cap: 0 }
        }

        fn push(self, item: T) -> Self {
            // implementation...
            self
        }

        fn len(self) -> u64 {
            self.len
        }
    }
}

fn main() -> i32 {
    let v = Vec(i32)::new();
    let v2 = v.push(42);
    v2.len() as i32
}
```

### The `Self` Type

Inside an anonymous struct's method definitions, `Self` refers to the anonymous struct type being defined:

```rue
fn Pair(comptime T: type) -> type {
    struct {
        first: T,
        second: T,

        fn swap(self) -> Self {
            Self { first: self.second, second: self.first }
        }
    }
}
```

`Self` is resolved during semantic analysis when the anonymous struct type is created.

### Method Storage and Lookup

**Key insight**: Anonymous structs are deduplicated by structural equality (same fields = same type). Methods must be part of this structural identity.

**Design**: Methods are stored with the struct definition, keyed by `StructId`:

```rust
// In Sema
methods: HashMap<(StructId, Spur), MethodInfo>  // StructId instead of struct name Spur
```

When an anonymous struct is created:
1. Check if a structurally-equivalent struct already exists (fields match)
2. If methods differ, they are NOT the same type (structural equality includes methods)
3. Register methods in the method table using `StructId`

### Structural Equality

Two anonymous structs are the same type if and only if:
1. Same field names, types, and order
2. Same method names and signatures (parameter types and return type)

Method bodies do NOT affect structural equality - only signatures matter.

```rue
fn A() -> type {
    struct { x: i32, fn get(self) -> i32 { self.x } }
}

fn B() -> type {
    struct { x: i32, fn get(self) -> i32 { self.x + 1 } }  // Same type as A()!
}

fn C() -> type {
    struct { x: i32, fn get(self) -> i64 { self.x as i64 } }  // DIFFERENT type (i64 vs i32)
}
```

### Associated Functions

Functions without `self` are associated functions, called with `Type::function()` syntax:

```rue
fn Point(comptime T: type) -> type {
    struct {
        x: T,
        y: T,

        fn origin() -> Self {
            Self { x: 0, y: 0 }
        }
    }
}

fn main() -> i32 {
    let p = Point(i32)::origin();
    p.x
}
```

### Comptime Parameter Access

Methods can reference comptime parameters from the enclosing function:

```rue
fn Array(comptime T: type, comptime N: i32) -> type {
    struct {
        data: [T; N],

        fn capacity(self) -> i32 {
            N  // Captured from enclosing comptime context
        }
    }
}
```

This is handled naturally by monomorphization - each specialization captures concrete values for `T` and `N`.

## Implementation Phases

Epic: rue-nj40

### Phase 1: Parser & AST (rue-nj40.1) ✅

- [x] Add `methods: Vec<Method>` to `TypeExpr::AnonymousStruct` in AST
- [x] Update Chumsky parser to accept `fn` inside `struct { ... }`
- [x] Add `Self` as a special type name in anonymous struct context
- [x] Add `SelfType` token to lexer and `Self { ... }` struct literal expression
- [x] Add preview gate `anon_struct_methods`
- [x] Unit tests for parsing

**Deliverable**: Parser accepts `struct { x: i32, fn get(self) -> i32 { self.x } }` syntax.

### Phase 2: RIR Generation (rue-nj40.2) ✅

- [x] Extend `InstData::AnonStructType` to include method references
- [x] Store anonymous struct methods in RIR extra data
- [x] Generate RIR for methods inside anonymous structs
- [x] Handle `Self` type reference in RIR

**Deliverable**: RIR correctly represents anonymous structs with methods.

### Phase 3: Semantic Analysis (rue-nj40.3) 🔶

- [x] Change method lookup key from `(Spur, Spur)` to `(StructId, Spur)`
- [x] Register methods when creating anonymous struct types
- [x] Resolve `Self` to the anonymous struct's `StructId` (in signatures only)
- [ ] Update structural equality to include method signatures
- [ ] Handle comptime parameter capture in method bodies
- [x] Analyze method bodies with `self` in scope
- [ ] Resolve `Self` in method body expressions

**Status**: Partial - method registration works, `self` in method bodies works, but `Self` type resolution is incomplete. See rue-h6zn for remaining work.

**Deliverable**: `v.push(42)` compiles when `v` is an anonymous struct type with a `push` method.

### Phase 4: Specification & Tests (rue-nj40.4) ✅

- [x] Add spec section for anonymous struct methods (4.14:10-15)
- [x] Add comprehensive spec tests (17 tests, preview-gated)
- [x] Add UI tests for error messages (2 tests)
- [x] Traceability coverage for all spec paragraphs (100% normative)

**Note**: Tests are in place but preview tests fail because Phase 3 is incomplete.

**Deliverable**: Full test coverage and specification documentation.

## Consequences

### Positive

- **Enables generic collections**: `Vec<T>`, `HashMap<K,V>` become possible with ergonomic APIs
- **Consistent with Zig model**: Methods travel with type definitions, no separate impl blocks needed
- **No timing issues**: Methods are part of the type expression, analyzed together
- **Natural for comptime**: Comptime parameters are in scope inside methods

### Negative

- **Structural equality complexity**: Must now compare method signatures, not just fields
- **Parser complexity**: `struct { ... }` can now contain both fields and functions
- **Larger anonymous struct representation**: Methods add to the AST/RIR size
- **Learning curve**: Users expecting Rust's `impl` blocks will need to adapt

### Neutral

- **Named structs unchanged for now**: `impl` blocks still work for named structs, but may be deprecated in favor of inline methods in the future
- **No runtime cost**: Methods are still just functions with receiver as first argument

## Open Questions

1. **Should `impl` blocks be disallowed for anonymous structs?**

   With inline methods, external `impl` blocks for anonymous structs would be redundant and confusing. We could either:
   - Allow both (more flexibility, more confusion)
   - Disallow external `impl` for anonymous structs (cleaner, but limits extensibility)

   **Decision**: Disallow external `impl` for anonymous structs. In fact, consider eventually removing `impl` blocks entirely in favor of inline methods for all structs. This would make Rue more consistent with Zig's model and simplify the language. Named structs would define methods inline:

   ```rue
   struct Point {
       x: i32,
       y: i32,

       fn origin() -> Self { Self { x: 0, y: 0 } }
       fn distance(self) -> i32 { self.x * self.x + self.y * self.y }
   }
   ```

2. **Method visibility inside anonymous structs?**

   Should `pub fn` vs `fn` matter for methods inside anonymous structs?

   **Decision**: Support `pub fn` vs `fn` from the start. This maintains consistency with the module system and follows the principle that visibility should be explicit.

3. **Generic methods inside anonymous structs?**

   Should methods have their own comptime parameters?
   ```rue
   fn Container(comptime T: type) -> type {
       struct {
           value: T,
           fn map(self, comptime U: type, f: ???) -> Container(U) { ... }
       }
   }
   ```

   **Decision**: Generic methods are desirable but blocked by the lack of function type syntax in Rue. The `f: fn(T) -> U` syntax shown above is not legal Rue - there's no function pointer or closure type yet. This should be addressed in a separate ADR for function types/closures. Once function types exist, generic methods become straightforward to add.

## Future Work

- **Inline methods for named structs**: Extend this syntax to named structs, potentially deprecating `impl` blocks
- **Function types ADR**: Design function pointer / closure types (needed for generic methods like `map`)
- **Generic methods**: Once function types exist, allow `fn map(self, comptime U: type, f: Fn(T) -> U) -> Container(U)`
- **Trait implementation for anonymous structs**: `impl Trait for Vec(T) { ... }`
- **Destructor methods**: `fn drop(self)` inside anonymous structs

## References

- [ADR-0009: Struct Methods](0009-struct-methods.md) - Foundation for method implementation
- [ADR-0025: Compile-Time Execution](0025-comptime.md) - Comptime infrastructure this builds on
- [Zig Language Reference: struct](https://ziglang.org/documentation/master/#struct) - Inspiration for inline method syntax
- [rue-nj40](https://github.com/...) - Original issue tracking this feature
