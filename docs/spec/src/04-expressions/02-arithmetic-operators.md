+++
title = "Arithmetic Operators"
weight = 2
template = "spec/page.html"
+++

# Arithmetic Operators

## Binary Arithmetic Operators

{{ rule(id="4.2:1", cat="normative") }}

Binary arithmetic operators take two operands of the same integer type and
produce a value of that same type (core calculus
`docs/formal/01-core-calculus.md` §5.8, rule `(Arith)`). Each operator denotes
the corresponding operation on the two operands' integer values, computed
exactly over the mathematical integers: when that exact result lies within the
operand type's range it is the value produced, and when it does not the
operation traps at runtime rather than wrapping (§6.4, rules
`(D-Arith)`/`(D-Arith-Trap)`; see 4.2:9). Division and remainder additionally
trap on a zero divisor (4.2:11).

| Operator | Name | Description |
|----------|------|-------------|
| `+` | Addition | Sum of operands |
| `-` | Subtraction | Difference of operands |
| `*` | Multiplication | Product of operands |
| `/` | Division | Quotient (integer division) |
| `%` | Remainder | Remainder after division |

## Operator Precedence

{{ rule(id="4.2:2", cat="normative") }}

Multiplicative operators (`*`, `/`, `%`) have higher precedence than additive operators (`+`, `-`).

{{ rule(id="4.2:3", cat="normative") }}

Parentheses can be used to override the default precedence of operators. A parenthesized expression evaluates to the value of its inner expression.

{{ rule(id="4.2:13") }}

```rue
fn main() -> i32 {
    @dbg(1 + 2 * 3);      // = 7 (not 9)
    @dbg((1 + 2) * 3);    // = 9 (parentheses override)
    0
}
```

## Associativity

{{ rule(id="4.2:4", cat="normative") }}

All binary arithmetic operators are left-associative.

{{ rule(id="4.2:5", cat="normative") }}

```rue
fn main() -> i32 {
    @dbg(10 - 3 - 2);    // = 5, parsed as (10 - 3) - 2
    @dbg(24 / 4 / 2);    // = 3, parsed as (24 / 4) / 2
    0
}
```

## Unary Negation

{{ rule(id="4.2:6", cat="normative") }}

The unary negation operator `-` takes a single signed integer operand and
produces the arithmetic negation of the operand's value, computed exactly over
the mathematical integers (core calculus `docs/formal/01-core-calculus.md` §6.4,
the `neg` case of `(D-Arith)`). The only value whose negation is not
representable is the type's minimum: negating it has no in-range result and
traps at runtime (4.2:16), except for the compile-time literal case of 4.2:15.

{{ rule(id="4.2:14", cat="legality-rule") }}

A compiler **MUST** reject unary negation whose operand is not a signed integer
type. Unsigned integer types have no negative range, and non-numeric types
(such as `bool` or `()`) are not negatable at all; applying `-` to any of them
is a compile-time error.

{{ rule(id="4.2:7", cat="normative") }}

Unary negation binds tighter than all binary operators.

{{ rule(id="4.2:8") }}

```rue
fn main() -> i32 {
    -42      // negation
    --5      // double negation = 5
    -2 * 3   // = -6, parsed as (-2) * 3
}
```

{{ rule(id="4.2:15", cat="normative") }}

When a negated integer literal represents the minimum value of a signed integer type, the compiler evaluates the negation at compile time and produces the minimum value directly. This special case allows expressions like `-128: i8` without runtime overflow.

{{ rule(id="4.2:16", cat="dynamic-semantics") }}

When negation is applied to a non-literal expression holding the minimum value
of a signed integer type, the operation overflows and **MUST** cause a runtime
panic (core calculus `docs/formal/01-core-calculus.md` §6.4: `neg (min_T)_T →
↯overflow`).

{{ rule(id="4.2:17") }}

```rue
fn main() -> i32 {
    let x: i8 = -128;    // valid: compile-time constant
    let y: i8 = -x;      // runtime panic: negating -128 overflows
    0
}
```

## Overflow

{{ rule(id="4.2:9", cat="dynamic-semantics") }}

Arithmetic operations that overflow the range of their type **MUST** cause a
runtime panic; the result is never silently wrapped or truncated (core calculus
`docs/formal/01-core-calculus.md` §6.4, rule `(D-Arith-Trap)`).

{{ rule(id="4.2:10") }}

```rue
fn main() -> i32 {
    2147483647 + 1  // Runtime error: integer overflow
}
```

## Division by Zero

{{ rule(id="4.2:11", cat="dynamic-semantics") }}

Division or remainder by zero **MUST** cause a runtime panic (core calculus
`docs/formal/01-core-calculus.md` §6.4, rules `(D-Div-Zero)` and the
corresponding `↯rem-zero` trap for `%`). Signed division or remainder of a
type's minimum value by `-1` likewise traps as an overflow (§6.4,
`(D-Div-Overflow)`).

{{ rule(id="4.2:12") }}

```rue
fn main() -> i32 {
    10 / 0  // Runtime error: division by zero
    10 % 0  // Runtime error: division by zero
}
```
