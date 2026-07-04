+++
title = "If Expressions"
weight = 6
template = "spec/page.html"
+++

# If Expressions

{{ rule(id="4.6:1", cat="normative") }}

An if expression conditionally evaluates one of two branches based on a boolean condition.

{{ rule(id="4.6:2", cat="syntax") }}

```ebnf
if_expr     = "if" expression "{" block "}" [ else_clause ] ;
else_clause = "else" ( "{" block "}" | if_expr ) ;
```

{{ rule(id="4.6:3", cat="legality-rule") }}

The condition expression **MUST** have type `bool`.

{{ rule(id="4.6:4", cat="legality-rule") }}

If an `else` branch is present, both branches **MUST** have the same type. (A diverging branch has type `!`, which coerces to the other branch's type — 3.4:3; core calculus `docs/formal/01-core-calculus.md` §5.7, rule `(Sub-Never)`.)

{{ rule(id="4.6:10", cat="normative") }}

The type of an if expression with an `else` branch is the common type of its branches (core calculus `docs/formal/01-core-calculus.md` §5.5, rule `(If)`).

{{ rule(id="4.6:5", cat="legality-rule") }}

If no `else` branch is present, the `then` branch **MUST** have type `()`.

{{ rule(id="4.6:6") }}

```rue
fn main() -> i32 {
    let x = if true { 42 } else { 0 };
    x
}
```

{{ rule(id="4.6:7", cat="dynamic-semantics") }}

The condition is evaluated first. If it evaluates to `true`, the then-branch is evaluated and the if expression evaluates to the then-branch's value; otherwise the else-branch is evaluated and the if expression evaluates to the else-branch's value (core calculus `docs/formal/01-core-calculus.md` §6.6, rules `(D-If-T)`/`(D-If-F)`).

{{ rule(id="4.6:11", cat="dynamic-semantics") }}

An if expression with no `else` branch evaluates to `()`: when the condition is `false` no branch is evaluated and the expression's value is `()`, and when it is `true` the then-branch's value is `()` by rule 4.6:5.

{{ rule(id="4.6:8") }}

```rue
fn main() -> i32 {
    let n = 5;
    if n > 3 { 100 } else { 0 }
}
```

{{ rule(id="4.6:9", cat="example") }}

If expressions can be chained using `else if`:

```rue
fn main() -> i32 {
    let x = 5;
    if x < 3 { 1 }
    else if x < 7 { 2 }
    else { 3 }
}
```

{{ rule(id="4.6:12", cat="informative") }}

The branches of an if expression also reconcile their *ownership* effects: each branch is checked under the same incoming ownership state, and the states are joined after the branch (3.8:50, 3.8:73; core calculus `docs/formal/01-core-calculus.md` §5.5). That interaction is specified in section 3.8, not here.
