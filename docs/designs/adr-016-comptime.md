# ADR-016: Comptime and Type-Level Computation

## Status

Proposed

## Context

Rue needs abstraction mechanisms (generics) for practical programming. The traditional approach (parametric polymorphism with trait bounds) leads to complexity:

- Rust is still extending its type system (GATs, HRTBs, const generics, async traits)
- Trait resolution is essentially running an interpreter (Prolog-like logic)
- Separate "generics" and "const generics" mechanisms

An alternative, pioneered by Zig, is **comptime**: the ability to run ordinary code at compile time, with types as first-class values.

Key insight: if trait resolution is an interpreter anyway, we might as well use a straightforward value-level interpreter and get more power with less conceptual overhead.

## Decision

Implement comptime evaluation with types as values.

### Core Concept

`comptime` marks values that must be known at compile time:

```rue
fn max(comptime T: type, a: T, b: T) -> T {
    if a > b { a } else { b }
}

fn main() {
    let x = max(i32, 10, 20);  // T resolved at compile time
}
```

At compile time:
1. `T` is bound to `i32`
2. Function is instantiated with concrete type
3. Type checking proceeds with known types

### Types as Values

Types are first-class values at comptime:

```rue
comptime fn array_of(T: type, n: usize) -> type {
    [T; n]
}

fn example() {
    let arr: array_of(i32, 10) = ...;  // Type computed at comptime
}
```

The `type` type is special: it can only exist at comptime.

### Comptime Blocks

Execute code at compile time:

```rue
const TABLE: [u8; 256] = comptime {
    let mut table: [u8; 256] = [0; 256];
    let mut i: usize = 0;
    while i < 256 {
        table[i] = compute_lookup(i as u8);
        i = i + 1;
    }
    table
};
```

### Surface Syntax for Generics

While the underlying mechanism is comptime, provide familiar generic syntax as sugar:

```rue
// What programmers write
fn max<T>(a: T, b: T) -> T {
    if a > b { a } else { b }
}

// Desugars to
fn max(comptime T: type, a: T, b: T) -> T {
    if a > b { a } else { b }
}
```

```rue
// Generic struct
struct Vec<T> {
    data: *mut T,
    len: usize,
    cap: usize,
}

// Desugars to (conceptually)
fn Vec(comptime T: type) -> type {
    struct {
        data: *mut T,
        len: usize,
        cap: usize,
    }
}
```

### Comptime Constraints (Duck Typing)

Instead of trait bounds, use comptime assertions:

```rue
fn sort<T>(items: inout [T]) {
    comptime {
        // Check that T supports comparison
        assert(has_method(T, "cmp") or has_operator(T, "<"),
               "sort requires comparable type");
    }
    // ... implementation
}
```

Or more implicitly - if you try to use `<` on a type that doesn't support it, you get a compile error at instantiation:

```rue
fn max<T>(a: T, b: T) -> T {
    if a > b { a } else { b }  // Error if T doesn't support >
}

max(Point{x:1,y:2}, Point{x:3,y:4});  // Error: Point doesn't support >
```

This is duck typing at compile time - simpler than traits, though with worse error messages (errors at instantiation, not declaration).

### Comptime Functions and Intrinsics

Built-in comptime intrinsics for reflection:

```rue
comptime fn type_name(T: type) -> str { ... }
comptime fn size_of(T: type) -> usize { ... }
comptime fn align_of(T: type) -> usize { ... }
comptime fn has_field(T: type, name: str) -> bool { ... }
comptime fn field_type(T: type, name: str) -> type { ... }
comptime fn is_integer(T: type) -> bool { ... }
comptime fn ownership_mode(T: type) -> OwnershipMode { ... }
```

Usage:
```rue
fn serialize<T>(value: T) -> [u8] {
    comptime {
        if is_integer(T) {
            // Generate integer serialization
        } else if has_field(T, "serialize") {
            // Call custom serialization
        } else {
            @compileError("Cannot serialize type: " + type_name(T));
        }
    }
}
```

### Comptime vs Runtime

Clear separation:

| Comptime | Runtime |
|----------|---------|
| Types (`type`) | Values of concrete types |
| `comptime` variables | Regular variables |
| `comptime` blocks | Regular blocks |
| Comptime intrinsics | Runtime functions |
| No side effects | Full side effects |

Comptime code cannot:
- Perform IO
- Allocate heap memory (result must be static)
- Access runtime variables
- Have observable side effects

### Specialization

Comptime enables specialization:

```rue
fn copy<T>(dst: inout [T], src: [T]) {
    comptime {
        if is_trivially_copyable(T) {
            // Use memcpy
            @memcpy(dst.ptr, src.ptr, src.len * size_of(T));
        } else {
            // Element-by-element copy
            let mut i: usize = 0;
            while i < src.len {
                dst[i] = src[i];
                i = i + 1;
            }
        }
    }
}
```

### Interaction with Ownership Modes

Comptime can inspect ownership modes:

```rue
fn container_drop<T>(items: move [T]) {
    comptime {
        match ownership_mode(T) {
            OwnershipMode::Value => {
                // Nothing special needed
            },
            OwnershipMode::Move => {
                // Drop each element
                for item in items {
                    drop(item);
                }
            },
            OwnershipMode::Linear => {
                @compileError("Cannot drop container of linear types - must consume each element");
            },
            OwnershipMode::Rc => {
                // Decrement each refcount
                for item in items {
                    rc_dec(item);
                }
            },
        }
    }
}
```

