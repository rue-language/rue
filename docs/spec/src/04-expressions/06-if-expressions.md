# If Expressions

r[4.6:1#normative]
An if expression conditionally executes one of two branches based on a boolean condition.

r[4.6:2#normative]
```ebnf
if_expr = "if" expression "{" block "}" [ "else" "{" block "}" ] ;
```

r[4.6:3#normative]
The condition expression must have type `bool`.

r[4.6:4#normative]
If an `else` branch is present, both branches must have the same type. The type of the if expression is the type of its branches.

r[4.6:5#normative]
If no `else` branch is present, the `then` branch must have type `()`.

r[4.6:6]
```rue
fn main() -> i32 {
    let x = if true { 42 } else { 0 };
    x
}
```

r[4.6:7#normative]
If the condition evaluates to `true`, the then-branch is executed. Otherwise, the else-branch is executed (if present).

r[4.6:8]
```rue
fn main() -> i32 {
    let n = 5;
    if n > 3 { 100 } else { 0 }
}
```

r[4.6:9]
If expressions can be nested:

```rue
fn main() -> i32 {
    let x = 5;
    if x < 3 { 1 }
    else { if x < 7 { 2 }
    else { 3 } }
}
```
