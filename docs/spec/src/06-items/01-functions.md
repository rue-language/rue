+++
title = "Functions"
weight = 1
template = "spec/page.html"
+++

# Functions

{{ rule(id="6.1:1", cat="normative") }}

A function is defined using the `fn` keyword.

{{ rule(id="6.1:2", cat="normative") }}

```ebnf
function = "fn" IDENT "(" [ params ] ")" [ "->" type ] "{" block "}" ;
params = param { "," param } ;
param = IDENT ":" type ;
```

## Function Signature

{{ rule(id="6.1:3", cat="legality-rule") }}

Parameters **MUST** have explicit type annotations.

{{ rule(id="6.1:4", cat="legality-rule") }}

If a return type is specified, the function body **MUST** produce a value of that type.

{{ rule(id="6.1:5", cat="normative") }}

If no return type is specified, the function returns `()`.

{{ rule(id="6.1:6", cat="normative") }}

```rue
fn add(x: i32, y: i32) -> i32 {
    x + y
}

fn do_nothing() {
    // implicitly returns ()
}
```

## Entry Point

{{ rule(id="6.1:7", cat="legality-rule") }}

A program **MUST** have a function named `main`.

{{ rule(id="6.1:8", cat="legality-rule") }}

The `main` function **MUST** return either `i32` or `()`. When it returns `i32`, that value becomes the program's exit code. When it returns `()`, the exit code is 0.

{{ rule(id="6.1:9") }}

```rue
fn main() -> i32 {
    42  // exit code is 42
}
```

## Recursion

{{ rule(id="6.1:10", cat="normative") }}

Functions **MAY** call themselves recursively.

{{ rule(id="6.1:11") }}

```rue
fn factorial(n: i32) -> i32 {
    if n <= 1 { 1 }
    else { n * factorial(n - 1) }
}

fn main() -> i32 {
    factorial(5)  // 120
}
```

## Function Visibility

{{ rule(id="6.1:12", cat="normative") }}

Functions **MAY** call any function defined in the same module, regardless of definition order.

{{ rule(id="6.1:13") }}

```rue
fn main() -> i32 {
    helper()  // can call function defined below
}

fn helper() -> i32 {
    42
}
```
