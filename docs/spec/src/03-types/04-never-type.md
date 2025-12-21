# Never Type

r[3.4.1#normative]
The never type, written `!`, is the type of expressions that never produce a value.

r[3.4.2#normative]
Expressions of type `!` include:
- `return` expressions
- `break` expressions
- `continue` expressions
- Infinite loops

r[3.4.3#normative]
The never type can coerce to any other type. This allows diverging expressions to appear in any context.

r[3.4.4]
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

r[3.4.5#normative]
A function with return type `!` never returns normally.
