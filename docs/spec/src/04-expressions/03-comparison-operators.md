# Comparison Operators

r[4.3.1#normative]
Comparison operators compare two values and produce a `bool` result.

## Equality Operators

r[4.3.2#normative]
Equality operators work on integers and booleans.

| Operator | Name | Description |
|----------|------|-------------|
| `==` | Equal | True if operands are equal |
| `!=` | Not equal | True if operands are not equal |

r[4.3.3]
```rue
fn main() -> i32 {
    let a = 1 == 1;    // true
    let b = 1 != 2;    // true
    let c = true == false;  // false (bool equality)
    if a && b && !c { 1 } else { 0 }
}
```

## Ordering Operators

r[4.3.4#normative]
Ordering operators work only on integers.

| Operator | Name | Description |
|----------|------|-------------|
| `<` | Less than | True if left < right |
| `>` | Greater than | True if left > right |
| `<=` | Less or equal | True if left <= right |
| `>=` | Greater or equal | True if left >= right |

r[4.3.5#normative]
Ordering operators on boolean values are a compile-time error.

r[4.3.6]
```rue
fn main() -> i32 {
    let a = 1 < 2;     // true
    let b = 5 >= 5;    // true
    if a && b { 1 } else { 0 }
}
```

## Precedence

r[4.3.7#normative]
Comparison operators have lower precedence than arithmetic operators.

r[4.3.8]
```rue
fn main() -> i32 {
    if 1 + 2 == 3 { 1 } else { 0 }  // 1 (comparison after arithmetic)
}
```

## Type Checking

r[4.3.9#normative]
Both operands of a comparison must have the same type.

r[4.3.10#normative]
When one operand has a known type, the other is inferred to have the same type.
