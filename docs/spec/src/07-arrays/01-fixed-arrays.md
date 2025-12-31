+++
title = "Fixed-Size Arrays"
weight = 1
template = "spec/page.html"
+++

# Fixed-Size Arrays

## Array Literals

{{ rule(id="7.1:1", cat="normative") }}

```ebnf
array_literal = "[" [ expression { "," expression } ] "]" ;
```

{{ rule(id="7.1:2", cat="normative") }}

An array literal creates an array with the given elements.

{{ rule(id="7.1:3", cat="legality-rule") }}

All elements **MUST** have the same type.

{{ rule(id="7.1:4", cat="legality-rule") }}

The number of elements **MUST** match the declared array size.

{{ rule(id="7.1:5") }}

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [10, 20, 12];
    arr[0] + arr[1] + arr[2]  // 42
}
```

## Array Indexing

{{ rule(id="7.1:6", cat="normative") }}

Array elements are accessed using bracket notation `arr[index]`.

{{ rule(id="7.1:7", cat="legality-rule") }}

The index **MUST** be an integer type.

{{ rule(id="7.1:8") }}

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [100, 42, 200];
    arr[1]  // 42
}
```

## Bounds Checking

{{ rule(id="7.1:9", cat="legality-rule") }}

For constant indices, bounds **MUST** be checked at compile time.

{{ rule(id="7.1:10", cat="dynamic-semantics") }}

For variable indices, bounds **MUST** be checked at runtime.

{{ rule(id="7.1:11", cat="dynamic-semantics") }}

Out-of-bounds access **MUST** cause a runtime panic.

## Mutable Arrays

{{ rule(id="7.1:12", cat="normative") }}

Mutable arrays allow element assignment.

{{ rule(id="7.1:13") }}

```rue
fn main() -> i32 {
    let mut arr: [i32; 2] = [0, 0];
    arr[0] = 20;
    arr[1] = 22;
    arr[0] + arr[1]  // 42
}
```

## Array Type Syntax

{{ rule(id="7.1:14", cat="normative") }}

```ebnf
array_type = "[" type ";" INTEGER "]" ;
```

{{ rule(id="7.1:15", cat="legality-rule") }}

The size **MUST** be a non-negative integer literal.

## Nested Arrays

{{ rule(id="7.1:16", cat="normative") }}

Arrays **MAY** contain other arrays as elements, forming multi-dimensional arrays.

{{ rule(id="7.1:17", cat="normative") }}

Nested arrays are indexed using chained bracket notation, evaluated left to right.

{{ rule(id="7.1:18") }}

```rue
fn main() -> i32 {
    let matrix: [[i32; 2]; 2] = [[1, 2], [3, 4]];
    matrix[1][1]  // 4
}
```

## Arrays in Structs

{{ rule(id="7.1:19", cat="normative") }}

Struct fields **MAY** have array types.

{{ rule(id="7.1:20", cat="normative") }}

Array fields are accessed by combining field access with array indexing.

{{ rule(id="7.1:21") }}

```rue
struct Container { values: [i32; 3] }

fn main() -> i32 {
    let c = Container { values: [10, 20, 30] };
    c.values[1]  // 20
}
```

## Arrays as Function Parameters

{{ rule(id="7.1:22", cat="normative") }}

Functions **MAY** accept arrays as parameters.

{{ rule(id="7.1:23", cat="normative") }}

Array parameters are passed by value; the entire array is copied to the function.

{{ rule(id="7.1:24") }}

```rue
fn sum(arr: [i32; 3]) -> i32 {
    arr[0] + arr[1] + arr[2]
}

fn main() -> i32 {
    let data: [i32; 3] = [10, 20, 12];
    sum(data)  // 42
}
```

## Array Projection Semantics

{{ rule(id="7.1:25", cat="normative") }}

Array indexing operates as a projection. Reading an element does not move the array itself.

{{ rule(id="7.1:26", cat="normative") }}

When reading an element of a Copy type (e.g., integers, booleans), the element is copied out.

{{ rule(id="7.1:27") }}

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [10, 20, 30];
    let x = arr[0];     // i32 is Copy, so x is a copy
    let y = arr[0];     // Can read same element again
    x + y               // 20
}
```

{{ rule(id="7.1:28", cat="legality-rule") }}

When reading an element of a non-Copy type, it is a compile-time error to move the element out of an array position. Use explicit methods like `swap` or `take` instead.

{{ rule(id="7.1:29") }}

```rue
struct BigThing { value: i32 }

fn main() -> i32 {
    let arr: [BigThing; 2] = [BigThing { value: 1 }, BigThing { value: 2 }];
    let x = arr[0];     // ERROR: cannot move out of indexed position
    0
}
```

{{ rule(id="7.1:30", cat="normative") }}

Array element assignment is an in-place mutation. It modifies the array without moving it.

{{ rule(id="7.1:31") }}

```rue
fn main() -> i32 {
    let mut arr: [i32; 3] = [1, 2, 3];
    arr[0] = 10;        // Mutates in place
    arr[1] = 20;        // Another mutation
    arr[0] + arr[1]     // 30
}
```
