# ADR-014: Ownership Modes

## Status

Proposed

## Context

Rue needs a memory management strategy that:
1. Provides memory safety without garbage collection
2. Is more explicit than Rust's implicit Copy/Move distinction
3. Supports linear types for resource management (must-use semantics)
4. Allows shared ownership when appropriate

Currently, Rue has no ownership tracking - all types behave like Copy types, which is unsound for heap-allocated data.

## Decision

Introduce four **ownership modes** as type-level declarations:

```rue
value struct Point { x: i32, y: i32 }
move struct Buffer { ptr: *mut u8, len: usize }
linear struct FileHandle { fd: i32 }
rc struct SharedConfig { data: ConfigData }
```

### Mode Semantics

#### `value` - Copy Semantics

```rue
value struct Point { x: i32, y: i32 }

fn example() {
    let p1 = Point { x: 1, y: 2 };
    let p2 = p1;      // Deep copy
    use(p1);          // OK - p1 still valid
    use(p2);          // OK
}
```

- Assignment creates an independent copy
- No aliasing possible
- Original remains valid after copy
- Default for small, simple types
- Similar to Rust's `Copy` types

#### `move` - Affine/Move Semantics

```rue
move struct Buffer { ptr: *mut u8, len: usize }

fn example() {
    let b1 = Buffer::new(1024);
    let b2 = b1;      // Ownership transfer
    // use(b1);       // ERROR: use after move
    use(b2);          // OK
}                     // b2 dropped here
```

- Assignment transfers ownership
- Original becomes invalid after move
- Value can be dropped without use (affine, not linear)
- Similar to Rust's default move semantics

#### `linear` - Linear Semantics (Must-Use)

```rue
linear struct FileHandle { fd: i32 }

fn example() {
    let f = FileHandle::open("data.txt");
    // implicit drop would be ERROR: linear value not consumed
    f.close();        // OK - explicitly consumed
}

fn also_ok() {
    let f = FileHandle::open("data.txt");
    take_ownership(f); // OK - consumed by function call
}

fn return_ok() -> FileHandle {
    let f = FileHandle::open("data.txt");
    f                 // OK - consumed by returning
}
```

- Assignment transfers ownership (like `move`)
- **Must be explicitly consumed** - cannot be implicitly dropped
- Consumption methods: explicit destructor call, passing to consuming function, returning
- Compiler error if value goes out of scope unconsumed
- Provides "cannot forget to close" guarantees

#### `rc` - Reference Counted Semantics

```rue
rc struct SharedConfig { data: ConfigData }

fn example() {
    let c1 = SharedConfig::new(load_config());
    let c2 = c1;      // Increment refcount, both valid
    use(c1);          // OK
    use(c2);          // OK
}                     // Both refs dropped, data freed when count hits 0
```

- Assignment increments reference count (shallow copy)
- Multiple aliases to same data
- Data freed when last reference dropped
- Always atomic (thread-safe) - compiler may optimize to non-atomic when provably single-threaded
- Provides shared ownership when needed

### Consuming Linear Values

Linear values can be consumed in these ways:

1. **Explicit destructor call**
   ```rue
   fn example() {
       let f = FileHandle::open("file.txt");
       f.close();  // close() consumes self
   }
   ```

2. **Passing to a consuming function**
   ```rue
   fn take_file(f: FileHandle) { ... }  // Takes ownership

   fn example() {
       let f = FileHandle::open("file.txt");
       take_file(f);  // Consumed by take_file
   }
   ```

3. **Returning from function**
   ```rue
   fn open_temp() -> FileHandle {
       let f = FileHandle::open("/tmp/data");
       f  // Consumed by returning
   }
   ```

4. **Pattern matching with consumption**
   ```rue
   linear enum Result<T, E> {
       Ok(T),
       Err(E),
   }

   fn example() {
       let r = fallible_op();
       match r {            // r consumed by match
           Ok(value) => use(value),
           Err(e) => handle(e),
       }
   }
   ```

### Drop Methods

Each mode has different drop behavior:

| Mode | Implicit Drop | Explicit Drop | Notes |
|------|---------------|---------------|-------|
| `value` | Allowed | Optional | Just deallocates |
| `move` | Allowed | Optional | Runs destructor if defined |
| `linear` | **Error** | Required | Must call consuming method |
| `rc` | Allowed | Optional | Decrements count, may free |

### Interaction with Fields

Struct fields inherit reasonable defaults but can be overridden:

```rue
move struct Container {
    buffer: Buffer,        // move field in move struct - OK
    config: SharedConfig,  // rc field in move struct - OK
    metadata: Point,       // value field in move struct - OK
}
```

Rules:
- `value` structs can only contain `value` and `rc` fields
- `move` structs can contain any mode
- `linear` structs can contain any mode (but linear fields must be consumed)
- `rc` structs can only contain `value` and `rc` fields (must be safely shareable)

