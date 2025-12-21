# Call Expressions

r[4.10.1#normative]
A call expression invokes a function with a list of arguments.

r[4.10.2#normative]
```ebnf
call_expr = expression "(" [ expression { "," expression } ] ")" ;
```

r[4.10.3#normative]
The number of arguments must match the number of parameters in the function signature.

r[4.10.4#normative]
Each argument type must be compatible with the corresponding parameter type.

r[4.10.5#normative]
The type of a call expression is the function's return type.

r[4.10.6]
```rue
fn add(x: i32, y: i32) -> i32 {
    x + y
}

fn main() -> i32 {
    add(40, 2)  // 42
}
```

r[4.10.7#normative]
Arguments are evaluated left-to-right before the function is called.

r[4.10.8]
Call expressions can be nested:

```rue
fn add(x: i32, y: i32) -> i32 { x + y }

fn main() -> i32 {
    add(add(10, 20), add(5, 7))  // 42
}
```
