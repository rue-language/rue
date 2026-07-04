+++
title = "Return Expressions"
weight = 9
template = "spec/page.html"
+++

# Return Expressions

{{ rule(id="4.9:1", cat="normative") }}

A return expression exits the current function and provides its return value.

{{ rule(id="4.9:2", cat="syntax") }}

```ebnf
return_expr = "return" expression? ;
```

{{ rule(id="4.9:3", cat="normative") }}

If the expression is omitted, it is equivalent to `return ()`.

{{ rule(id="4.9:4", cat="legality-rule") }}

The operand of `return` — the written expression, or the `()` of the omitted form (4.9:3) — **MUST** have the function's declared return type. As everywhere, the one admitted coercion is from the never type (3.4:3); no other type difference is accepted.

{{ rule(id="4.9:5", cat="normative") }}

A return expression has the never type `!` (core calculus `docs/formal/01-core-calculus.md` §5.7, rule `(Return)`).

{{ rule(id="4.9:11", cat="informative") }}

`return` transfers control away instead of yielding a value to its own surrounding context; 3.4:2 lists the control-transfer forms that have type `!`.

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

{{ rule(id="4.9:7", cat="dynamic-semantics") }}

When a return expression is evaluated, its operand's value becomes the function's return value and control leaves the function: the live bindings of the function's still-open scopes are dropped (3.9:18), and no further expression in the function is evaluated (core calculus `docs/formal/01-core-calculus.md` §6.9, rule `(D-Return)`).

{{ rule(id="4.9:8", cat="normative") }}

Because a return expression has type `!`, it may appear in any context where a value of any type is expected (3.4:4).

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