### Syntax Alternatives Considered

**Alternative A: Attributes**
```rue
#[linear]
struct FileHandle { fd: i32 }
```
Rejected: Less visible, feels like metadata rather than core semantics.

**Alternative B: Trait/interface based (Rust-style)**
```rue
struct FileHandle: Linear { fd: i32 }
```
Rejected: Conflates ownership with interface inheritance.

**Alternative C: Keyword modifiers (chosen)**
```rue
linear struct FileHandle { fd: i32 }
```
Preferred: Clear, prominent, part of the type declaration.

## Implementation

### Phase 1: Parser and AST

Add ownership mode to struct declarations:

```rust
// In rue-parser
pub enum OwnershipMode {
    Value,
    Move,
    Linear,
    Rc,
}

pub struct StructDecl {
    pub mode: OwnershipMode,  // New field
    pub name: Ident,
    pub fields: Vec<StructField>,
}
```

### Phase 2: Type System

Extend the `Type` enum or add mode tracking:

```rust
// In rue-air
pub struct StructDef {
    pub name: String,
    pub mode: OwnershipMode,  // New field
    pub fields: Vec<StructField>,
}
```

### Phase 3: Semantic Analysis - Move Checking

Track variable state:

```rust
enum VarState {
    Valid,
    Moved,
    PartiallyMoved(HashSet<FieldIndex>),
}

// In AnalysisContext
struct LocalVar {
    slot: u32,
    ty: Type,
    is_mut: bool,
    state: VarState,  // New field
}
```

On assignment/passing of move/linear types:
1. Check source is `Valid`
2. Mark source as `Moved`
3. Report error on subsequent use

### Phase 4: Linear Checking

At scope exit, verify all linear values consumed:

```rust
fn check_scope_exit(&self, ctx: &AnalysisContext) -> CompileResult<()> {
    for (name, local) in &ctx.locals {
        if local.ty.is_linear() && local.state == VarState::Valid {
            return Err(CompileError::new(
                ErrorKind::LinearValueNotConsumed(name.to_string()),
                local.span,
            ));
        }
    }
    Ok(())
}
```

### Phase 5: Reference Counting Runtime

Add runtime support for rc types:

```rust
// In rue-runtime
#[repr(C)]
struct RcBox<T> {
    count: AtomicUsize,
    value: T,
}

fn rc_clone<T>(ptr: *mut RcBox<T>) -> *mut RcBox<T> {
    (*ptr).count.fetch_add(1, Ordering::Relaxed);
    ptr
}

fn rc_drop<T>(ptr: *mut RcBox<T>) {
    if (*ptr).count.fetch_sub(1, Ordering::Release) == 1 {
        fence(Ordering::Acquire);
        drop_in_place(&mut (*ptr).value);
        dealloc(ptr);
    }
}
```

## Consequences

### Positive

- **Explicit control**: Programmers see ownership mode in type declaration
- **Linear safety**: Cannot forget to close files, release locks
- **Flexible**: Right tool for each job (copy for small data, rc for sharing)
- **Teachable**: Each mode is simple; complexity is in choosing the right one

### Negative

- **Decision burden**: Programmer must choose mode for each type
- **More keywords**: Four ownership keywords to learn
- **Runtime cost for rc**: Reference counting has overhead (though often optimized away)

### Comparison to Rust

| Aspect | Rust | Rue |
|--------|------|-----|
| Copy types | Opt-in via `Copy` trait | `value` keyword |
| Move types | Default | `move` keyword |
| Linear types | Not supported (`#[must_use]` is advisory) | `linear` keyword |
| Shared ownership | Library types (`Rc`, `Arc`) | `rc` keyword |
| Visibility | Implicit (check trait impls) | Explicit in declaration |

## Open Questions

1. **Default mode**: Should there be a default if no mode specified? Options:
   - Error (require explicit mode)
   - Default to `value` for small types, `move` for others
   - Default to `move` (Rust-like)

2. **Primitive types**: Are `i32`, `bool`, etc. implicitly `value`?

3. **Generic containers**: How does `Vec<T>` behave when T is linear?

4. **Partial moves**: Allow moving individual fields out of a struct?

## Related ADRs

- ADR-013: Type System Evolution Overview
- ADR-015: Mutable Value Semantics (uses ownership modes)
- ADR-016: Comptime (needed for generic containers with ownership)

## References

- [Rust Ownership](https://doc.rust-lang.org/book/ch04-00-understanding-ownership.html)
- [Linear Types in Haskell](https://ghc.gitlab.haskell.org/ghc/doc/users_guide/exts/linear_types.html)
- [Austral Language](https://austral-lang.org/) - Linear types focus
- [Swift ARC](https://docs.swift.org/swift-book/documentation/the-swift-programming-language/automaticreferencecounting/)
