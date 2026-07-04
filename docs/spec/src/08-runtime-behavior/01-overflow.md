+++
title = "Integer Overflow"
weight = 1
template = "spec/page.html"
+++

# Integer Overflow

{{ rule(id="8.1:1", cat="dynamic-semantics") }}

Integer overflow during an arithmetic operation **MUST** cause a runtime panic:
when the mathematical result of the operation falls outside the range
`[min, max]` of its integer type, the operation traps rather than wrapping or
truncating (core calculus `docs/formal/01-core-calculus.md` §6.4, rule
`(D-Arith-Trap)`, and the `neg (min_T)_T → ↯overflow` case for unary negation).

{{ rule(id="8.1:2", cat="dynamic-semantics") }}

On an overflow trap, the program **MUST** terminate with exit code 101 — the panic
exit code of Appendix B — after printing an error message identifying the overflow
(core calculus `docs/formal/01-core-calculus.md` §6.12, rule `(Result-Panic)`).

{{ rule(id="8.1:3", cat="normative") }}

The following operations **MAY** overflow (core calculus
`docs/formal/01-core-calculus.md` §6.4: `+`, `-`, `*`, and `neg` trap by rule
`(D-Arith-Trap)`; the `min / -1` and `min % -1` cases trap by rule
`(D-Div-Overflow)`):
- Addition (`+`)
- Subtraction (`-`)
- Multiplication (`*`)
- Negation (`-` unary)
- Division (`/`) and remainder (`%`), exactly when the dividend is the
  signed type's minimum value and the divisor is `-1` (the quotient
  `-MIN` is not representable; the remainder operation overflows in the
  same case even though its mathematical result would be `0`)

{{ rule(id="8.1:4") }}

```rue
fn main() -> i32 {
    2147483647 + 1  // Runtime error: integer overflow
}
```

{{ rule(id="8.1:5") }}

```rue
fn main() -> i32 {
    -2147483648 - 1  // Runtime error: integer overflow
}
```

{{ rule(id="8.1:6") }}

Future versions of Rue may provide wrapping arithmetic operations that do not panic on overflow.
