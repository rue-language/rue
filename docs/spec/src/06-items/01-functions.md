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

## Inout Parameters

{{ rule(id="6.1:14", cat="normative") }}

A parameter **MAY** be marked with the `inout` keyword to indicate that it is passed by reference and may be mutated by the callee. Changes made to an `inout` parameter are visible to the caller after the call returns.

{{ rule(id="6.1:15", cat="syntax") }}

```ebnf
param = [ "inout" ] IDENT ":" type ;
```

{{ rule(id="6.1:16", cat="legality-rule") }}

At the call site, an argument passed to an `inout` parameter **MUST** be marked with the `inout` keyword.

{{ rule(id="6.1:17", cat="legality-rule") }}

An argument to an `inout` parameter **MUST** be an lvalue (a variable, field access, or array index expression).

{{ rule(id="6.1:18", cat="dynamic-semantics") }}

When a function is called with an `inout` argument:
1. The address of the argument is passed to the callee
2. The callee reads and writes to the argument through this address
3. After the call returns, the original variable holds the updated value

{{ rule(id="6.1:19", cat="example") }}

```rue
fn increment(inout x: i32) {
    x = x + 1;
}

fn main() -> i32 {
    let mut n = 10;
    increment(inout n);
    n  // 11
}
```

{{ rule(id="6.1:20", cat="legality-rule") }}

A single function call **MUST NOT** pass the same variable to multiple `inout` parameters. This prevents aliasing of mutable references within a single call.

{{ rule(id="6.1:21", cat="example") }}

```rue
fn swap(inout a: i32, inout b: i32) {
    let tmp = a;
    a = b;
    b = tmp;
}

fn main() -> i32 {
    let mut x = 1;
    swap(inout x, inout x);  // error: cannot pass same variable to multiple inout parameters
    0
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
