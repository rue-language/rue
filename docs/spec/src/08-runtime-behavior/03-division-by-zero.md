+++
title = "Division by Zero"
weight = 3
template = "spec/page.html"
+++

# Division by Zero

{{ rule(id="8.3:1", cat="dynamic-semantics") }}

Evaluating `/` or `%` on integer operands with a zero divisor **MUST** cause a
runtime panic: the operation traps before it computes a result (core calculus
`docs/formal/01-core-calculus.md` §6.4, rule `(D-Div-Zero)` and its remainder
analogue — these are the distinct `div-zero` and `rem-zero` trap categories of
§6.12).

{{ rule(id="8.3:2", cat="dynamic-semantics") }}

On a division- or remainder-by-zero trap, the program **MUST** terminate with exit
code 101 — the panic exit code of Appendix B — after printing an error message
(core calculus `docs/formal/01-core-calculus.md` §6.12, rule `(Result-Panic)`).

{{ rule(id="8.3:3", cat="normative") }}

Both the division operator (`/`) and remainder operator (`%`) **MAY** cause division-by-zero errors, on integer operands.

{{ rule(id="8.3:4") }}

```rue
fn main() -> i32 {
    10 / 0  // Runtime error: division by zero
}
```

{{ rule(id="8.3:5") }}

```rue
fn main() -> i32 {
    10 % 0  // Runtime error: division by zero
}
```

{{ rule(id="8.3:6") }}

```rue
fn main() -> i32 {
    let divisor = 5 - 5;
    10 / divisor  // Runtime error: division by zero
}
```

## Floating-Point Division

{{ rule(id="8.3:7", cat="informative") }}

This chapter is about integer division and remainder. Floating-point division
is total and never traps: `x / 0.0` is an infinity, `0.0 / 0.0` is a NaN, and
the whole rule is stated in
[Floating-Point Types](@/03-types/12-floating-point-types.md) 3.12:22. `%` has
no floating-point form at all (3.12:25), so there is no floating-point
remainder-by-zero case to classify.
