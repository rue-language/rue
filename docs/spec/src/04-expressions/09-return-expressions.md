# Return Expressions

r[4.9.1#normative]
A return expression exits the current function and provides its return value.

r[4.9.2#normative]
```ebnf
return_expr = "return" expression? ;
```

r[4.9.3#normative]
If the expression is omitted, it is equivalent to `return ()`.

r[4.9.4#normative]
The expression following `return` (or the implicit `()`) must have a type compatible with the function's declared return type.

r[4.9.5#normative]
A return expression has the never type `!` because it never produces a local value.

r[4.9.6]
```rue
fn abs(x: i32) -> i32 {
    if x < 0 {
        return 0 - x;
    }
    x
}

fn main() -> i32 {
    abs(-5)  // 5
}
```

r[4.9.7#normative]
When a return expression is evaluated, the function immediately returns the value of the expression. No further code in the function is executed.

r[4.9.8#normative]
Because return has type `!`, it can appear in contexts that expect any type.

r[4.9.9]
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

r[4.9.10]
```rue
fn do_nothing() {
    return;  // equivalent to return ()
}

fn explicit_return() {
    return ();  // explicit unit return
}

fn main() -> i32 {
    do_nothing();
    explicit_return();
    0
}
```
