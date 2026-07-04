+++
title = "Bounds Checking"
weight = 2
template = "spec/page.html"
+++

# Bounds Checking

{{ rule(id="8.2:1", cat="dynamic-semantics") }}

Array access with an out-of-range index **MUST** cause a runtime panic. The bound
is checked at the moment the index is used to navigate into the array; an index
`i` is out of range for an `[T; n]` when `i` is negative or `i ≥ n`, and either
case traps (core calculus `docs/formal/01-core-calculus.md` §6.5, and the `bounds`
trap category of §6.12).

{{ rule(id="8.2:2", cat="dynamic-semantics") }}

On an out-of-range access, the program **MUST** terminate with exit code 101 — the
panic exit code of Appendix B — after printing an error message identifying the
out-of-bounds access (core calculus `docs/formal/01-core-calculus.md` §6.12, rule
`(Result-Panic)`).

{{ rule(id="8.2:3", cat="legality-rule") }}

For constant indices, bounds checking **MUST** be performed at compile time. A
constant index that is out of range is rejected during compilation and so never
reaches the runtime `bounds` check of the core calculus (§6.5).

{{ rule(id="8.2:4", cat="normative") }}

A constant index is an expression that can be fully evaluated at compile time. This includes integer literals, arithmetic operations on constants, comparison operations on constants, and parenthesized constant expressions.

{{ rule(id="8.2:5", cat="dynamic-semantics") }}

For variable indices, bounds checking **MUST** be performed at runtime, before the
element is read, at the point where the index navigates into the array (core
calculus `docs/formal/01-core-calculus.md` §6.5: the check precedes the projection
that reads the element).

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
