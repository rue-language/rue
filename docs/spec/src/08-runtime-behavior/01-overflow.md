+++
title = "Integer Overflow"
weight = 1
template = "spec/page.html"
+++

# Integer Overflow

{{ rule(id="8.1:1", cat="dynamic-semantics") }}

Integer overflow during arithmetic operations **MUST** cause a runtime panic.

{{ rule(id="8.1:2", cat="dynamic-semantics") }}

On overflow, the program **MUST** terminate with exit code 101 and print an error message.

{{ rule(id="8.1:3", cat="normative") }}

The following operations **MAY** overflow:
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
