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

The operand of `?` **MUST** have an `Option` type: an enum with exactly two
variants, a single-payload `Some(T)` and a payload-less `None`. Applying `?` to
any other type is a compile error (E0504).

{{ rule(id="4.15:4", cat="legality-rule") }}

A try expression **MUST** appear in the body of a function whose declared return
type is an `Option`. Using `?` in a function that does not return an `Option` is
a compile error (E0503).

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
fn Option(comptime T: type) -> type {
    enum { Some(T), None }
}

fn checked_div(a: i64, b: i64) -> Option(i64) {
    let O = Option(i64);
    if b == 0 { O::None } else { O::Some(a / b) }
}

fn halve_then_div(a: i64, b: i64) -> Option(i64) {
    let O = Option(i64);
    // `?` unwraps the quotient, or short-circuits to None when b == 0.
    let q = checked_div(a, b)?;
    O::Some(q / 2)
}

fn main() -> i32 {
    let O = Option(i64);
    match halve_then_div(40, 4) {
        O::Some(n) => n,      // 40 / 4 / 2 == 5
        O::None => 0 - 1,
    }
}
```
