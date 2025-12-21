# Arithmetic Operators

## Binary Arithmetic Operators

r[4.2.1#normative]
Binary arithmetic operators take two operands of the same integer type and produce a result of that type.

| Operator | Name | Description |
|----------|------|-------------|
| `+` | Addition | Sum of operands |
| `-` | Subtraction | Difference of operands |
| `*` | Multiplication | Product of operands |
| `/` | Division | Quotient (integer division) |
| `%` | Remainder | Remainder after division |

## Operator Precedence

r[4.2.2#normative]
Multiplicative operators (`*`, `/`, `%`) have higher precedence than additive operators (`+`, `-`).

r[4.2.3#normative]
Parentheses can be used to override the default precedence of operators. A parenthesized expression evaluates to the value of its inner expression.

r[4.2.13]
```rue
fn main() -> i32 {
    1 + 2 * 3    // = 7 (not 9)
    (1 + 2) * 3  // = 9 (parentheses override)
}
```

## Associativity

r[4.2.4#normative]
All binary arithmetic operators are left-associative.

r[4.2.5#normative]
```rue
fn main() -> i32 {
    10 - 3 - 2   // = 5, parsed as (10 - 3) - 2
    24 / 4 / 2   // = 3, parsed as (24 / 4) / 2
}
```

## Unary Negation

r[4.2.6#normative]
The unary negation operator `-` takes a single integer operand and produces its arithmetic negation.

r[4.2.7#normative]
Unary negation binds tighter than all binary operators.

r[4.2.8]
```rue
fn main() -> i32 {
    -42      // negation
    --5      // double negation = 5
    -2 * 3   // = -6, parsed as (-2) * 3
}
```

## Overflow

r[4.2.9#normative]
Arithmetic operations that overflow the range of their type cause a runtime panic.

r[4.2.10]
```rue
fn main() -> i32 {
    2147483647 + 1  // Runtime error: integer overflow
}
```

## Division by Zero

r[4.2.11#normative]
Division or remainder by zero causes a runtime panic.

r[4.2.12]
```rue
fn main() -> i32 {
    10 / 0  // Runtime error: division by zero
    10 % 0  // Runtime error: division by zero
}
```
