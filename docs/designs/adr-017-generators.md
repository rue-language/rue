# ADR-017: Generators and Iterators

## Status

Proposed

## Context

Rue needs iteration abstractions. Several approaches exist:

1. **Interface-based iterators** (Rust, Java): Define an `Iterator` interface with `next()` method
2. **Generator functions** (Python, JS, C#): Functions that can `yield` values
3. **Coroutines with effects** (Koka, Eff): General algebraic effects with `yield` as one effect
4. **For-comprehensions** (Scala, Haskell): Syntactic sugar over monadic operations

Given our goals (simplicity, no full effect system yet), generators as syntax sugar offer the best tradeoff: practical iteration without requiring the full effect machinery.

## Decision

Implement generators as syntax sugar that compiles to state machines.

### Basic Syntax

Generator functions are marked with `fn*` (inspired by JS) or `gen fn`:

```rue
fn* range(start: i32, end: i32) -> i32 {
    let mut i = start;
    while i < end {
        yield i;
        i = i + 1;
    }
}
```

The return type is the **yield type**, not the function's actual return type. The function returns a `Generator<i32>`.

### Using Generators

```rue
fn main() {
    // For loop consumes generator
    for x in range(0, 10) {
        print(x);
    }

    // Manual iteration
    let gen = range(0, 5);
    loop {
        match gen.next() {
            Some(x) => print(x),
            None => break,
        }
    }
}
```

### Generator Type

```rue
// Built-in generic type (using comptime)
struct Generator<T> {
    // Opaque - implementation varies per generator
    state: GeneratorState,
}

impl Generator<T> {
    fn next(inout self) -> Option<T>;
}
```

Each generator function creates a unique anonymous state machine type. `Generator<T>` is the interface they all implement.

### Yield Expressions

`yield` is an expression that:
1. Returns a value to the caller
2. Suspends the generator
3. Resumes when `next()` is called

```rue
fn* repeat_with<T>(f: fn() -> T) -> T {
    loop {
        yield f();
    }
}
```

### Yield with Input (Bidirectional)

Generators can receive values when resumed:

```rue
fn* accumulator() -> (i32, i32) {
    let mut sum = 0;
    loop {
        let input = yield sum;  // yield returns the input
        sum = sum + input;
    }
}

fn main() {
    let gen = accumulator();
    let s1 = gen.send(10);  // s1 = 0, sum becomes 10
    let s2 = gen.send(5);   // s2 = 10, sum becomes 15
    let s3 = gen.send(3);   // s3 = 15, sum becomes 18
}
```

The generator type becomes `Generator<YieldType, InputType>`.

### Generator Ownership and Linearity

Generators interact with ownership modes:

```rue
fn* file_lines(f: move FileHandle) -> String {
    // f is owned by the generator
    while let Some(line) = f.read_line() {
        yield line;
    }
    f.close();  // Consumed when generator completes
}
```

For linear types, the generator **must run to completion** to consume them:

```rue
fn main() {
    let f = FileHandle::open("data.txt");
    let gen = file_lines(f);  // f moved into generator

    // If we don't exhaust gen, f is never closed!
    // Solution: gen itself becomes linear if it captures linear values

    for line in gen {  // Exhausts generator, closes file
        process(line);
    }
}
```

### Transformation to State Machine

The compiler transforms generators to state machines:

```rue
// Source
fn* range(start: i32, end: i32) -> i32 {
    let mut i = start;
    while i < end {
        yield i;
        i = i + 1;
    }
}

// Transformed (conceptual)
struct RangeGenerator {
    state: u8,      // Which yield point
    i: i32,         // Local variable
    end: i32,       // Captured parameter
}

impl Generator<i32> for RangeGenerator {
    fn next(inout self) -> Option<i32> {
        match self.state {
            0 => {
                // Initial state
                if self.i < self.end {
                    let result = self.i;
                    self.i = self.i + 1;
                    self.state = 0;  // Stay in loop state
                    Some(result)
                } else {
                    self.state = 1;  // Done
                    None
                }
            },
            1 => None,  // Already finished
        }
    }
}

fn range(start: i32, end: i32) -> RangeGenerator {
    RangeGenerator { state: 0, i: start, end: end }
}
```

### Relationship to Effects

Generators are a special case of algebraic effects:

```
yield : T -> Yield<T> ()
```

The `Yield` effect suspends computation and returns a value. The generator runner handles this effect by storing state and returning to caller.

If we later add a full effect system, generators can be expressed as:

```rue
fn range(start: i32, end: i32) with Yield<i32> {
    let mut i = start;
    while i < end {
        do yield(i);  // Effect operation
        i = i + 1;
    }
}
```

For now, `fn*` is special syntax that desugars to state machines, avoiding the need for general effect handling.

### Iterator Protocol

Define a standard iteration protocol using comptime:

```rue
// Any type with a next() method works
interface Iterable<T> {
    fn next(inout self) -> Option<T>;
}

// For loop desugaring
for x in collection {
    body(x);
}

// Becomes
{
    let mut iter = collection;  // Or collection.iter() if needed
    loop {
        match iter.next() {
            Some(x) => body(x),
            None => break,
        }
    }
}
```

### Lazy Evaluation

Generators are lazy - they only compute values when requested:

```rue
fn* naturals() -> i32 {
    let mut n = 0;
    loop {
        yield n;
        n = n + 1;
    }
}

fn* take<T>(gen: Generator<T>, count: usize) -> T {
    let mut remaining = count;
    for item in gen {
        if remaining == 0 {
            break;
        }
        yield item;
        remaining = remaining - 1;
    }
}

fn main() {
    // Only computes first 5 naturals
    for n in take(naturals(), 5) {
        print(n);
    }
}
```

### Error Handling in Generators

Generators can yield `Result` types:

```rue
fn* parse_lines(input: String) -> Result<Record, ParseError> {
    for line in input.lines() {
        yield parse_record(line);  // Yields Ok or Err
    }
}

fn main() {
    for result in parse_lines(data) {
        match result {
            Ok(record) => process(record),
            Err(e) => log_error(e),
        }
    }
}
```

Or use early return to fail the generator:

```rue
fn* parse_all(input: String) -> Record {
    for line in input.lines() {
        yield parse_record(line)?;  // Propagates error, ends generator
    }
}
```

## Implementation

### Phase 1: Parser Changes

Add generator syntax:

```rust
pub enum FnKind {
    Regular,
    Generator,
}

pub struct FnDecl {
    pub kind: FnKind,
    pub name: Ident,
    pub params: Vec<Param>,
    pub yield_type: Option<TypeExpr>,  // For generators
    pub return_type: Option<TypeExpr>,
    pub body: Block,
}

pub enum Expr {
    // ...
    Yield(Box<Expr>),
}
```

### Phase 2: Generator Lowering Pass

Before sema, transform generators to structs + impl:

```rust
fn lower_generator(gen: &FnDecl) -> (StructDecl, ImplBlock) {
    // Collect all local variables
    let locals = collect_locals(&gen.body);

    // Identify yield points
    let yield_points = find_yields(&gen.body);

    // Generate state enum
    let state_variants = yield_points.len() + 1;  // +1 for Done

    // Generate struct
    let struct_decl = StructDecl {
        name: gen.name.with_suffix("Generator"),
        fields: [
            Field { name: "state", ty: "u8" },
        ].chain(locals.iter().map(|l| Field { name: l.name, ty: l.ty })),
    };

    // Generate next() method
    let next_impl = generate_next_method(gen, &yield_points);

    (struct_decl, next_impl)
}
```

### Phase 3: State Machine Generation

Transform control flow to state-based:

```rust
fn generate_next_method(gen: &FnDecl, yields: &[YieldPoint]) -> ImplMethod {
    // Each yield point becomes a state
    // Control flow becomes transitions between states

    // Handle:
    // - Loops containing yields
    // - Conditionals containing yields
    // - Nested generators (yield*)
}
```

### Phase 4: Integration with For Loops

Desugar for loops to use iterator protocol:

```rust
fn desugar_for_loop(for_loop: &ForLoop) -> Block {
    // for x in expr { body }
    // =>
    // { let mut iter = expr; loop { match iter.next() { ... } } }
}
```

## Consequences

### Positive

- **Familiar syntax**: Similar to Python/JS generators
- **Zero-cost abstraction**: Compiles to state machines, no heap allocation
- **Lazy evaluation**: Natural for large/infinite sequences
- **Foundation for async**: Similar transformation works for async/await

### Negative

- **Compilation complexity**: State machine transformation is non-trivial
- **Debug experience**: Generated code harder to debug
- **Not full effects**: Can't express all effect patterns

### Comparison to Alternatives

| Approach | Pros | Cons |
|----------|------|------|
| Interface-based (Rust) | Simple model | Boilerplate, manual state |
| Generators (chosen) | Ergonomic | Transformation complexity |
| Full effects (Koka) | Most general | Implementation complexity |
| Async/await only | Simpler | Less general than generators |

## Future: Async Integration

Generators and async share the same state machine foundation:

```rue
// Generator
fn* numbers() -> i32 {
    yield 1;
    yield 2;
}

// Async (potential future syntax)
async fn fetch() -> Response {
    let data = await http_get(url);
    parse(data)
}
```

Both transform to state machines; the difference is:
- Generators yield values to immediate caller
- Async yields control to runtime, resumes when IO completes

Could unify as effects:

```rue
fn numbers() with Yield<i32> { ... }
fn fetch() with Async { ... }
```

## Open Questions

1. **Recursive generators**: Can a generator call itself?
   ```rue
   fn* tree_values<T>(node: Tree<T>) -> T {
       yield node.value;
       for child in node.children {
           yield* tree_values(child);  // yield* ?
       }
   }
   ```

2. **Generator cleanup**: What if generator is dropped before completion?
   - For linear captures: Compile error
   - For move captures: Drop them
   - For value captures: Nothing needed

3. **Pinning**: Do generators need pinning like Rust futures?
   - Probably not if we ban self-references in generator state

4. **Syntax bikeshed**: `fn*`, `gen fn`, `generator fn`, `iter fn`?

## Related ADRs

- ADR-013: Type System Evolution Overview
- ADR-016: Comptime (for generic generators)
- ADR-014: Ownership Modes (generators capturing linear values)

## References

- [Python Generators](https://peps.python.org/pep-0255/)
- [JavaScript Generators](https://developer.mozilla.org/en-US/docs/Web/JavaScript/Reference/Global_Objects/Generator)
- [Rust Generators (unstable)](https://doc.rust-lang.org/unstable-book/language-features/generators.html)
- [Algebraic Effects for the Rest of Us](https://overreacted.io/algebraic-effects-for-the-rest-of-us/)
- [Koka Effect Handlers](https://koka-lang.github.io/koka/doc/book.html#sec-handlers)
