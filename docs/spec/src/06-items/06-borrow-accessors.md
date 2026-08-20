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
The same preview also supports mutable accessors: `v.get_mut(i)` produces an
exclusive place, and uses `inout self` with an `-> inout T` result.

## Declaration

{{ rule(id="6.6:2", cat="syntax") }}

```ebnf
accessor    = "fn" IDENT "(" accessor_self [ "," params ] ")"
              "->" accessor_result type "{" { statement } yield_expr [ ";" ] "}" ;
accessor_self   = "borrow" "self" | "inout" "self" ;
accessor_result = "borrow" | "inout" ;
yield_expr  = "yield" expression ;
```

{{ rule(id="6.6:3", cat="legality-rule") }}

A `-> borrow` or `-> inout` result position and the `yield` form require the
`borrow_accessors` preview feature. Without `--preview borrow_accessors`, a
program using either is rejected at compile time (E1100), per 8.4.

{{ rule(id="6.6:4", cat="legality-rule") }}

An accessor **MUST** pair its result and receiver modes exactly: `borrow self`
with `-> borrow T`, or `inout self` with `-> inout T`. A result on a free
function, an associated function, a by-value or `mut self` method, or a method
of an anonymous struct type is rejected (E0257); a mismatched pair reports the
required pairing.

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
it as a `borrow` or `inout` argument does (6.4:27), and evaluates its
arguments by value. A `-> borrow` result is a *borrowed place*, not a
first-class value: a shared loan on the receiver's root variable. A `-> inout`
result is an exclusive place and requires the receiver to be mutable, with
the same addressability and mutability rules as an `inout` argument. Both
loans extend through the enclosing full expression (core calculus
`docs/formal/01-core-calculus.md` §5.8, rule `(Accessor-Call)`).

{{ rule(id="6.6:9", cat="legality-rule") }}

An accessor result **MUST NOT** escape its full expression: returning it
(E0250), storing it by assignment (E0251), binding it with a plain `let`
(E0252), or capturing it in a struct or array literal (E0253) is rejected.

{{ rule(id="6.6:10", cat="legality-rule") }}

The law of exclusivity extends over the accessor loan's whole extent: shared
accessor results may coexist with one another, while an exclusive accessor
result conflicts with every shared or exclusive access to the same root, and
an exclusive use conflicts with every active accessor loan. An
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
expansion — a property of the declarations alone, so a cycle whose every link
is a call on the accessor's own `self` receiver is rejected at the
declaration, whether or not anything calls an accessor in it, like the other
legality rules of this chapter. A re-entrant call reached through any other
receiver (a by-value guard of the owner's own type) is rejected when a call
site demands the body's analysis and expansion. The rejection names the
recursive accessor either way.

{{ rule(id="6.6:15", cat="normative") }}

An exclusive accessor result is expression-scoped and may be used as a place:
`v.get_mut(i) = value`, projected assignment such as
`v.get_mut(i).field = value`, and `set(inout v.get_mut(i))` are valid when the
receiver is mutable. The right-hand side is evaluated first; when the yielded
destination owns a droppable value, its old value is dropped before the new
value is stored. Ordinary linear overwrite checks apply to the yielded place;
an overwrite of a live linear value is rejected unless reinitialization is
provable.

{{ rule(id="6.6:16", cat="legality-rule") }}

An accessor result **MUST NOT** escape its expression-scoped loan. It cannot be
returned, captured in an aggregate, or assigned as a value. A mutable accessor
cannot be called through an immutable or shared-borrowed receiver, and two
mutable results (or a mutable result and a shared result) rooted at the same
receiver are rejected in either evaluation order.

{{ rule(id="6.6:17", cat="informative") }}

Accessor exclusivity is root-granular in this phase. Path-granular disjointness,
coroutine accessor bodies, and `Option(inout T)` results are outside scope.
