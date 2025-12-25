+++
title = "Arithmetic Operators"
weight = 2
template = "spec/page.html"
+++

# Arithmetic Operators

## Binary Arithmetic Operators

{{ rule(id="4.2:1", cat="normative") }}

Binary arithmetic operators take two operands of the same integer type and produce a result of that type.

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
    1 + 2 * 3    // = 7 (not 9)
    (1 + 2) * 3  // = 9 (parentheses override)
}
```

## Associativity

{{ rule(id="4.2:4", cat="normative") }}

All binary arithmetic operators are left-associative.

{{ rule(id="4.2:5", cat="normative") }}

```rue
fn main() -> i32 {
    10 - 3 - 2   // = 5, parsed as (10 - 3) - 2
    24 / 4 / 2   // = 3, parsed as (24 / 4) / 2
}
```

## Unary Negation

{{ rule(id="4.2:6", cat="normative") }}

The unary negation operator `-` takes a single signed integer operand and produces its arithmetic negation.

{{ rule(id="4.2:14", cat="normative") }}

Unary negation on unsigned integer types is a compile-time error.

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

{{ rule(id="4.2:16", cat="normative") }}

When negation is applied to a non-literal expression holding the minimum value of a signed integer type, the operation overflows and causes a runtime panic.

{{ rule(id="4.2:17") }}

```rue
fn main() -> i32 {
    let x: i8 = -128;    // valid: compile-time constant
    let y: i8 = -x;      // runtime panic: negating -128 overflows
    0
}
```

## Overflow

{{ rule(id="4.2:9", cat="normative") }}

Arithmetic operations that overflow the range of their type cause a runtime panic.

{{ rule(id="4.2:10") }}

```rue
fn main() -> i32 {
    2147483647 + 1  // Runtime error: integer overflow
}
```

## Division by Zero

{{ rule(id="4.2:11", cat="normative") }}

Division or remainder by zero causes a runtime panic.

{{ rule(id="4.2:12") }}

```rue
fn main() -> i32 {
    10 / 0  // Runtime error: division by zero
    10 % 0  // Runtime error: division by zero
}
```
