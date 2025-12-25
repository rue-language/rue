+++
title = "Call Expressions"
weight = 10
template = "spec/page.html"
+++

# Call Expressions

{{ rule(id="4.10:1", cat="normative") }}

A call expression invokes a function with a list of arguments.

{{ rule(id="4.10:2", cat="normative") }}

```ebnf
call_expr = expression "(" [ expression { "," expression } ] ")" ;
```

{{ rule(id="4.10:3", cat="legality-rule") }}

The number of arguments **MUST** match the number of parameters in the function signature.

{{ rule(id="4.10:4", cat="legality-rule") }}

Each argument type **MUST** be compatible with the corresponding parameter type.

{{ rule(id="4.10:5", cat="normative") }}

The type of a call expression is the function's return type.

{{ rule(id="4.10:6") }}

```rue
fn add(x: i32, y: i32) -> i32 {
    x + y
}

fn main() -> i32 {
    add(40, 2)  // 42
}
```

{{ rule(id="4.10:7", cat="normative") }}

Arguments are evaluated left-to-right before the function is called, as specified in section 4.0.

{{ rule(id="4.10:8") }}

Call expressions can be nested:

```rue
fn add(x: i32, y: i32) -> i32 { x + y }

fn main() -> i32 {
    add(add(10, 20), add(5, 7))  // 42
}
```
