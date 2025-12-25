---
id: 0009
title: Struct Methods
status: proposal
tags: [types, syntax]
feature-flag: methods
created: 2025-12-24
accepted:
implemented:
spec-sections: []
superseded-by:
---

# ADR-0009: Struct Methods

## Status

Proposal

## Summary

Add the ability to define methods on structs using `impl` blocks, allowing method call syntax (`obj.method(args)`) as an ergonomic alternative to free functions.

## Context

Rue currently supports structs with fields, but all operations on structs must be implemented as free functions that take the struct as a parameter:

```rue
struct Point { x: i32, y: i32 }

fn distance_from_origin(p: Point) -> i32 {
    // calculate distance
}

fn main() -> i32 {
    let p = Point { x: 3, y: 4 };
    distance_from_origin(p)
}
```

This pattern is verbose and doesn't scale well as the number of struct-specific operations grows. Method syntax provides several benefits:

1. **Discoverability**: Methods are associated with types, making it easier to find relevant operations
2. **Namespacing**: Methods don't pollute the global function namespace
3. **Ergonomics**: Method chaining becomes possible (`point.translate(1, 2).scale(2)`)
4. **Self reference**: Methods can implicitly reference their receiver

## Decision

### Syntax

We will add `impl` blocks that contain method definitions:

```rue
struct Point { x: i32, y: i32 }

impl Point {
    fn distance_from_origin(self) -> i32 {
        // self.x and self.y are accessible
        self.x * self.x + self.y * self.y
    }

    fn translate(self, dx: i32, dy: i32) -> Point {
        Point { x: self.x + dx, y: self.y + dy }
    }
}

fn main() -> i32 {
    let p = Point { x: 3, y: 4 };
    p.distance_from_origin()
}
```

### Self Parameter

Methods take `self` as their first parameter, representing the receiver:

- `self` - takes ownership of the receiver (move semantics)

For the initial implementation, only by-value `self` is supported, matching Rue's current copy-by-default semantics for structs.

Additional receiver types will be added as part of [ADR-0008: Affine Types and Mutable Value Semantics](0008-affine-types-mvs.md):
- `inout self` - mutable projection (Phase 3 of ADR-0008)

### Associated Functions (Static Methods)

Functions in `impl` blocks without a `self` parameter are associated functions (like Rust's associated functions):

```rue
impl Point {
    fn origin() -> Point {
        Point { x: 0, y: 0 }
    }
}

fn main() -> i32 {
    let p = Point::origin();
    p.x
}
```

These are called with `Type::function()` syntax.

### Method Resolution

When encountering `expr.method(args)`:

1. Evaluate `expr` to determine its type
2. If the type is a struct, look up `method` in that struct's impl block
3. If found, desugar to a call with `expr` as the first argument
4. If not found, emit an error

### Multiple Impl Blocks

Multiple `impl` blocks for the same struct are allowed and their methods are merged:

```rue
impl Point {
    fn x(self) -> i32 { self.x }
}

impl Point {
    fn y(self) -> i32 { self.y }
}
```

Duplicate method names across impl blocks are an error.

### No Orphan Methods

Methods can only be defined for structs in the same compilation unit. (This is automatic since we compile single files currently.)

## Implementation Phases

- [x] **Phase 1: Parsing** - rue-qs3z.1
  - Add `impl` keyword to lexer
  - Parse `impl Type { fn... }` blocks
  - Add `Item::ImplBlock` to AST
  - Parse method calls as a variant of field access

- [x] **Phase 2: RIR Generation** - rue-qs3z.2
  - Add method info to RIR (ImplDecl, MethodCall, AssocFnCall instructions)
  - Generate RIR for impl blocks
  - Handle method calls in expression generation
  - Parse associated function calls (Type::fn() syntax)

- [x] **Phase 3: Type Checking** - rue-qs3z.3
  - Add method registry to struct definitions
  - Type check impl blocks
  - Resolve method calls to their definitions
  - Handle `self` parameter binding
  - Method resolution requires:
    1. Looking up the receiver type
    2. Finding the impl block for that type
    3. Resolving the method name
    4. Type checking the arguments against method signature

- [x] **Phase 4: Code Generation** - rue-qs3z.4
  - Lower method calls to regular calls with receiver as first argument
  - Update both x86_64 and aarch64 backends
  - Handle associated function calls (`Type::method()`)

- [ ] **Phase 5: Specification & Tests** - rue-qs3z.5
  - Add spec chapter 6.4 for impl blocks and methods
  - Add comprehensive spec tests
  - Ensure traceability coverage

## Consequences

### Positive

- More ergonomic API design for struct operations
- Method chaining becomes possible
- Better code organization with type-associated functions
- Familiar syntax for developers from Rust, Swift, or similar languages

### Negative

- Adds complexity to name resolution
- Parser needs to distinguish method calls from field access
- Two ways to call the same operation (method vs function)

### Neutral

- Methods are syntactic sugar over functions - no runtime cost
- No impact on existing struct field access semantics

## Open Questions

1. **Should associated functions require `Self` as return type annotation?**

   In Rust, `Self` is an alias for the implementing type. We could add this for consistency, or keep it simple and require the explicit type name.

   **Decision**: Defer `Self` keyword to future work. Use explicit type names for now.

2. **Order of impl blocks vs struct definition?**

   Should impl blocks be required to come after the struct definition, or can they appear anywhere in the file?

   **Decision**: Impl blocks must come after the struct definition they implement (same file, after the struct item).

## Future Work

- Additional receiver types (`inout self`) - see [ADR-0008](0008-affine-types-mvs.md)
- `Self` type alias in impl blocks
- Trait methods and default implementations
- Method visibility (`pub fn` vs `fn`)
- Generic methods
- Universal Function Call Syntax (UFCS) - allowing `x.foo()` to desugar to `foo(x)` for any function, not just impl methods. This would enable extension-method patterns without requiring impl blocks, but adds complexity to name resolution.

## References

- [Rust Reference: Implementations](https://doc.rust-lang.org/reference/items/implementations.html)
- [ADR-0008: Affine Types and Mutable Value Semantics](0008-affine-types-mvs.md) - future receiver types like `inout self`
