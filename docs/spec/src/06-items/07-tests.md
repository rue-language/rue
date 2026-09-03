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
block's value **MUST** have type `()`. The one exception is the `?` operator,
which a test body gives its own meaning (6.7:13).

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

This section specifies the declaration, and the one construct whose meaning a
test body changes (6.7:13). How tests are selected and executed — and the wire
shape a failure report takes once a runner collects it — is defined by ADR-0083
Phase 2 and is not part of this specification; what 6.7:14 pins is the program's
own behavior, which is observable without a runner.

## `?` in a test body

{{ rule(id="6.7:13", cat="legality-rule") }}

The `?` operator **MAY** be applied, in the block of a test declaration, to an
operand whose type is an exact specialization of a trusted producer (4.15:3).
Everything 4.15 says about which operands qualify still holds, so a same-shape
lookalike is still rejected (E0504); what does not hold is 4.15:4's requirement
on the enclosing function's return type, because no value is propagated out of a
test body. Each `?` site is therefore independent: two sites in one test body
**MAY** apply `?` to standard `Result`s with different error types.

This rule governs the test declaration's own block, including any nested block
of an `if`, `while`, or `match` inside it. It does not extend to a function the
test calls: that function has its own body, and `?` in it means what 4.15 says
it means — so a `()`-returning helper still rejects `?` (E0503, E0505).

{{ rule(id="6.7:14", cat="dynamic-semantics") }}

When `?` is evaluated in a test body and the operand is `Some(v)` or `Ok(v)`,
the expression evaluates to `v` and execution continues normally, exactly as
4.15:6 describes.

When the operand is `None` or `Err(e)`, the enclosing function does **not**
return. The implementation reports a structured failure naming the kind
`unhandled_error`, the source position of the `?` operator itself, and the
rendered error value (6.7:15); it then terminates the process the way every
other trap does — exit status 101, with `panic: unhandled error` on the standard
error stream (8.5). No further code in the test body is executed.

{{ rule(id="6.7:15", cat="normative") }}

The rendered error value is produced from the operand's failure payload by these
rules, and is bounded to 4096 bytes; a rendering that would exceed that bound is
truncated to it and the marker ` …[truncated]` is appended.

- A `None` renders as `None`.
- A variant of an enum renders as its variant name when the variant has no
  payload, and as the variant name followed by its rendered payloads in
  declaration order, parenthesized and comma-separated, when it has one:
  `Invalid(-7, bad)`.
- An integer renders in decimal, with a leading `-` when it is negative. A
  `bool` renders as `true` or `false`.
- A byte string — a `str`, a fixed `Str(N)`, or a `StrBuf` — renders as its own
  bytes, verbatim.
- A struct renders as `{ field: value, … }`: each field's name, then its
  rendered value, in declaration order.
- Rendering descends one level. A value reached inside a rendered struct field
  or enum payload that is itself an aggregate renders as the name of its type,
  and so does any value these rules cannot otherwise render.

{{ rule(id="6.7:16", cat="normative") }}

The failing path of 6.7:14 is a trap, not a return. It therefore ends the
process without running the destructors (3.8) of the values live at the `?`
site, exactly as `@panic` and every other trap does. A `drop fn` whose
observable work matters — a flush, or the release of something outside the
process — does not perform it when a `?` in a test body fails.

{{ rule(id="6.7:17", cat="informative") }}

`?` is for the failures a test does not expect. A test that asserts a call
*does* fail matches on the result in the ordinary way (4.14) and never reaches
this rule; nothing about `Option` or `Result` handling changes inside a test
body beyond the meaning of `?` itself.
