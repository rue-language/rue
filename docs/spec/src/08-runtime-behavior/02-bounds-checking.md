+++
title = "Bounds Checking"
weight = 2
template = "spec/page.html"
+++

# Bounds Checking

{{ rule(id="8.2:1", cat="dynamic-semantics") }}

Array access with an out-of-range index **MUST** cause a runtime panic, taking
effect as of the moment the index is used to navigate into the array (8.2:5); an
index `i` is out of range for an `[T; n]` when `i` is negative or `i ≥ n`, and
either case traps (core calculus `docs/formal/01-core-calculus.md` §6.5, and the
`bounds` trap category of §6.12).

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

For variable indices, an out-of-range access **MUST** trap as if the bound were
tested at the point where the index navigates into the array, before the element
is read (core calculus `docs/formal/01-core-calculus.md` §6.5: the check precedes
the projection that reads the element).

This is an obligation on observable behavior, not on emitted code. The core
calculus rules `(D-Index)` and `(D-Index-Trap)` say what reading an index
*means*; they do not require that a conforming implementation execute a distinct
test instruction at each access. An implementation **MAY** eliminate, combine,
hoist, or otherwise transform the dynamic test — including omitting it entirely
for an access it proves in range — subject to 8.2:9.

{{ rule(id="8.2:9", cat="normative") }}

A transformation of bounds checks **MUST** preserve the trap behavior of the
untransformed program, which is observable (8.0:1). Specifically, such a
transformation:

- **MUST NOT** introduce a trap on an execution that would not have trapped.
  In particular a check **MUST NOT** be speculated onto a path the original
  program does not take, and a check hoisted out of a loop **MUST NOT** trap
  when the loop body never executes.
- **MUST NOT** remove a trap the untransformed program would take.
- **MUST NOT** reorder a trap across an observable effect: every observable
  effect the original program performs before trapping **MUST** still be
  performed before the trap, and an effect the original program reaches only
  after the trap point **MUST NOT** be performed at all.
- **MUST** take the trap the untransformed program would have taken first,
  where more than one access is out of range on the same execution.

An access proven in range on every execution that reaches it therefore needs no
dynamic test: omitting it satisfies each clause above vacuously. Which accesses
an implementation proves is implementation-defined and **MUST NOT** be relied
on — a program cannot observe whether a check was elided, only whether the trap
required by 8.2:1 and 8.2:2 occurs.

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
