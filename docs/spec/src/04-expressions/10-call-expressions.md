+++
title = "Call Expressions"
weight = 10
template = "spec/page.html"
+++

# Call Expressions

{{ rule(id="4.10:1", cat="normative") }}

A call expression invokes a function with a list of arguments.

{{ rule(id="4.10:12", cat="normative") }}

The callee may be either a function name or a callable function alias defined
by a constant item (6.5:12). Calling an alias is equivalent to calling the
function it aliases.

{{ rule(id="4.10:2", cat="syntax") }}

```ebnf
call_expr = expression "(" [ call_arg { "," call_arg } ] ")" ;
call_arg  = [ "inout" | "borrow" ] expression ;
```

{{ rule(id="4.10:10", cat="informative") }}

An `inout` argument must denote a place; that requirement is a post-parse legality rule of the parameter-mode system (6.1:17), not a grammar restriction (see the grammar notes in appendix A). A `borrow` argument that denotes no place is elaborated into one instead of being rejected (6.1:39).

{{ rule(id="4.10:3", cat="legality-rule") }}

The number of arguments **MUST** match the number of parameters in the
function signature. Each explicit argument's source-level passing mode
**MUST** exactly match the corresponding parameter: an `inout` parameter
requires an `inout` argument, a `borrow` parameter requires a `borrow`
argument, and every other parameter, including a `comptime` parameter,
requires an unmarked argument. Arguments to built-in call forms and enum
tuple-variant payloads are likewise unmarked unless a source-level parameter
explicitly declares a mode.

A method receiver is not an explicit argument for this rule. Its passing mode
is selected automatically from the receiver declaration as specified by the
receiver-autoref rule (6.4:25).

{{ rule(id="4.10:4", cat="legality-rule") }}

Each argument's type **MUST** be the corresponding parameter's type after any
argument-position coercion explicitly defined by that type's rules. Those
coercions include the never type (3.4:3), a first-class `str` or string buffer
viewed through a `borrow str` parameter, a caller-owned string buffer viewed
through an `inout str` parameter (3.7:55, 3.7:58, 3.7:60), and the analogous
fixed-array-to-slice coercion (7.2:12, whose operand, element-type, and
element-layout requirements are 7.2:13 and 7.2:14). View
materialization may change the physical calling convention — for example, a
borrowed two-word view is passed by value — but it does not admit an unrelated
source type or change the exact source-level argument-mode rule (4.10:3). No
other type difference is accepted (core calculus
`docs/formal/01-core-calculus.md` §5.8, rule `(Call)`).

{{ rule(id="4.10:5", cat="normative") }}

The type of a call expression is the function's declared return type (core calculus `docs/formal/01-core-calculus.md` §5.8, rule `(Call)`).

{{ rule(id="4.10:9", cat="dynamic-semantics") }}

A call expression evaluates to the value the invocation returns: the callee's body is evaluated with each parameter bound to its corresponding argument — a by-value argument's evaluated value in fresh storage, or, for an `inout`/`borrow` argument, the argument place itself (6.1:18) — and the value the body produces (see 4.5 and 4.9) is the call expression's value (core calculus `docs/formal/01-core-calculus.md` §6.9, rules `(D-Call)`/`(D-Return-Value)`). When the return type is `()`, the call evaluates to `()`.

{{ rule(id="4.10:11", cat="informative") }}

Passing an argument by value is a *use* of it — a move for a non-Copy type, a copy for a Copy type (3.8:11). An `inout`/`borrow` argument is not used; it takes a scoped loan for the call's duration (6.1; core calculus `docs/formal/01-core-calculus.md` §5.4). Those rules are specified in sections 3.8 and 6.1, not here.

{{ rule(id="4.10:6") }}

```rue
fn add(x: i32, y: i32) -> i32 {
    x + y
}

fn main() -> i32 {
    add(40, 2)  // 42
}
```

{{ rule(id="4.10:7", cat="normative") }}

Arguments are evaluated left-to-right before the function is called, as specified in section 4.0 (core calculus `docs/formal/01-core-calculus.md` §6.2).

{{ rule(id="4.10:8") }}

Call expressions can be nested:

```rue
fn add(x: i32, y: i32) -> i32 { x + y }

fn main() -> i32 {
    add(add(10, 20), add(5, 7))  // 42
}
```
