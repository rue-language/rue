+++
title = "Array Types"
weight = 5
template = "spec/page.html"
+++

# Array Types

{{ rule(id="3.5:1", cat="normative") }}

An array type, written `[T; N]`, represents a fixed-size sequence of `N` elements of type `T`.

{{ rule(id="3.5:2", cat="normative") }}

The length `N` **MUST** be a non-negative integer literal known at compile time.

{{ rule(id="3.5:3", cat="normative") }}

All elements of an array **MUST** have the same type `T`.

{{ rule(id="3.5:4", cat="normative") }}

Arrays are stored contiguously in memory in **ascending** order: element `i` is located at offset `i * size_of(T)` from the start of the array, so element `0` occupies the array's lowest address and element `N-1` its highest. Consequently array indexing and pointer arithmetic (`@ptr_offset`, §9.2) agree by construction — both advance by `size_of(T)` per element. The size of `[T; N]` is `N * size_of(T)`. Zero-length arrays `[T; 0]` are zero-sized types. See [Zero-Sized Types](../#zero-sized-types) for the general definition. (Struct field layout, by contrast, remains implementation-defined; see ADR-0040.)

{{ rule(id="3.5:5") }}

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [10, 20, 12];
    arr[0] + arr[1] + arr[2]  // 42
}
```

{{ rule(id="3.5:6", cat="normative") }}

Array elements are accessed using index syntax `arr[index]`.

{{ rule(id="3.5:7", cat="normative") }}

For constant indices, bounds checking is performed at compile time.

{{ rule(id="3.5:8", cat="normative") }}

For variable indices, bounds checking is performed at runtime.

{{ rule(id="3.5:9", cat="normative") }}

Accessing an array with an out-of-bounds index causes a runtime panic.

{{ rule(id="3.5:10") }}

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    let idx = 5;
    arr[idx]  // Runtime error: index out of bounds
}
```
