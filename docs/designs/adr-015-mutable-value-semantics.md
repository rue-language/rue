# ADR-015: Mutable Value Semantics

## Status

Proposed

## Context

Rue aims to provide memory safety without Rust's borrow checker complexity. The key insight from Val/Hylo is that **mutable value semantics** can provide similar safety guarantees with a simpler mental model.

The core problem:
- We want to mutate data efficiently (no copies)
- We want to prevent data races and dangling references
- We don't want lifetime annotations

Rust's solution (borrow checker with lifetimes) is powerful but has a steep learning curve. MVS offers an alternative.

## Decision

Adopt mutable value semantics with `inout` parameters:

### Core Concepts

#### 1. Values Are Independent

By default, values don't alias. Assignment creates independent copies (for `value` types) or transfers ownership (for `move`/`linear` types).

```rue
value struct Point { x: i32, y: i32 }

let a = Point { x: 1, y: 2 };
let b = a;           // Copy for value types
a.x = 10;            // Only affects a
assert(b.x == 1);    // b is independent
```

#### 2. `inout` Parameters for Mutation

When you need to mutate a value owned by the caller, use `inout`:

```rue
fn scale(inout p: Point, factor: i32) {
    p.x = p.x * factor;
    p.y = p.y * factor;
}

fn main() {
    let mut p = Point { x: 1, y: 2 };
    scale(inout p, 3);
    assert(p.x == 3);
}
```

Key properties:
- Caller retains ownership after the call
- Callee has exclusive mutable access during the call
- No copy occurs (efficient)
- Original value reflects mutations after call returns

#### 3. Law of Exclusivity

While a value is passed `inout`, no other access is permitted:

```rue
fn bad_example() {
    let mut p = Point { x: 1, y: 2 };

    // ERROR: cannot access p while passed inout
    do_something(inout p, p.x);

    // ERROR: cannot pass same value inout twice
    swap(inout p, inout p);
}
```

This is checked at compile time through simple rules (no lifetime annotations needed).

### Syntax

#### Function Parameters

```rue
fn read_only(p: Point) { ... }           // Immutable borrow or copy
fn mutating(inout p: Point) { ... }      // Exclusive mutable access
fn consuming(p: move Point) { ... }      // Takes ownership
```

#### Call Sites

```rue
read_only(p);        // No annotation needed
mutating(inout p);   // Must mark with 'inout'
consuming(move p);   // Must mark with 'move' (for move/linear types)
```

The explicit `inout` at call sites makes mutation visible:

```rue
// You can see at a glance which calls might modify p
process(p);
transform(inout p);   // This one mutates p
validate(p);
```

### Comparison to Rust References

| Rust | Rue | Semantics |
|------|-----|-----------|
| `&T` | `p: T` | Immutable access |
| `&mut T` | `inout p: T` | Exclusive mutable access |
| `T` (move) | `p: move T` | Ownership transfer |

The key difference: Rue's `inout` doesn't create a reference value that can be stored or returned. It's purely a calling convention.

### Projections (Field Access)

You can pass a field `inout`:

```rue
value struct Rectangle {
    origin: Point,
    size: Size,
}

fn move_origin(inout origin: Point, dx: i32, dy: i32) {
    origin.x = origin.x + dx;
    origin.y = origin.y + dy;
}

fn main() {
    let mut rect = Rectangle { ... };
    move_origin(inout rect.origin, 10, 20);  // Only origin is borrowed
    use(rect.size);                           // size is still accessible
}
```

The exclusivity rule applies to the specific projection:
- `rect.origin` is exclusively borrowed
- `rect.size` is still accessible (disjoint fields)

### No Stored References

Unlike Rust, `inout` bindings cannot be stored:

```rue
// Rust allows this
struct Holder<'a> {
    data: &'a mut Point
}

// Rue does NOT allow this - no reference types
// inout is only a parameter passing mode
```

This simplification eliminates the need for lifetime annotations - `inout` is purely a temporary, scoped access.

### Interaction with Ownership Modes

| Mode | Pass by value | Pass `inout` | Pass `move` |
|------|---------------|--------------|-------------|
| `value` | Copy | Exclusive access | N/A |
| `move` | Error (use `inout` or `move`) | Exclusive access | Transfer |
| `linear` | Error | Exclusive access | Transfer (consumed) |
| `rc` | Increment refcount | Error (use shared access) | Decrement + transfer |

Notes:
- `rc` types cannot be passed `inout` because they represent shared ownership
- For shared mutation of `rc` types, use interior mutability patterns (future ADR)

### Method Syntax

Methods use `self` with optional `inout`:

```rue
value struct Counter {
    count: i32,
}

impl Counter {
    fn get(self) -> i32 {           // Immutable access
        self.count
    }

    fn increment(inout self) {       // Mutating method
        self.count = self.count + 1;
    }

    fn into_count(self: move) -> i32 {  // Consuming method
        self.count
    }
}

fn main() {
    let mut c = Counter { count: 0 };
    c.increment();     // Implicitly passes inout self
    let n = c.get();
}
```

### Exclusivity Checking Algorithm

At compile time, track "active borrows" using a simple model:

```
ActiveBorrow = Path × AccessMode

Path = Variable
     | Path.field
     | Path[index]

AccessMode = Read | Write
```

