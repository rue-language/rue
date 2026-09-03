+++
title = "Tests"
weight = 7
template = "spec/page.html"
+++

# Tests

{{ rule(id="6.7:1", cat="informative") }}

A *test declaration* is an item that names a block of code exercising the
module it sits in: `test "parse_port accepts the loopback default" { … }`. The
string is the test's name, not a value; the block is an ordinary `()`-typed
body. Test declarations are not part of an executable program — nothing an
executable request compiles, links, or runs can reach one — so they are a
place to keep checks beside the code they check without paying for them in the
shipped program.

## Syntax

{{ rule(id="6.7:2", cat="syntax") }}

<!-- grammar-sync(id="6.7:2", production="item", role="source", relation="contains", symbol="test_item") -->
<!-- grammar-sync(id="6.7:2", production="test_item", role="source") -->

```ebnf
test_item = directives "test" STRING "{" block "}" ;
```

{{ rule(id="6.7:3", cat="legality-rule") }}

`test` is a *contextual keyword*. It introduces a test declaration only at item
position and only when the token immediately following it is a `STRING`.
Everywhere else — including as a function name, a parameter name, a `const`
name, a field name, a method name, and a local binding — `test` remains an
ordinary identifier and **MUST** keep its ordinary meaning.

{{ rule(id="6.7:4", cat="example") }}

```rue
fn test(x: i32) -> i32 { x }        // `test` is an ordinary function name

fn main() -> i32 {
    let test = test(0);             // and an ordinary local binding
    test
}
```

{{ rule(id="6.7:5", cat="legality-rule") }}

A test declaration takes no visibility modifier and no `unchecked` modifier: it
is not callable, so neither has a meaning for it. `pub test "…" { … }` and
`unchecked test "…" { … }` are rejected. It **MAY** carry the same directives a
function may carry.

## Name uniqueness

{{ rule(id="6.7:6", cat="legality-rule") }}

The names of the test declarations in one module **MUST** be pairwise distinct.
A second test declaration with a name already declared in that module is
rejected (E0262). Test names live in their own namespace: a test **MAY** share
its spelling with a function, type, or constant in the same module, and
distinct modules **MAY** each declare a test with the same name.

## The test body

{{ rule(id="6.7:7", cat="normative") }}

A test declaration's block is analyzed exactly as the body of a parameterless
function whose result type is `()` (6.1). Every rule that governs such a body
governs a test body, unchanged — type checking, ownership and borrow checking,
linearity, and every legality rule of chapters 3 and 4. In particular, the
block's value **MUST** have type `()`, and the `?` operator is rejected in a
test body exactly as it is in any other `()`-returning body (E0503, E0505).

{{ rule(id="6.7:8", cat="normative") }}

A test declaration sees exactly what its module's other items see. It resolves
names under the ordinary rules of chapter 10 and is subject to the same
visibility boundary (10.3), so a test in a module may use that module's private
items and a test in another directory may use only the public API. Placement is
therefore the whole visibility model for tests: no item needs `pub` in order to
be tested.

## Rooting

{{ rule(id="6.7:9", cat="normative") }}

Test declarations are roots, not reachable code. An executable program's
closure is rooted at its entry point `main` (6.1) together with its `extern "C"`
exports (9.3), and a test declaration is in neither: an executable request
**MUST NOT** analyze, lower, code-generate, or link a test body, and the
presence of a test declaration **MUST NOT** change an executable program's
behavior, its generated code, or its linked image. A test body containing an
error that would be rejected in a function therefore does not reject the
program it sits in when that program is built as an executable.

{{ rule(id="6.7:10", cat="normative") }}

A test body nevertheless participates in the whole-program reference scan that
filters unused-item warnings, so an item used only by a test is not reported as
unused in an executable build.

## Preview gate

{{ rule(id="6.7:11", cat="informative") }}

Test declarations require the `test_declarations` preview feature (8.4:1). The
gate covers a grammar change, so *any* request whose module closure contains a
test declaration needs the flag — an executable build included, since it parses
test declarations for the reference scan of 6.7:10. Declaring a test without
the feature enabled is the standard preview-gate diagnostic (E1100).

{{ rule(id="6.7:12", cat="informative") }}

This section specifies the declaration only. How tests are selected, executed,
and reported is defined by ADR-0083 Phase 2 and is not yet part of this
specification.
