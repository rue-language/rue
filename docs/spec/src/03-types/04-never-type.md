# Never Type

r[3.4:1#normative]
The never type, written `!`, is the type of expressions that never produce a value.

r[3.4:2#normative]
Expressions of type `!` include:
- `return` expressions
- `break` expressions
- `continue` expressions
- Infinite loops

## Type Coercion

r[3.4:3#normative]
A type coercion is an implicit type conversion that occurs automatically during type checking. Rue has exactly one coercion: the never type coerces to any type.

r[3.4:4#normative]
When type checking requires a value of type `T`, a value of type `!` is accepted. This allows diverging expressions to appear in any context where a value is expected.

r[3.4:5]
```rue
fn test(x: i32) -> i32 {
    // `return 100` has type !, which coerces to i32
    let y = if x > 5 { return 100 } else { x };
    y * 2
}

fn main() -> i32 {
    test(3) + test(10)  // 6 + 100 = 106
}
```

r[3.4:6#normative]
When both branches of an `if` expression or all arms of a `match` expression have type `!`, the entire expression has type `!`.

r[3.4:7]
```rue
fn diverges(x: i32) -> i32 {
    // Both branches return, so the if has type !
    // This coerces to i32 (the function's return type)
    if x > 0 { return 1 } else { return 0 }
}

fn main() -> i32 { diverges(5) }
```

## Diverging Functions

r[3.4:8#normative]
A function with return type `!` never returns normally.
