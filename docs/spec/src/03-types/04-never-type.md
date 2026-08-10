+++
title = "Never Type"
weight = 4
template = "spec/page.html"
+++

# Never Type

{{ rule(id="3.4:1", cat="normative") }}

The never type, written `!`, is the type of an expression that transfers control away from its context rather than producing a value — a diverging expression (`docs/formal/01-core-calculus.md` §5.7).

{{ rule(id="3.4:2", cat="normative") }}

Expressions of type `!` include:
- `return` expressions
- `break` expressions
- `continue` expressions
- Infinite loops
- `@panic(msg?)`, which aborts the process and never returns (4.13:5b)

## Type Coercion

{{ rule(id="3.4:3", cat="normative") }}

A type coercion is an implicit type conversion applied during type checking. Rue has exactly one coercion: the never type coerces to any type. The core states this as subsumption on the bottom type — the (Sub-Never) rule of `docs/formal/01-core-calculus.md` §5.7 — while every other typing rule demands exact type identity.

{{ rule(id="3.4:4", cat="normative") }}

When type checking requires a value of type `T`, an expression of type `!` is accepted at `T`. The conversion is vacuously sound: `!` has no values (3.4:9), so re-typing a diverging expression at `T` converts no run-time value and creates no ownership obligation (`docs/formal/01-core-calculus.md` §5.7). This lets diverging expressions appear in any context where a value is expected.

{{ rule(id="3.4:5") }}

```rue
fn test(x: i32) -> i32 {
    // `return 100` has type !, which coerces to i32
    let y = if x > 5 { return 100 } else { x };
    y * 2
}

fn main() -> i32 {
    test(3) + test(10)  // 6 + 100 = 106
}
```

{{ rule(id="3.4:6", cat="normative") }}

When both branches of an `if` expression or all arms of a `match` expression have type `!`, the entire expression has type `!`.

{{ rule(id="3.4:7") }}

```rue
fn diverges(x: i32) -> i32 {
    // Both branches return, so the if has type !
    // This coerces to i32 (the function's return type)
    if x > 0 { return 1 } else { return 0 }
}

fn main() -> i32 { diverges(5) }
```

{{ rule(id="3.4:6a", cat="normative") }}

`!` propagates through any composite expression that control cannot fall out
of, not only `if` and `match`. A block expression has type `!` when control
cannot reach its end: either its tail expression has type `!`, or a statement
within it diverges — for example an expression statement whose expression has
type `!` — which makes the remainder of the block, and thus the block's end,
unreachable. A `loop` expression containing no `break` targeting it likewise
has type `!` (3.4:2) — the same purely syntactic classification as 4.8:21,
which is not a reachability question: a `loop` containing a `break` has type
`()` even when that `break` can never execute (formal core §5.7, (Loop-Div) /
(Loop-Break)). By the coercion of 3.4:3, such an expression may appear
wherever a value of any type is expected.

{{ rule(id="3.4:6b") }}

```rue
fn test(x: i32) -> i32 {
    // The block's tail expression is `return`, so the block has type !,
    // which coerces to the i32 the `let` expects.
    let y = { return 100 };
    y * 2
}

fn main() -> i32 { test(3) }  // exit code 100
```

## Diverging Functions

{{ rule(id="3.4:8", cat="normative") }}

A function with return type `!` never returns normally.

## Memory Representation

{{ rule(id="3.4:9", cat="normative") }}

The never type is a zero-sized type. See [Zero-Sized Types](../#zero-sized-types) for the general definition.
