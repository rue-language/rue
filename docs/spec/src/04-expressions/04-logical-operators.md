# Logical Operators

r[4.4:1#normative]
Logical operators operate on `bool` values and produce `bool` results.

## Logical NOT

r[4.4:2#normative]
The logical NOT operator `!` negates its operand.

r[4.4:3]
```rue
fn main() -> i32 {
    let a = !false;   // true
    let b = !true;    // false
    let c = !!true;   // true (double negation)
    if a { 1 } else { 0 }
}
```

## Logical AND

r[4.4:4#normative]
The logical AND operator `&&` returns `true` if both operands are `true`.

r[4.4:5#normative]
The `&&` operator uses short-circuit evaluation: if the left operand is `false`, the right operand is not evaluated.

r[4.4:6]
```rue
fn main() -> i32 {
    if true && true { 1 } else { 0 }   // 1
    if true && false { 1 } else { 0 }  // 0
}
```

## Logical OR

r[4.4:7#normative]
The logical OR operator `||` returns `true` if either operand is `true`.

r[4.4:8#normative]
The `||` operator uses short-circuit evaluation: if the left operand is `true`, the right operand is not evaluated.

r[4.4:9]
```rue
fn main() -> i32 {
    if false || true { 1 } else { 0 }  // 1
    if false || false { 1 } else { 0 } // 0
}
```

## Precedence

r[4.4:10#normative]
Operator precedence (highest to lowest):
1. `!` (logical NOT)
2. `&&` (logical AND)
3. `||` (logical OR)

r[4.4:11]
```rue
fn main() -> i32 {
    // true || false && false => true || (false && false) => true
    if true || false && false { 1 } else { 0 }
}
```

## Type Checking

r[4.4:12#normative]
All operands of logical operators must have type `bool`.
