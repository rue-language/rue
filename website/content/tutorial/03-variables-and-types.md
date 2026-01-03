+++
title = "Variables and Types"
weight = 3
template = "tutorial/page.html"
+++

# Variables and Types

Rue is statically typed—every variable has a type known at compile time.

## Integer Types

Rue has the integer types you'd expect:

```rue
fn main() -> i32 {
    // Signed integers: i8, i16, i32, i64
    let x: i32 = 42;
    let big: i64 = 1000000000000;

    // Unsigned integers: u8, u16, u32, u64
    let index: u64 = 0;
    let byte: u8 = 255;

    @dbg(x);
    x
}
```

The number after `i` or `u` is the bit width. Signed integers (`i`) can be negative; unsigned integers (`u`) cannot.

## Type Inference

You don't always need to write types explicitly. The compiler can often infer them:

```rue
fn main() -> i32 {
    let x = 42;        // inferred as i32 (the default)
    let y = true;      // inferred as bool

    @dbg(x);
    x
}
```

When there's no context, integer literals default to `i32`.

## Booleans

Boolean values are either `true` or `false`:

```rue
fn main() -> i32 {
    let flag: bool = true;
    let done = false;

    @dbg(flag);   // prints: 1 (true)
    @dbg(done);   // prints: 0 (false)

    0
}
```

## Mutability

Variables are immutable by default. Use `let mut` to make them mutable:

```rue
fn main() -> i32 {
    let mut count = 0;
    count = count + 1;
    count = count + 1;
    @dbg(count);  // prints: 2
    count
}
```

Trying to assign to an immutable variable is a compile error:

```rue
fn main() -> i32 {
    let x = 42;
    x = 43;  // Error: cannot assign to immutable variable
    x
}
```

This helps catch bugs—if a value shouldn't change, the compiler enforces it.
