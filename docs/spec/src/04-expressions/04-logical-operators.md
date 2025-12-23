+++
title = "Logical Operators"
weight = 4
template = "spec/page.html"
+++

# Logical Operators

{{ rule(id="4.4:1", cat="normative") }}

Logical operators operate on `bool` values and produce `bool` results.

## Logical NOT

{{ rule(id="4.4:2", cat="normative") }}

The logical NOT operator `!` negates its operand.

{{ rule(id="4.4:3") }}

```rue
fn main() -> i32 {
    let a = !false;   // true
    let b = !true;    // false
    let c = !!true;   // true (double negation)
    if a { 1 } else { 0 }
}
```

## Logical AND

{{ rule(id="4.4:4", cat="normative") }}

The logical AND operator `&&` returns `true` if both operands are `true`.

{{ rule(id="4.4:5", cat="normative") }}

The `&&` operator uses short-circuit evaluation: if the left operand is `false`, the right operand is not evaluated.

{{ rule(id="4.4:6") }}

```rue
fn main() -> i32 {
    if true && true { 1 } else { 0 }   // 1
    if true && false { 1 } else { 0 }  // 0
}
```

## Logical OR

{{ rule(id="4.4:7", cat="normative") }}

The logical OR operator `||` returns `true` if either operand is `true`.

{{ rule(id="4.4:8", cat="normative") }}

The `||` operator uses short-circuit evaluation: if the left operand is `true`, the right operand is not evaluated.

{{ rule(id="4.4:9") }}

```rue
fn main() -> i32 {
    if false || true { 1 } else { 0 }  // 1
    if false || false { 1 } else { 0 } // 0
}
```

## Precedence

{{ rule(id="4.4:10", cat="normative") }}

Operator precedence (highest to lowest):
1. `!` (logical NOT)
2. `&&` (logical AND)
3. `||` (logical OR)

{{ rule(id="4.4:11") }}

```rue
fn main() -> i32 {
    // true || false && false => true || (false && false) => true
    if true || false && false { 1 } else { 0 }
}
```

## Type Checking

{{ rule(id="4.4:12", cat="normative") }}

All operands of logical operators must have type `bool`.
