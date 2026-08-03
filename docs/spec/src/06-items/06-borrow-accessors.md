+++
title = "Borrow Accessors"
weight = 6
template = "spec/page.html"
+++

# Borrow Accessors

{{ preview_feature(feature="borrow_accessors", adr="ADR-0062") }}

{{ rule(id="6.6:1", cat="informative") }}

A *borrow accessor* is a method that hands out a second-class borrow of a
projection of its receiver: `v.get_ref(i)` produces a borrowed place naming
element `i` in place — no copy, no move-out — checked by the ordinary
law-of-exclusivity loan machinery and scoped to the enclosing full expression
(core calculus `docs/formal/01-core-calculus.md` §5.8, rule `(Accessor-Call)`).
This is the ADR-0062 read-accessor form; mutable accessors (`inout self` →
exclusive result) are a later phase.

## Declaration

{{ rule(id="6.6:2", cat="syntax") }}

```ebnf
accessor    = "fn" IDENT "(" "borrow" "self" [ "," params ] ")"
              "->" "borrow" type "{" { statement } yield_expr [ ";" ] "}" ;
yield_expr  = "yield" expression ;
```

{{ rule(id="6.6:3", cat="legality-rule") }}

A `-> borrow` result position and the `yield` form require the
`borrow_accessors` preview feature. Without `--preview borrow_accessors`, a
program using either is rejected at compile time (E1100), per 8.4.

{{ rule(id="6.6:4", cat="legality-rule") }}

An accessor **MUST** declare a `borrow self` receiver. A `-> borrow` result on
a free function, an associated function, a by-value or `mut self` method, an
`inout self` method, or a method of an anonymous struct type is rejected
(E0257).

{{ rule(id="6.6:5", cat="legality-rule") }}

Accessor value parameters **MUST** be plain by-value parameters: `borrow`,
`inout`, and `comptime` parameter modes are rejected on an accessor (E0260).

## The accessor body

{{ rule(id="6.6:6", cat="legality-rule") }}

Every non-diverging path through an accessor body **MUST** fall through to the
body's single trailing `yield`: the final statement of the body is a `yield`,
no other `yield` may appear, no code may follow it, and the body **MUST NOT**
contain `return` or `?` (E0254). Guard code before the `yield` may only
diverge — trap or `@panic` — or fall through. A `yield` outside an accessor
body is rejected (E0256).

{{ rule(id="6.6:7", cat="legality-rule") }}

The operand of the `yield` **MUST** be a place rooted at the receiver
parameter: `self`, or a projection chain from `self` through fields, indices,
or nested accessor calls (E0255). Yielding a local, a parameter other than the
receiver, or a computed value would hand out a place that dies with the
accessor's guards.

## Calls

{{ rule(id="6.6:8", cat="normative") }}

A call to an accessor requires its receiver to be a place, exactly as passing
it as a `borrow` argument does (6.4:27), and evaluates its arguments by value.
The result is a *borrowed place*, not a first-class value: a shared loan on
the receiver's root variable whose extent is the enclosing full expression
(core calculus `docs/formal/01-core-calculus.md` §5.8, rule
`(Accessor-Call)`). Within that extent the result may be read, projected
further (`v.get_ref(i).name`), passed as a `borrow` argument, or compared.

{{ rule(id="6.6:9", cat="legality-rule") }}

An accessor result **MUST NOT** escape its full expression: returning it
(E0250), storing it by assignment (E0251), binding it with a plain `let`
(E0252), or capturing it in a struct or array literal (E0253) is rejected.

{{ rule(id="6.6:10", cat="legality-rule") }}

The law of exclusivity extends over the accessor loan's whole extent: an
exclusive use of the borrowed root — passing it `inout`, an `inout self`
receiver access, assigning to it, or moving it — anywhere within the same full
expression is rejected (E0259). `use(v.get_ref(i), g(inout v))` is ill-formed
even though the read syntactically precedes the exclusive access.

{{ rule(id="6.6:11", cat="legality-rule") }}

Reading a value that owns resources (one with drop glue) out of an accessor
result by value is rejected (E0258): the result is a borrow, not an owner, and
a by-value read would mint an aliasing second owner — the same soundness
argument as the by-copy container-read gate (E0711). Only trivially droppable
values may be read out; owning values are used in place through projection,
`borrow` arguments, comparison, or `borrow self` methods.

## Dynamics and lowering

{{ rule(id="6.6:12", cat="dynamic-semantics") }}

An accessor call evaluates by the accessor's inlined body: the guards run in
the calling context — and may trap — and the call's result is then the
yielded place itself, projected from the caller's receiver place (core
calculus §5.8 dynamics note). No function call occurs at runtime.

{{ rule(id="6.6:13", cat="informative") }}

Accessors are required-inlineable by design: no calling convention for
"returning a place" exists, which is the forward-compatibility contract that
keeps the future coroutine-accessor generalization (RUE-1012) free to choose
its own call shape.

{{ rule(id="6.6:14", cat="legality-rule") }}

Accessor expansion **MUST** be acyclic: an accessor body **MUST NOT** call an
accessor whose expansion encloses it, whether directly (`fn xr(borrow self) ->
borrow i64 { yield self.xr(); }`) or through a chain of other accessors
(E0261). Because the call is the inlined body (6.6:12), a cycle has no finite
expansion. The rejection names the recursive call.
