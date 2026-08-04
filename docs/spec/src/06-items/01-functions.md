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

{{ rule(id="6.1:35", cat="legality-rule") }}

The body of a function **MUST NOT** move out of an `inout` parameter. An `inout`
parameter may be read, assigned, mutated through its fields or elements, and
forwarded to another `inout` parameter, but it cannot be returned, stored in
another owned value, or passed to a by-value parameter in a way that consumes it.

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
1. Ordinarily, the address of the argument place is passed to the callee. For a
   field access or array index argument, this is the address of that field or
   element (an out-of-bounds index causes a runtime panic before the call); for
   a place rooted at a by-ref parameter of the caller, it is computed from the
   pointer the caller received. When the parameter type explicitly defines an
   argument-position view coercion (4.10:4), the caller instead materializes
   that view from the place and passes it using the representation specified by
   the type; `borrow str` and slice views are two-word values passed by value.
2. The callee reads the original storage through the received address or
   materialized view.
3. After the call returns, the original variable is unchanged and still valid;
   a borrowed place is not moved out of its owner.

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

{{ rule(id="6.1:36", cat="legality-rule") }}

A single call **MUST NOT** both loan a variable — pass it `inout` or `borrow`, including as a by-reference method receiver — and move that variable's value by passing it, or any part of it, to a by-value parameter of a non-`Copy` type. The loan spans the entire call, so the move would leave the loaned place referring to moved-from storage (its destructor would run both through the moved-into owner and through the loaned alias). The rule applies in either argument order, and to a move that occurs anywhere within the call's argument expressions, including inside a nested call that consumes the variable. As with rules 6.1:20 and 6.1:30, it applies to the argument's root variable: moving one field while loaning another field of the same variable is likewise rejected. A by-value argument of a `Copy` type is a read that completes before the call begins and does not conflict; likewise a loan nested inside another argument (such as `f(inout x, g(borrow x))`) ends before the outer call begins and does not conflict.

{{ rule(id="6.1:37", cat="example") }}

```rue
struct R { id: i32 }
drop fn R(self) { @dbg(self.id); }

fn f(inout a: R, b: R) -> i32 {
    a = R { id: 99 };
    b.id
}

fn main() -> i32 {
    let mut x = R { id: 5 };
    let r = f(inout x, x);  // error: cannot move 'x' into a call that also passes it 'inout'
    r
}
```

## Borrow Operands

{{ rule(id="6.1:39", cat="legality-rule") }}

An argument to a `borrow` parameter **MAY** be any expression of the parameter's
type. When the argument does not denote a place — a variable, or a field or
element projection chain rooted at one — it is a *borrow operand*, and the
implementation elaborates it into a place by exactly one of two mechanisms:
**static promotion** (6.1:40) when the operand meets the promotion criterion,
and a **compiler-materialized temporary** (6.1:41) otherwise. Elaboration
introduces no binding that source code can name, so the storage it produces is
reachable from no other expression and takes part in none of the exclusivity
rules 6.1:20, 6.1:30, and 6.1:36. This rule does **not** extend to `inout`: an
exclusive loan writes through the argument's own storage, so an `inout`
argument **MUST** still be an lvalue (6.1:17). Nor does it extend to a method
receiver, whose passing mode is chosen by autoref (6.4:25) rather than written
as a `borrow` operand.

{{ rule(id="6.1:40", cat="legality-rule") }}

A borrow operand is **promoted** when it is compile-time evaluable and
infallible. It is promoted exactly when both of the following hold:

- it is built only from a literal (integer, boolean, unit, or string), a named
  value constant (6.5), a `comptime` parameter, and the unary operators `-`,
  `!`, `~` and the binary operators `+`, `-`, `*`, `&`, `|`, `^`, `<<`, `>>`,
  `&&`, `||`, `==`, `!=`, `<`, `<=`, `>`, `>=` applied to promoted operands;
  and
- it is a string, or it evaluates to a value at compile time.

Division (`/`) and remainder (`%`) are excluded from the first condition even
when the divisor is a nonzero literal, because their trap conditions depend on
operand values rather than on the shape of the expression. An operand outside
the set, and an operand whose compile-time evaluation is diagnosed (for example
one that overflows its type), is not promoted and is elaborated under 6.1:41
instead.

A promoted operand loans the value's static image, which exists for the whole
program's execution: no destructor runs for it, and no cleanup is scheduled at
any point. A string literal promoted at a `borrow str` parameter is the
static-backed two-word view of 3.7:44; promoted at a `borrow StrBuf` parameter
it is the zero-capacity, literal-backed representation of 3.10:2, which owns no
allocation.

{{ rule(id="6.1:41", cat="dynamic-semantics") }}

A borrow operand that is not promoted is evaluated **exactly once**, in
argument order (4.10:7), and its value is placed in a fresh hidden binding
created for the call; the loan names that binding's storage, so the value is
live for the whole call. The binding's scope is that call — the exact extent
of the loan it backs, since loans are second-class and cannot outlive the call
(6.1:26) — so the binding goes out of scope as soon as the call returns, and
its value is dropped there under the ordinary rules of 3.9. It is dropped
exactly once on every path that reaches it, including a path that leaves the
enclosing statement early through `return`, `break`, `continue`, or `?` after
the binding was initialized, and it is not dropped on a path that leaves before
the operand was evaluated at all.

Because no expression can name the hidden binding, nothing can consume it. An
operand whose type carries a linear value (3.8:57) is therefore rejected, on
the same ground as a discarded expression value (3.8:64).

{{ rule(id="6.1:42", cat="example") }}

```rue
struct Log { id: i32 }
drop fn Log(self) { @dbg(self.id); }

fn make(id: i32) -> Log { Log { id: id } }
fn level(borrow entry: Log) -> i32 { entry.id }

fn threshold(borrow limit: i32) -> i32 { limit }

fn main() -> i32 {
    let promoted = threshold(borrow 40);  // static: no temporary, no drop
    let temporary = level(borrow make(2));  // hidden binding, dropped after the call
    promoted + temporary  // 42
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

The `main` function **MUST** declare no parameters, including `comptime`
parameters, and **MUST** return either `i32` or `()`. Thus `main` is never a
generic function and the runtime can invoke it using the executable entry ABI
without supplying source-level arguments. When it returns `i32`, that value
becomes the program's exit code. When it returns `()`, the exit code is 0.

Programs can also terminate explicitly through the standard library:
`std.exit(code)` terminates the process immediately with the provided `u64`
status code and does not return. This bypasses `main`'s return value; if
`std.exit` is reached, code after that call is not evaluated.

{{ rule(id="6.1:38", cat="legality-rule") }}

The program entry point is the `main` function of the **root module** — the
module designated as the compilation root. A top-level function named `main` in
any other loaded module is an ordinary namespaced function with no entry-point
role: it is reachable only through its module path (for example `m.main()`), and
it neither satisfies nor conflicts with the root module's entry-point
requirement. Consequently the requirement of 6.1:7 is checked against the root
module alone: a root module without `main` is rejected even when an imported
module defines one, and defining `main` in more than one module is not itself an
error (each module is its own namespace, per 10.5:1).

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