## Implementation

### Phase 1: Comptime Interpreter

Build an interpreter that can evaluate a subset of Rue at compile time:

```rust
struct ComptimeInterpreter {
    // Comptime values
    values: HashMap<Symbol, ComptimeValue>,
    // Type definitions discovered during evaluation
    types: Vec<TypeDef>,
}

enum ComptimeValue {
    Int(i64),
    Bool(bool),
    Type(TypeId),
    Array(Vec<ComptimeValue>),
    Struct(HashMap<String, ComptimeValue>),
    // No pointers, no heap allocation
}
```

Supported operations:
- Arithmetic, logic, comparison
- Control flow (if, while, for)
- Function calls (comptime functions only)
- Type construction
- Struct/array literals

Not supported:
- Heap allocation
- IO
- Pointer operations
- Calling non-comptime functions

### Phase 2: Type as Value

Represent types as comptime values:

```rust
// Types are represented as indices into the type table
enum ComptimeValue {
    // ...
    Type(TypeId),
}

// Type operations return comptime values
fn eval_type_operation(&mut self, op: TypeOp) -> ComptimeValue {
    match op {
        TypeOp::SizeOf(ty) => ComptimeValue::Int(self.size_of(ty) as i64),
        TypeOp::ArrayType(elem, len) => {
            let type_id = self.create_array_type(elem, len);
            ComptimeValue::Type(type_id)
        },
        // ...
    }
}
```

### Phase 3: Generic Instantiation

When a generic function is called:

1. Evaluate comptime arguments
2. Check if this instantiation already exists
3. If not, create a new instantiation:
   - Substitute comptime values
   - Run comptime blocks
   - Type check the result
   - Lower to AIR

```rust
fn instantiate_generic(
    &mut self,
    func: &GenericFn,
    comptime_args: &[ComptimeValue],
) -> FunctionId {
    // Check cache
    let key = (func.id, comptime_args.to_vec());
    if let Some(id) = self.instantiation_cache.get(&key) {
        return *id;
    }

    // Create new instantiation
    let specialized = self.specialize(func, comptime_args);
    let id = self.functions.push(specialized);
    self.instantiation_cache.insert(key, id);
    id
}
```

### Phase 4: Comptime Blocks in Regular Code

Handle `comptime { }` blocks during compilation:

```rust
fn lower_comptime_block(&mut self, block: &Block) -> LoweredCode {
    // Evaluate the block at compile time
    let result = self.comptime_interpreter.eval_block(block)?;

    // The result must be a valid compile-time value
    // Inline it into the generated code
    self.lower_comptime_value(result)
}
```

## Consequences

### Positive

- **One mechanism**: No separate generics, const generics, associated types
- **Full language power**: Arbitrary computation at compile time
- **Simple mental model**: "It's just Rue, running at compile time"
- **Specialization built-in**: Natural to specialize based on type properties
- **Reflection**: Can inspect types, generate code based on structure

### Negative

- **Error messages**: Duck typing means errors at instantiation, not declaration
- **Implementation complexity**: Need an interpreter (though simpler than trait solver)
- **Compile time**: More computation at compile time
- **No separate compilation**: Generic code must be available at instantiation

### Comparison to Alternatives

| Feature | Rust Generics | Zig Comptime | Rue Comptime |
|---------|---------------|--------------|--------------|
| Type bounds | Traits | Duck typing | Duck typing |
| Error location | Declaration | Instantiation | Instantiation |
| Specialization | Limited | Natural | Natural |
| Type reflection | Limited | Full | Full |
| Complexity | High | Medium | Medium |
| Separate compilation | Monomorphization | No | No |

### Future: Improving Error Messages

Could add optional "interface" declarations for documentation and better errors:

```rue
interface Comparable {
    fn cmp(self, other: Self) -> Ordering;
}

// Optional bound for better errors
fn sort<T: Comparable>(items: inout [T]) { ... }
```

This would check the bound at the call site, giving better error locations, while still using comptime underneath.

## Open Questions

1. **Comptime allocations**: Can comptime code build complex structures? Zig allows this with result stored in static memory.

2. **Comptime from runtime**: What if comptime value depends on runtime branching?
   ```rue
   if runtime_condition {
       let x: some_comptime_fn() = ...;  // Which branch to evaluate?
   }
   ```
   Answer: Both branches must be valid at comptime; dead code elimination happens after.

3. **Incremental compilation**: How to cache across compilations?

4. **Standard library**: How much of stdlib needs comptime-aware versions?

## Related ADRs

- ADR-013: Type System Evolution Overview
- ADR-014: Ownership Modes (comptime can inspect modes)
- ADR-015: Mutable Value Semantics (comptime functions need MVS semantics)
- ADR-017: Generators (generic iterators need comptime)

## References

- [Zig Language Reference - Comptime](https://ziglang.org/documentation/master/#Compile-Time)
- [D Language - CTFE](https://dlang.org/spec/function.html#ctfe)
- [Reflection in Modern C++](https://www.open-std.org/jtc1/sc22/wg21/docs/papers/2022/p2320r0.pdf)
- [Type-Level Computation in Haskell](https://serokell.io/blog/type-level-programming-in-haskell)