Rules:
1. Creating `inout` binding: add `(path, Write)` to active borrows
2. Reading a value: check no `(prefix(path), Write)` is active
3. When `inout` scope ends: remove from active borrows
4. Conflict = overlapping paths with at least one Write

Example:
```rue
fn example() {
    let mut r = Rectangle { origin: Point{x:0,y:0}, size: Size{w:10,h:10} };

    scale_point(inout r.origin, 2);
    // Active: { (r.origin, Write) }

    let w = r.size.w;  // OK: r.size doesn't overlap r.origin
    // let x = r.origin.x;  // ERROR: r.origin is borrowed
}
```

This is simpler than Rust's borrow checker because:
- No lifetimes to track (borrows are scoped to function calls)
- No reference values that can be stored/returned
- Just path overlap checking within a function

## Implementation

### Phase 1: Parser Changes

Add `inout` keyword:

```rust
pub enum ParamMode {
    Default,    // Copy or immutable borrow
    Inout,      // Exclusive mutable access
    Move,       // Ownership transfer
}

pub struct Param {
    pub mode: ParamMode,
    pub name: Ident,
    pub ty: TypeExpr,
}
```

Update call expressions:

```rust
pub enum ArgMode {
    Default,
    Inout,
    Move,
}

pub struct Arg {
    pub mode: ArgMode,
    pub expr: Expr,
}
```

### Phase 2: RIR Changes

Represent parameter modes in the IR:

```rust
pub struct FnDecl {
    pub name: Symbol,
    pub params: Vec<(Symbol, Symbol, ParamMode)>,  // (name, type, mode)
    pub return_type: Symbol,
    pub body: InstRef,
}
```

### Phase 3: Exclusivity Analysis

Add a new analysis pass before/during sema:

```rust
struct ExclusivityChecker {
    active_borrows: Vec<(Path, AccessMode, Span)>,
}

impl ExclusivityChecker {
    fn enter_inout(&mut self, path: Path, span: Span) -> Result<(), ConflictError> {
        // Check for conflicts
        for (existing, mode, existing_span) in &self.active_borrows {
            if path.overlaps(existing) {
                return Err(ConflictError { ... });
            }
        }
        self.active_borrows.push((path, AccessMode::Write, span));
        Ok(())
    }

    fn check_read(&self, path: Path, span: Span) -> Result<(), ConflictError> {
        for (existing, mode, _) in &self.active_borrows {
            if *mode == AccessMode::Write && path.overlaps(existing) {
                return Err(ConflictError { ... });
            }
        }
        Ok(())
    }

    fn exit_inout(&mut self, path: &Path) {
        self.active_borrows.retain(|(p, _, _)| p != path);
    }
}
```

### Phase 4: Codegen

For `inout` parameters, pass by pointer:

```rue
fn scale(inout p: Point, factor: i32)
```

Becomes (conceptually):

```c
void scale(Point* p, int32_t factor) {
    p->x = p->x * factor;
    p->y = p->y * factor;
}
```

At call sites:
```rue
scale(inout p, 3);
```

Becomes:
```c
scale(&p, 3);
```

## Consequences

### Positive

- **No lifetimes**: Simpler than Rust's borrow checker
- **Safe mutation**: Exclusivity prevents data races
- **Efficient**: No copies for large values
- **Visible mutation**: `inout` at call site shows what might change
- **Composable**: Works well with ownership modes

### Negative

- **No stored references**: Can't have `&mut` fields or return `&mut`
- **Function-scoped only**: Can't express "borrow for longer than this call"
- **More restricted than Rust**: Some patterns require restructuring

### Patterns That Change

**Rust pattern: Iterator returning references**
```rust
fn iter(&self) -> impl Iterator<Item = &T>
```

**Rue pattern: Index-based or callback-based**
```rue
fn for_each(self, f: fn(inout T)) { ... }
fn get(self, index: usize) -> T { ... }
```

**Rust pattern: Builder with &mut self**
```rust
fn set_name(&mut self, name: String) -> &mut Self
```

**Rue pattern: Chained inout or value return**
```rue
fn set_name(inout self, name: String) { ... }
// or
fn with_name(self: move, name: String) -> Self { ... }
```

## Open Questions

1. **Implicit inout for methods?** Should `c.increment()` implicitly pass `inout self`?
   - Pro: Less syntax
   - Con: Mutation less visible

2. **Nested inout calls?** Can you pass something `inout` that's already borrowed?
   ```rue
   fn outer(inout p: Point) {
       inner(inout p);  // Re-borrow?
   }
   ```
   Probably yes, since it's equivalent to just more code in `outer`.

3. **Array element access?** How does `inout arr[i]` work with dynamic indices?
   - Conservative: Borrow entire array
   - Aggressive: Track index ranges (complex)

## Related ADRs

- ADR-013: Type System Evolution Overview
- ADR-014: Ownership Modes (interacts with parameter passing)
- ADR-016: Comptime (for generic functions with inout)

## References

- [Val Language Design](https://www.val-lang.dev/pages/design)
- [Hylo - Mutable Value Semantics](https://github.com/hylo-lang/hylo/blob/main/Docs/Specification/Language-reference.md)
- [Swift Exclusivity Enforcement](https://www.swift.org/blog/swift-5-exclusivity/)
- [The Problem with Borrowing](https://www.jot.fm/issues/issue_2022_02/article2.pdf)
