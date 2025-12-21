# Functions

r[6.1.1#normative]
A function is defined using the `fn` keyword.

r[6.1.2#normative]
```ebnf
function = "fn" IDENT "(" [ params ] ")" [ "->" type ] "{" block "}" ;
params = param { "," param } ;
param = IDENT ":" type ;
```

## Function Signature

r[6.1.3#normative]
Parameters must have explicit type annotations.

r[6.1.4#normative]
If a return type is specified, the function body must produce a value of that type.

r[6.1.5#normative]
If no return type is specified, the function returns `()`.

r[6.1.6#normative]
```rue
fn add(x: i32, y: i32) -> i32 {
    x + y
}

fn do_nothing() {
    // implicitly returns ()
}
```

## Entry Point

r[6.1.7#normative]
A program must have a function named `main`.

r[6.1.8#normative]
The `main` function must either return `i32` or `()`. When it returns `i32`, that value becomes the program's exit code. When it returns `()`, the exit code is 0.

r[6.1.9]
```rue
fn main() -> i32 {
    42  // exit code is 42
}
```

## Recursion

r[6.1.10#normative]
Functions can call themselves recursively.

r[6.1.11]
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

r[6.1.12#normative]
Functions can call any function defined in the same module, regardless of definition order.

r[6.1.13]
```rue
fn main() -> i32 {
    helper()  // can call function defined below
}

fn helper() -> i32 {
    42
}
```
