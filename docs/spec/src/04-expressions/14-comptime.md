+++
title = "Compile-Time Expressions"
weight = 14
template = "spec/page.html"
+++

# Compile-Time Expressions

{{ rule(id="4.14:1", cat="normative") }}

A compile-time expression is an expression marked with the `comptime` keyword that **MUST** be fully evaluated at compile time.

{{ rule(id="4.14:2", cat="normative") }}

```ebnf
comptime_expr = "comptime" "{" expression "}" ;
```

The expression inside a comptime block is evaluated during compilation. The following operations are supported within comptime blocks:

- Integer literals
- Boolean literals (`true`, `false`)
- Arithmetic operators (`+`, `-`, `*`, `/`, `%`)
- Comparison operators (`==`, `!=`, `<`, `<=`, `>`, `>=`)
- Logical operators (`&&`, `||`, `!`)
- Bitwise operators (`&`, `|`, `^`, `<<`, `>>`)

{{ rule(id="4.14:3", cat="normative") }}

A comptime expression can be used anywhere an expression is expected. The result of the comptime evaluation replaces the comptime block.

```rue
fn main() -> i32 {
    let x: i32 = comptime { 21 * 2 };
    x
}
```

## Comptime Restrictions

{{ rule(id="4.14:4", cat="legality-rule") }}

It is a compile-time error if an expression inside a comptime block cannot be evaluated at compile time. This includes:

- References to runtime variables
- Function calls (except to comptime-evaluable functions in future versions)
- Operations that would panic at runtime

```rue
fn main() -> i32 {
    let x = 10;
    comptime { x + 1 }  // ERROR: x cannot be known at compile time
}
```

## Comptime Parameters

{{ rule(id="4.14:5", cat="normative") }}

Function parameters can be marked with `comptime`, requiring the caller to provide a compile-time known value. The parameter's value is available as a compile-time constant within the function body.

```ebnf
parameter = [ "comptime" ] IDENT ":" type ;
```

Comptime parameters can have any type, including the special `type` type (see below).

```rue
fn multiply(comptime n: i32, value: i32) -> i32 {
    n * value
}

fn main() -> i32 {
    multiply(6, 7)  // n is known at compile time
}
```

Comptime parameters enable monomorphization: each unique combination of comptime arguments creates a specialized version of the function.

The keyword `type` is a comptime-only type whose values are types themselves. A parameter of type `type` must be marked `comptime`.

```rue
fn identity(comptime T: type, x: T) -> T {
    x
}

fn main() -> i32 {
    identity(i32, 42)
}
```

When a function has a `comptime T: type` parameter, occurrences of `T` in parameter types and return types are substituted with the concrete type at each call site.

{{ rule(id="4.14:6", cat="legality-rule") }}

It is a compile-time error to pass a runtime value to a comptime parameter.

```rue
fn double(comptime n: i32) -> i32 { n * 2 }

fn main() -> i32 {
    let x = 21;
    double(x)  // ERROR: comptime parameter requires a compile-time known value
}
```

Type values cannot exist at runtime. It is a compile-time error to attempt to store a type value in a runtime variable.

```rue
fn main() -> i32 {
    let t = comptime { i32 };  // ERROR: type values cannot exist at runtime
    0
}
```

## Anonymous Struct Types

{{ rule(id="4.14:7", cat="normative") }}

A comptime function that returns `type` can construct an anonymous struct type using the following syntax:

```ebnf
anon_struct_type = "struct" "{" struct_field { "," struct_field } "}" ;
struct_field = IDENT ":" type ;
```

```rue
fn Point() -> type {
    struct { x: i32, y: i32 }
}

fn main() -> i32 {
    let P = Point();
    let p: P = P { x: 10, y: 32 };
    p.x + p.y
}
```

Anonymous structs can be parameterized using comptime type parameters:

```rue
fn Pair(comptime T: type) -> type {
    struct { first: T, second: T }
}

fn main() -> i32 {
    let IntPair = Pair(i32);
    let p: IntPair = IntPair { first: 20, second: 22 };
    p.first + p.second
}
```

{{ rule(id="4.14:8", cat="normative") }}

Two anonymous struct types are structurally equal if and only if they have the same field names in the same order with the same types.

```rue
fn make_point1() -> type { struct { x: i32, y: i32 } }
fn make_point2() -> type { struct { x: i32, y: i32 } }

fn main() -> i32 {
    let P1 = make_point1();
    let P2 = make_point2();
    let p1: P1 = P1 { x: 10, y: 20 };
    let p2: P2 = p1;  // OK: P1 and P2 are structurally equal
    p2.x + p2.y
}
```

Anonymous structs with different field names or different field types are different types and are not assignable to each other.

{{ rule(id="4.14:9", cat="legality-rule") }}

It is a compile-time error to define an anonymous struct type with no fields.

```rue
fn empty() -> type {
    struct { }  // ERROR: empty struct
}
```
