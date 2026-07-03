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

## Array-Repeat Literals

{{ rule(id="7.1:36", cat="normative") }}

```ebnf
array_literal = "[" ( [ expression { "," expression } ]
                    | expression ";" array_length ) "]" ;
```

An array literal has two forms. The *list form* `[e0, e1, …]` gives each
element explicitly. The *repeat form* `[value; count]` constructs an array of
`count` elements, each equal to `value`. The result type is `[T; count]` where
`T` is the type of `value`.

{{ rule(id="7.1:37", cat="legality-rule") }}

In the repeat form, `count` **MUST** be a non-negative integer compile-time
constant — an integer literal or an identifier naming a compile-time constant
(a file-level `const` or an in-scope `comptime` value parameter), resolved by
the same rules as an array-type length.

{{ rule(id="7.1:38", cat="legality-rule") }}

The element type of a repeat literal **MUST** be `Copy`. A repeat literal
materializes `count` copies of a single value, which is only well-defined when
the value can be copied; a non-Copy element type is a compile-time error.

{{ rule(id="7.1:39", cat="dynamic-semantics") }}

The `value` expression is evaluated exactly once; its result is copied into
each of the `count` array slots.

{{ rule(id="7.1:40") }}

```rue
fn main() -> i32 {
    let zeros: [i32; 128] = [0; 128];   // 128 copies of 0
    let sevens = [7; 3];                 // [7, 7, 7]
    zeros[0] + sevens[0] + sevens[1] + sevens[2]  // 21
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
array_type   = "[" type ";" array_length "]" ;
array_length = INTEGER | IDENTIFIER ;
```

{{ rule(id="7.1:15", cat="legality-rule") }}

The length **MUST** be a non-negative integer value known at compile time. It is
either an integer literal, or an identifier naming a compile-time constant — a
file-level `const` or an in-scope `comptime` value parameter.

## Compile-Time Array Lengths

{{ rule(id="7.1:32", cat="normative") }}

An array length **MAY** be an identifier naming a compile-time constant in
addition to an integer literal. A file-level `const` of an integer type and a
`comptime` value parameter both qualify.

{{ rule(id="7.1:33", cat="legality-rule") }}

A named array length **MUST** resolve to a non-negative integer compile-time
constant. A runtime variable, a negative value, or an undefined name in
length position is a compile-time error.

{{ rule(id="7.1:34", cat="normative") }}

When an array length names a `comptime` value parameter, the length is resolved
at each specialization. The same generic definition therefore yields arrays of
different sizes for different argument values, so a `comptime` value parameter
can parameterize a type's memory layout — for example a fixed-capacity buffer or
stack — and not only its behavior.

{{ rule(id="7.1:35") }}

```rue
fn Buffer(comptime N: i32) -> type {
    struct { data: [i32; N], len: u32 }
}

fn main() -> i32 {
    let B2 = Buffer(2);
    let B4 = Buffer(4);
    let b2: B2 = B2 { data: [1, 2], len: 2 };
    let b4: B4 = B4 { data: [10, 20, 30, 40], len: 4 };
    b2.data[1] + b4.data[3]  // 42
}
```

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

When reading an element of a non-Copy type, the read moves the element out of the array only when the index is a compile-time constant and the indexing applies directly to an array variable (per-element move tracking; see Array Element Moves in the Move Semantics chapter). Any other such read — a non-constant index, or an array reached through another projection or computed by an expression — is a compile-time error: with a runtime index the compiler cannot know which element moved, so neither use-after-move checking nor drop elaboration could remain sound.

{{ rule(id="7.1:29") }}

```rue
struct BigThing { value: i32 }

fn first_index() -> u64 { 0 }

fn main() -> i32 {
    let arr: [BigThing; 2] = [BigThing { value: 1 }, BigThing { value: 2 }];
    let x = arr[0];             // OK: constant index moves arr[0] out
    let i = first_index();
    let y = arr[i];             // ERROR: cannot move out of indexed position
    x.value
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
