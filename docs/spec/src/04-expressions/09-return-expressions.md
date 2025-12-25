+++
title = "Return Expressions"
weight = 9
template = "spec/page.html"
+++

# Return Expressions

{{ rule(id="4.9:1", cat="normative") }}

A return expression exits the current function and provides its return value.

{{ rule(id="4.9:2", cat="normative") }}

```ebnf
return_expr = "return" expression? ;
```

{{ rule(id="4.9:3", cat="normative") }}

If the expression is omitted, it is equivalent to `return ()`.

{{ rule(id="4.9:4", cat="legality-rule") }}

The expression following `return` (or the implicit `()`) **MUST** have a type compatible with the function's declared return type.

{{ rule(id="4.9:5", cat="normative") }}

A return expression has the never type `!` because it never produces a local value.

{{ rule(id="4.9:6") }}

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

{{ rule(id="4.9:7", cat="normative") }}

When a return expression is evaluated, the function immediately returns the value of the expression. No further code in the function is executed.

{{ rule(id="4.9:8", cat="normative") }}

Because return has type `!`, it can appear in contexts that expect any type.

{{ rule(id="4.9:9") }}

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

{{ rule(id="4.9:10") }}

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
