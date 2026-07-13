# Tutorial process

The tutorial is a guided path into the current Rue implementation. It should
teach what a user can run today, name rough edges honestly, and avoid becoming
a second language specification.

## Target chapter outline

Use this outline as the sequencing target for the tutorial refresh. Individual
chapter PRs may split or merge files while the content is in motion, but they
should preserve this learning order.

| Order | Chapter | Purpose | Related work |
| --- | --- | --- | --- |
| 1 | Install/build/run | Get a working compiler and explain the repo wrapper commands. | RUE-393 |
| 2 | Hello world and executables | Introduce `main`, exit status, compile/run loop, and first output. | RUE-393, RUE-394 |
| 3 | Values, types, mutability, and output | Establish integer/bool basics, type inference, `let mut`, and user-facing output. | RUE-394 |
| 4 | Functions and expression-oriented control flow | Teach calls, returns, blocks, `if`, `while`, and `match` as expressions where applicable. | RUE-394 |
| 5 | Structs, enums, and pattern matching | Introduce aggregate data and sum types before ownership details. | RUE-396 |
| 6 | Ownership and access modes | Teach moves, `@copy`, `borrow`, `inout`, destructors, and explicit `@drop`. | RUE-396 |
| 7 | Arrays and bounds safety | Cover fixed arrays, index types, mutation, iteration, constant bounds diagnostics, and runtime bounds checks. | RUE-397 |
| 8 | Modules and std imports | Teach file imports, module-qualified access, `@import("std")`, and the no-prelude stance. | RUE-395 |
| 9 | Strings, printing, parsing, and `Option` | Move from debugging output to string-oriented I/O, parsing, and optional results. | RUE-397, RUE-398 |
| 10 | Growable collections / `ArrayBuf` | Introduce the growable collection rung once the implementation is ready enough to dogfood. | RUE-397 |
| 11 | Real final project | Replace synthetic algorithm demos with a small dogfood program such as streaming stats. | RUE-398 |

## Editorial rules

Every chapter should:

1. Start with a runnable complete program that demonstrates the chapter's main
   point.
2. Keep examples self-contained unless the prose explicitly says they are
   fragments.
3. Add one positive example per new concept and, where it materially helps,
   one expected-error example.
4. Prefer stable, user-facing APIs over compiler-internal or debugging-only
   tools. Use `print`/`println` for user-facing output from the first chapter
   that produces any output; example output should flow through `println`.
   `@dbg` may be introduced once as a brief debugging aid, but it is not the
   tutorial's output mechanism.
5. Say when a feature is preview, incomplete, or dogfood-motivated. Do not
   silently teach unstable syntax as if it were settled.
6. Avoid duplicating the specification. Link to the spec for exact normative
   rules after the tutorial has taught the concept operationally.
7. Keep comments truthful under the current compiler. If a diagnostic is
   compile-time today, do not describe it as runtime behavior.

## Snippet verification

Snippet verification is tracked by RUE-399. Once that infrastructure is present,
tutorial code fences use explicit metadata for automated checks:

- ` ```rue check` must compile successfully.
- ` ```rue compile-fail Edddd` must fail compilation with the named diagnostic.
- ` ```rue skip` is an intentional prose-only or context-dependent fragment.
- Plain ` ```rue` remains prose-only until a chapter refresh opts it in.

After RUE-399 lands, run:

```bash
scripts/check-tutorial-snippets.py
./buck2 test //:tutorial-snippet-tests
```

New or rewritten chapter-level complete programs should be marked `check`.
Intentionally-invalid snippets should be marked `compile-fail Edddd` when they
are self-contained, using the diagnostic code the prose intends to demonstrate,
or `skip` when they depend on context from adjacent prose.

## Dogfood and preview stance

The tutorial should aim at the minimum dogfoodable language, but it must not
pretend future library ergonomics already exist.

- Stable behavior can be taught without caveats.
- Preview behavior can appear when it is needed for the dogfood story, but the
  prose must name the preview/in-progress status and the likely migration path.
- Standard-library examples should use the current ADR-0042 model:
  `@import("std")`, namespace-qualified access, and no prelude initially.
- Collection and string examples should follow ADR-0043 terminology:
  fixed arrays, second-class slices, and growable `ArrayBuf` / `StrBuf`-style
  library types. Avoid reviving `String`-as-special-type language in
  new tutorial prose.

## Child issue sequencing

Use this logical order for the RUE-329 child work unless a prerequisite lands
earlier:

1. RUE-400: establish and maintain this outline/style guide.
2. RUE-399: snippet verification infrastructure.
3. RUE-393: current build/run workflow.
4. RUE-394: output model and early chapter cleanup.
5. RUE-396: ownership/access-mode progression.
6. RUE-395: modules and `std` import chapter.
7. RUE-397: arrays, `Option`, and collection path.
8. RUE-398: final dogfood program.

When a child issue materially changes the outline, update this file in the same
PR so the next chapter author is not reconstructing intent from Linear history.
