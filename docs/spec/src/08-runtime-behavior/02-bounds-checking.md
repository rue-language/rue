+++
title = "Bounds Checking"
weight = 2
template = "spec/page.html"
+++

# Bounds Checking

{{ rule(id="8.2:1", cat="dynamic-semantics") }}

Array access with an out-of-bounds index **MUST** cause a runtime panic.

{{ rule(id="8.2:2", cat="dynamic-semantics") }}

On out-of-bounds access, the program **MUST** terminate with exit code 101 and print an error message.

{{ rule(id="8.2:3", cat="legality-rule") }}

For constant indices, bounds checking **MUST** be performed at compile time.

{{ rule(id="8.2:4", cat="normative") }}

A constant index is an expression that can be fully evaluated at compile time. This includes integer literals, arithmetic operations on constants, comparison operations on constants, and parenthesized constant expressions.

{{ rule(id="8.2:5", cat="dynamic-semantics") }}

For variable indices, bounds checking **MUST** be performed at runtime before the access.

{{ rule(id="8.2:6") }}

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    let idx: u64 = 10;
    arr[idx]  // Runtime error: index out of bounds
}
```

{{ rule(id="8.2:7") }}

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    arr[5]  // Compile-time error: index out of bounds
}
```

{{ rule(id="8.2:8") }}

```rue
fn main() -> i32 {
    let arr: [i32; 3] = [1, 2, 3];
    arr[1 + 5]  // Compile-time error: index out of bounds
}
```
