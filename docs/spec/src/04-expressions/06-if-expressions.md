+++
title = "If Expressions"
weight = 6
template = "spec/page.html"
+++

# If Expressions

{{ rule(id="4.6:1", cat="normative") }}

An if expression conditionally executes one of two branches based on a boolean condition.

{{ rule(id="4.6:2", cat="syntax") }}

```ebnf
if_expr     = "if" expression "{" block "}" [ else_clause ] ;
else_clause = "else" ( "{" block "}" | if_expr ) ;
```

{{ rule(id="4.6:3", cat="normative") }}

The condition expression must have type `bool`.

{{ rule(id="4.6:4", cat="normative") }}

If an `else` branch is present, both branches must have the same type. The type of the if expression is the type of its branches.

{{ rule(id="4.6:5", cat="normative") }}

If no `else` branch is present, the `then` branch must have type `()`.

{{ rule(id="4.6:6") }}

```rue
fn main() -> i32 {
    let x = if true { 42 } else { 0 };
    x
}
```

{{ rule(id="4.6:7", cat="normative") }}

If the condition evaluates to `true`, the then-branch is executed. Otherwise, the else-branch is executed (if present).

{{ rule(id="4.6:8") }}

```rue
fn main() -> i32 {
    let n = 5;
    if n > 3 { 100 } else { 0 }
}
```

{{ rule(id="4.6:9", cat="example") }}

If expressions can be chained using `else if`:

```rue
fn main() -> i32 {
    let x = 5;
    if x < 3 { 1 }
    else if x < 7 { 2 }
    else { 3 }
}
```
