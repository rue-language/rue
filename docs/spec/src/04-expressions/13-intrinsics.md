# Intrinsic Expressions

r[4.13:1#normative]
An intrinsic expression invokes a compiler-provided primitive operation.

r[4.13:2#normative]
```ebnf
intrinsic = "@" IDENT "(" [ expression { "," expression } ] ")" ;
```

r[4.13:3#normative]
Intrinsics are prefixed with `@` to distinguish them from user-defined functions.

r[4.13:4#normative]
Each intrinsic has a fixed signature specifying the number and types of arguments it accepts.

r[4.13:5#normative]
Using an unknown intrinsic name is a compile-time error.

## `@dbg`

r[4.13:6#normative]
The `@dbg` intrinsic prints a value to standard output for debugging purposes.

r[4.13:7#normative]
`@dbg` accepts exactly one argument of integer, boolean, or string type.

r[4.13:8#normative]
`@dbg` prints the value followed by a newline character.

r[4.13:9#normative]
The return type of `@dbg` is `()`.

r[4.13:10]
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

r[4.13:11]
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
