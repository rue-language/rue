+++
title = "Comparison Operators"
weight = 3
template = "spec/page.html"
+++

# Comparison Operators

{{ rule(id="4.3:1", cat="normative") }}

Comparison operators compare two values and produce a `bool` result.

## Equality Operators

{{ rule(id="4.3:2", cat="normative") }}

Equality operators work on integers, booleans, and strings.

| Operator | Name | Description |
|----------|------|-------------|
| `==` | Equal | True if operands are equal |
| `!=` | Not equal | True if operands are not equal |

{{ rule(id="4.3:3", cat="normative") }}

Two strings are equal if they have the same length and identical byte content.

{{ rule(id="4.3:4") }}

```rue
fn main() -> i32 {
    let a = 1 == 1;    // true
    let b = 1 != 2;    // true
    let c = true == false;  // false (bool equality)
    let d = "hello" == "hello";  // true (string equality)
    if a && b && !c && d { 1 } else { 0 }
}
```

## Ordering Operators

{{ rule(id="4.3:5", cat="normative") }}

Ordering operators work only on integers.

| Operator | Name | Description |
|----------|------|-------------|
| `<` | Less than | True if left < right |
| `>` | Greater than | True if left > right |
| `<=` | Less or equal | True if left <= right |
| `>=` | Greater or equal | True if left >= right |

{{ rule(id="4.3:6", cat="normative") }}

Ordering operators on boolean or string values are a compile-time error.

{{ rule(id="4.3:7") }}

```rue
fn main() -> i32 {
    let a = 1 < 2;     // true
    let b = 5 >= 5;    // true
    if a && b { 1 } else { 0 }
}
```

## Precedence

{{ rule(id="4.3:8", cat="normative") }}

Comparison operators have lower precedence than arithmetic operators.

{{ rule(id="4.3:9") }}

```rue
fn main() -> i32 {
    if 1 + 2 == 3 { 1 } else { 0 }  // 1 (comparison after arithmetic)
}
```

## Type Checking

{{ rule(id="4.3:10", cat="normative") }}

Both operands of a comparison must have the same type.

{{ rule(id="4.3:11", cat="normative") }}

When one operand has a known type, the other is inferred to have the same type.

## Associativity

{{ rule(id="4.3:12", cat="legality-rule") }}

Comparison operators cannot be chained. Expressions like `a < b < c` or `a == b == c` are compile-time errors.

{{ rule(id="4.3:13", cat="example") }}

To compare multiple values, use logical operators:

```rue
fn main() -> i32 {
    let a = 1;
    let b = 2;
    let c = 3;
    if a < b && b < c { 1 } else { 0 }  // correct way to chain comparisons
}
```
