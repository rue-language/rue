+++
title = "Assignment Statements"
weight = 2
template = "spec/page.html"
+++

# Assignment Statements

{{ rule(id="5.2:1", cat="normative") }}

An assignment statement assigns a new value to a mutable variable.

{{ rule(id="5.2:2", cat="normative") }}

```ebnf
assign_stmt = IDENT "=" expression ";"
            | IDENT "[" expression "]" "=" expression ";"
            | expression "." IDENT "=" expression ";" ;
```

## Variable Assignment

{{ rule(id="5.2:3", cat="normative") }}

The variable must have been declared with `let mut`.

{{ rule(id="5.2:4", cat="normative") }}

The expression type must be compatible with the variable's type.

{{ rule(id="5.2:5") }}

```rue
fn main() -> i32 {
    let mut x = 0;
    x = 42;
    x
}
```

## Array Element Assignment

{{ rule(id="5.2:6", cat="normative") }}

Array element assignment requires a mutable array.

{{ rule(id="5.2:7") }}

```rue
fn main() -> i32 {
    let mut arr: [i32; 2] = [0, 0];
    arr[0] = 20;
    arr[1] = 22;
    arr[0] + arr[1]
}
```

## Struct Field Assignment

{{ rule(id="5.2:8", cat="normative") }}

Struct field assignment requires a mutable struct value.

{{ rule(id="5.2:9") }}

```rue
struct Point { x: i32, y: i32 }

fn main() -> i32 {
    let mut p = Point { x: 0, y: 0 };
    p.x = 42;
    p.x
}
```

## Assignment is Not an Expression

{{ rule(id="5.2:10", cat="normative") }}

Assignment is a statement, not an expression. It cannot be used in expression position.
