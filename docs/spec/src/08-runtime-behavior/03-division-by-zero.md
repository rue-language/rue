+++
title = "Division by Zero"
weight = 3
template = "spec/page.html"
+++

# Division by Zero

{{ rule(id="8.3:1", cat="dynamic-semantics") }}

Evaluating `/` or `%` with a zero divisor **MUST** cause a runtime panic: the
operation traps before it computes a result (core calculus
`docs/formal/01-core-calculus.md` §6.4, rule `(D-Div-Zero)` and its remainder
analogue — these are the distinct `div-zero` and `rem-zero` trap categories of
§6.12).

{{ rule(id="8.3:2", cat="dynamic-semantics") }}

On a division- or remainder-by-zero trap, the program **MUST** terminate with exit
code 101 — the panic exit code of Appendix B — after printing an error message
(core calculus `docs/formal/01-core-calculus.md` §6.12, rule `(Result-Panic)`).

{{ rule(id="8.3:3", cat="normative") }}

Both the division operator (`/`) and remainder operator (`%`) **MAY** cause division-by-zero errors.

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
