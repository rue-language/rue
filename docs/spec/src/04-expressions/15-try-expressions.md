+++
title = "Try Expressions"
weight = 15
template = "spec/page.html"
+++

# Try Expressions

{{ rule(id="4.15:1", cat="normative") }}

A try expression applies the postfix `?` operator to an operand of an `Option`
type. It evaluates to the payload of the `Some` variant, and short-circuits the
enclosing function by returning `None` when the operand is the `None` variant
(the propagation form of error handling, see ADR-0038).

{{ rule(id="4.15:2", cat="syntax") }}

```ebnf
try_expr = expression "?" ;
```

The `?` operator is postfix and binds as tightly as the other postfix operators
(field access, indexing, and calls).

{{ rule(id="4.15:3", cat="legality-rule") }}

The operand of `?` **MUST** have a type that is an exact specialization of a
*trusted producer*: the standard `Option` declared by `std.option.Option`
(`std/option.rue`) or the standard `Result` declared by `std.result.Result`
(`std/result.rue`) (ADR-0038). The trusted producers are exactly these two
standard declarations. A user-defined enum that repeats the `Some`/`None` or
`Ok`/`Err` shape under a different producer is an ordinary enum (§4.14), not a
trusted producer, and receives no `?` behavior. Applying `?` to such a
lookalike, or to any other type, is a compile error (E0504).

{{ rule(id="4.15:4", cat="legality-rule") }}

A try expression **MUST** appear in the body of a function whose declared return
type is an exact specialization of the **same** trusted producer as the operand
(rule 4.15:3). When the operand is a standard `Option(T)`, the enclosing function
**MUST** return a standard `Option(U)`; the success payload may differ (`U` need
not equal `T`), but using `?` in a function that does not return a standard
`Option` is a compile error (E0503). When the operand is a standard
`Result(T, E)`, the enclosing function **MUST** return a standard `Result(U, E)`
whose error type `E` is **identical** to the operand's — Rue has no error
conversion until traits exist. Using `?` on a `Result` in a function that does
not return a standard `Result` is a compile error (E0505), and a mismatched
error type is a compile error (E0506). In every case the failure arm constructs
the `None` or `Err(e)` of the enclosing function's own return type (rule 4.15:7).

{{ rule(id="4.15:5", cat="normative") }}

The type of a try expression `operand?`, where `operand` has type `Option(T)`,
is `T`.

{{ rule(id="4.15:6", cat="dynamic-semantics") }}

When a try expression is evaluated and the operand is `Some(v)`, the expression
evaluates to `v` and execution continues normally.

{{ rule(id="4.15:7", cat="dynamic-semantics") }}

When a try expression is evaluated and the operand is `None`, the enclosing
function immediately returns `None`. No further code in the function is executed.
This is equivalent to the desugaring:

```rue
match operand {
    Some(v) => v,
    None => return None,
}
```

where the returned `None` is the `None` variant of the enclosing function's
`Option` return type.

{{ rule(id="4.15:8") }}

```rue
const std = @import("std");

fn checked_div(a: i64, b: i64) -> std.option.Option(i64) {
    let O = std.option.Option(i64);
    if b == 0 { O.None } else { O.Some(a / b) }
}

fn halve_then_div(a: i64, b: i64) -> std.option.Option(i64) {
    let O = std.option.Option(i64);
    // `?` unwraps the quotient, or short-circuits to None when b == 0.
    let q = checked_div(a, b)?;
    O.Some(q / 2)
}

fn main() -> i32 {
    let O = std.option.Option(i64);
    match halve_then_div(40, 4) {
        O.Some(n) => @intCast(n),   // 40 / 4 / 2 == 5
        O.None => 0 - 1,
    }
}
```

{{ rule(id="4.15:9", cat="normative") }}

When the operand of `?` is a bare call to a fallible intrinsic — `@read_line`
or one of the `@parse_*` intrinsics (§4.13) — no special resolution applies: the
operand already has the trusted standard `Option` type that the intrinsic always
produces (rules 4.13:35, 4.13:44). Its fixed payload (`@read_line` → `StrBuf`,
`@parse_i64` → `i64`, and so on) is unwrapped by `?`, which short-circuits the
enclosing standard-`Option`-returning function with the standard `None` on
failure. This is what lets `@read_line()?` and `@parse_i64(s)?` be written
directly, without first binding the result to an annotated `let` (RUE-318). The
enclosing function still needs no lexical `std` import for the intrinsic's type
to be the standard `Option` (rule 4.13:35).

{{ rule(id="4.15:10") }}

```rue
const std = @import("std");

// `@read_line()?` short-circuits to None at end-of-input; the tail
// `@parse_i64(line)` is None on a line that is not a number. Both intrinsics
// already have the trusted standard `Option`, so no local `Option` is declared.
fn read_num() -> std.option.Option(i64) {
    let line = @read_line()?;
    @parse_i64(line)
}

fn main() -> i32 {
    let O = std.option.Option(i64);
    match read_num() {
        O.Some(n) => @intCast(n),
        O.None => 0,
    }
}
```
