+++
title = "Index Expressions"
weight = 11
template = "spec/page.html"
+++

# Index Expressions

{{ rule(id="4.11:1", cat="normative") }}

An index expression accesses an element of an array.

{{ rule(id="4.11:2", cat="normative") }}

```ebnf
index_expr = expression "[" expression "]" ;
```

{{ rule(id="4.11:3", cat="normative") }}

The expression before the brackets must have an array type `[T; N]`.

{{ rule(id="4.11:4", cat="normative") }}

The index expression must have an integer type.

{{ rule(id="4.11:5", cat="normative") }}

The type of an index expression is the element type `T`.

{{ rule(id="4.11:6") }}

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [10, 42, 100];
    arr[1]  // 42
}
```

## Bounds Checking

{{ rule(id="4.11:7", cat="normative") }}

For constant indices, bounds checking is performed at compile time.

{{ rule(id="4.11:8", cat="normative") }}

For variable indices, bounds checking is performed at runtime.

{{ rule(id="4.11:9", cat="normative") }}

An out-of-bounds access causes a runtime panic.

{{ rule(id="4.11:10") }}

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    arr[5]  // Compile-time error: index out of bounds
}
```

{{ rule(id="4.11:11") }}

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    let idx = 5;
    arr[idx]  // Runtime error: index out of bounds
}
```

## Index Assignment

{{ rule(id="4.11:12", cat="normative") }}

For mutable arrays, elements can be assigned using index expressions.

{{ rule(id="4.11:13") }}

```rue
fn main() -> i32 {
    let mut arr: [i32; 2] = [0, 0];
    arr[0] = 20;
    arr[1] = 22;
    arr[0] + arr[1]  // 42
}
```
