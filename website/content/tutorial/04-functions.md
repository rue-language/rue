+++
title = "Functions"
weight = 4
template = "tutorial/page.html"
+++

# Functions

Functions are declared with `fn`, followed by parameters and a return type.

## Basic Functions

```rue
fn add(a: i32, b: i32) -> i32 {
    a + b
}

fn is_positive(n: i32) -> bool {
    n > 0
}

fn main() -> i32 {
    let sum = add(3, 4);
    @dbg(sum);  // prints: 7

    @dbg(is_positive(sum));   // prints: 1 (true)
    @dbg(is_positive(-5));    // prints: 0 (false)

    sum
}
```

## Implicit Returns

The last expression in a function is its return value—no `return` keyword needed:

```rue
fn double(x: i32) -> i32 {
    x * 2  // this value is returned
}
```

Note the lack of a semicolon. Adding one would make it a statement instead of an expression.

## Explicit Returns

You can use `return` for early exits:

```rue
fn absolute(n: i32) -> i32 {
    if n < 0 {
        return -n;
    }
    n
}

fn main() -> i32 {
    @dbg(absolute(-42));  // prints: 42
    @dbg(absolute(17));   // prints: 17
    0
}
```

## Functions Without Return Values

Functions that don't return a meaningful value return the unit type `()`:

```rue
fn greet() {
    @dbg(42);  // side effect only
}

fn main() -> i32 {
    greet();
    0
}
```

When there's no `-> Type`, the return type is implicitly `()`.
