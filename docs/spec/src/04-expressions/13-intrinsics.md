+++
title = "Intrinsic Expressions"
weight = 13
template = "spec/page.html"
+++

# Intrinsic Expressions

{{ rule(id="4.13:1", cat="normative") }}

An intrinsic expression invokes a compiler-provided primitive operation.

{{ rule(id="4.13:2", cat="normative") }}

```ebnf
intrinsic = "@" IDENT "(" [ expression { "," expression } ] ")" ;
```

{{ rule(id="4.13:3", cat="normative") }}

Intrinsics are prefixed with `@` to distinguish them from user-defined functions.

{{ rule(id="4.13:4", cat="normative") }}

Each intrinsic has a fixed signature specifying the number and types of arguments it accepts.

{{ rule(id="4.13:5", cat="normative") }}

Using an unknown intrinsic name is a compile-time error.

## `@dbg`

{{ rule(id="4.13:6", cat="normative") }}

The `@dbg` intrinsic prints a value to standard output for debugging purposes.

{{ rule(id="4.13:7", cat="normative") }}

`@dbg` accepts exactly one argument of integer, boolean, or string type.

{{ rule(id="4.13:8", cat="normative") }}

`@dbg` prints the value followed by a newline character.

{{ rule(id="4.13:9", cat="normative") }}

The return type of `@dbg` is `()`.

{{ rule(id="4.13:10") }}

```rue
fn main() -> i32 {
    @dbg(42);           // prints: 42
    @dbg(-17);          // prints: -17
    @dbg(true);         // prints: true
    @dbg(false);        // prints: false
    @dbg(10 + 5);       // prints: 15
    @dbg("hello");      // prints: hello
    0
}
```

{{ rule(id="4.13:11") }}

`@dbg` is useful for inspecting values during development:

```rue
fn factorial(n: i32) -> i32 {
    @dbg(n);  // trace each call
    if n <= 1 {
        1
    } else {
        n * factorial(n - 1)
    }
}

fn main() -> i32 {
    factorial(5)
}
```
