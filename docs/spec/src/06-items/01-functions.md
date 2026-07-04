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

{{ rule(id="6.1:34", cat="legality-rule") }}

The parameters in a single parameter list **MUST** have distinct names. It is a compile-time error for a function or method to declare two parameters with the same name (for example, `fn f(x: i32, x: i32)`); the diagnostic identifies the second occurrence. A method's `self` receiver is not a named parameter for the purpose of this rule.

{{ rule(id="6.1:4", cat="dynamic-semantics") }}

A function call evaluates to the value the function's body block evaluates to (see 4.5). Reaching the end of the body is not a distinct "implicit return": the body is an expression, and the call's value *is* that expression's value.

{{ rule(id="6.1:5", cat="normative") }}

If a return type is specified, the body block **MUST** evaluate to a value of that type. If no return type is specified, the return type is `()`, and the body block evaluates to `()`.

{{ rule(id="6.1:6", cat="normative") }}

```rue
fn add(x: i32, y: i32) -> i32 {
    x + y   // the body block evaluates to x + y, which is returned
}

fn do_nothing() {
    // the body block has no final expression, so it evaluates to ()
}
```

## Inout Parameters

{{ rule(id="6.1:14", cat="normative") }}

A parameter **MAY** be marked with the `inout` keyword to indicate that it is passed by reference and may be mutated by the callee. Changes made to an `inout` parameter are visible to the caller after the call returns.

{{ rule(id="6.1:15", cat="syntax") }}

```ebnf
param = [ param_mode ] IDENT ":" type ;
param_mode = "inout" | "borrow" ;
```

{{ rule(id="6.1:16", cat="legality-rule") }}

At the call site, an argument passed to an `inout` parameter **MUST** be marked with the `inout` keyword.

{{ rule(id="6.1:17", cat="legality-rule") }}

An argument to an `inout` parameter **MUST** be an lvalue (a variable, field access, or array index expression).

{{ rule(id="6.1:18", cat="dynamic-semantics") }}

When a function is called with an `inout` argument:
1. The address of the argument place is passed to the callee. For a field access or array index argument, this is the address of that field or element (an out-of-bounds index causes a runtime panic before the call); for a place rooted at a by-ref parameter of the caller, it is computed from the pointer the caller received
2. The callee reads and writes to the argument through this address
3. After the call returns, the original place holds the updated value, visible to the caller

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

A single function call **MUST NOT** pass the same variable to multiple `inout` parameters. This prevents aliasing of mutable references within a single call. The rule applies to the argument's root variable: two arguments that project different fields or elements of the same variable (such as `o.a` and `o.b`) are likewise rejected.

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

## Borrow Parameters

{{ rule(id="6.1:22", cat="normative") }}

A parameter **MAY** be marked with the `borrow` keyword to indicate that it is passed by reference for read-only access. The callee cannot mutate the borrowed value, and the value is not consumed (ownership is not transferred).

{{ rule(id="6.1:23", cat="legality-rule") }}

At the call site, an argument passed to a `borrow` parameter **MUST** be marked with the `borrow` keyword.

{{ rule(id="6.1:24", cat="legality-rule") }}

The body of a function **MUST NOT** mutate a `borrow` parameter. This includes:
- Assignment to the parameter itself
- Assignment to fields of the parameter
- Assignment to array elements of the parameter
- Passing the parameter, or any field or element of it, as an `inout` argument

{{ rule(id="6.1:25", cat="legality-rule") }}

The body of a function **MUST NOT** move out of a `borrow` parameter. A borrowed value cannot be returned, stored in a struct field, or passed to a function expecting an owned value.

{{ rule(id="6.1:26", cat="dynamic-semantics") }}

When a function is called with a `borrow` argument:
1. The address of the argument place is passed to the callee. For a field access or array index argument, this is the address of that field or element (an out-of-bounds index causes a runtime panic before the call); for a place rooted at a by-ref parameter of the caller, it is computed from the pointer the caller received
2. The callee reads from the argument through this address
3. After the call returns, the original variable is unchanged and still valid; a borrowed place is not moved out of its owner

{{ rule(id="6.1:27", cat="example") }}

```rue
struct Point { x: i32, y: i32 }

fn sum_coords(borrow p: Point) -> i32 {
    p.x + p.y
}

fn main() -> i32 {
    let p = Point { x: 10, y: 32 };
    let result = sum_coords(borrow p);
    result + p.x - p.x  // p is still valid after the borrow
}
```

{{ rule(id="6.1:28", cat="normative") }}

Multiple `borrow` parameters **MAY** refer to the same variable. Unlike `inout`, borrows are shared read-only access.

{{ rule(id="6.1:29", cat="example") }}

```rue
fn sum_both(borrow a: i32, borrow b: i32) -> i32 {
    a + b
}

fn main() -> i32 {
    let x = 21;
    sum_both(borrow x, borrow x)  // OK: multiple borrows of same variable
}
```

{{ rule(id="6.1:30", cat="legality-rule") }}

A single function call **MUST NOT** pass the same variable to both a `borrow` parameter and an `inout` parameter. This enforces the law of exclusivity: either one `inout` or any number of `borrow` accesses, but never both simultaneously. As with rule 6.1:20, the rule applies to the argument's root variable: a `borrow` of one field and an `inout` of another field of the same variable are likewise rejected.

{{ rule(id="6.1:31", cat="example") }}

```rue
fn mixed(borrow a: i32, inout b: i32) {
    b = a + 1;
}

fn main() -> i32 {
    let mut x = 41;
    mixed(borrow x, inout x);  // error: cannot borrow and inout same variable
    0
}
```

## Parameter Immutability

{{ rule(id="6.1:32", cat="legality-rule") }}

A parameter that is not marked `inout` is immutable within the function body. Assigning to such a parameter or modifying its fields is a compile-time error.

{{ rule(id="6.1:33", cat="example") }}

```rue
fn bad(x: i32) {
    x = 5;  // error: cannot assign to immutable parameter 'x'
}

struct Point { x: i32, y: i32 }

fn also_bad(p: Point) {
    p.x = 10;  // error: cannot assign to immutable parameter 'p'
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
