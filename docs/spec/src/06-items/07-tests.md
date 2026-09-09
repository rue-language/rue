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

## Stability

{{ rule(id="6.7:11", cat="informative") }}

Test declarations are part of the stable language surface. No preview flag is
required to declare one, in any request — an executable build included, since
it parses test declarations for the reference scan of 6.7:10. (The feature was
introduced behind the `test_declarations` preview gate and stabilized by
RUE-1955.)

{{ rule(id="6.7:18", cat="legality-rule") }}

A test declaration **MAY** carry `@known_bug("RUE-NNN")` or
`@known_bug_on("platform", "RUE-NNN")` metadata. These directives are valid
only on tests. The issue marker **MUST** use the canonical positive Rue issue
spelling, and a platform name **MUST** name a supported host platform;
malformed markers are rejected. A matching marker records an ordinary test
failure as an expected failure. A matching marker on a passing test is an
unexpected pass and **MUST** fail the test run. Timeouts, crashes, and runner
failures remain failures even when a test has a matching marker. A test
**MAY** have one unscoped marker or one marker for each distinct platform; an
unscoped marker **MUST NOT** be combined with platform-scoped markers.

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
`unhandled_error`, the source position at which the `?` operator's operand
begins, and the rendered error value (6.7:15); it then terminates the process
the way every other trap does — exit status 101, with `panic: unhandled error`
on the standard error stream (8.5). No further code in the test body is
executed.

The reported position is the operand's first character rather than the `?`
token, so the report names the expression that failed: in
`let mut f = std.fs.File.open(borrow path)?;` it is the column of
`std.fs.File.open`, which is what a reader needs in order to see which call
produced the error.

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
  `bool` renders as `true` or `false`. An `f32` or `f64` renders exactly as
  3.12:40 through 3.12:42 define, through the same formatter `@to_string` uses.
- A byte string — a `str`, a fixed `Str(N)`, or a `StrBuf` — renders as its own
  bytes, verbatim.
- A struct renders as `{ field: value, … }`: each field's name, then its
  rendered value, in declaration order.
- A *standard container* — one of the trusted standard library's collection
  types `ArrayBuf(T)`, `Deque(T)`, `Stack(T)`, `Queue(T)`, `BinaryHeap(T)`,
  `Grid2D(T)`, `StrMap(V)` and `IntMap(V)` — renders by what it holds, never by
  its own fields. A container is recognized by that standard-library identity,
  so a user type of the same shape, or of the same name, is not one.
- A sequence container renders as `[`, then its elements separated by `, `,
  then `]`: `[1, 2, 3]`, and `[]` when it is empty. Each element renders by
  these same rules *as if it were the reported value itself*, because a
  container is transparent to the one-level rule below: a struct element renders
  as `{ x: 1 }` and an enum element as `Some(3)`, and it is that element's own
  fields and payloads which fall to the type-name rule.
  The order is the container's own — front to back for a `Deque(T)` and a
  `Queue(T)`, bottom to top for a `Stack(T)`, and storage order rather than
  sorted order for a `BinaryHeap(T)`. A `Grid2D(T)` is row-major and brackets
  each row: a two-by-three grid renders as `[[1, 0, 0], [0, 0, 9]]`. A byte
  string reached as an element renders double-quoted with `\` and `"` escaped,
  rather than verbatim, so that no element can be mistaken for two.
- A container whose elements these rules cannot render — because an element is a
  value they can only *name*, such as a raw pointer, or because the container is
  nested inside more than four enclosing containers — renders as its type name
  followed by its size instead: `ArrayBuf(Foo) <3 elements>`, and `<1 element>`
  for one.
  `StrMap(V)` and `IntMap(V)` always take that form —
  `StrMap(i64) <2 entries>` — because their live entries are reached through a
  representation this rendering does not read.
- Rendering descends one level, and through a standard container. A value
  reached inside a rendered struct field or enum payload that is itself an
  aggregate renders as the name of its type, and so does any value these rules
  cannot otherwise render; but a standard container reached there renders by its
  elements, and so does a standard container reached as another one's element.

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
