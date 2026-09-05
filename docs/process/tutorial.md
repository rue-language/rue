# Tutorial process

The tutorial (`website/content/tutorial/`) is a guided path into the current
Rue implementation. It teaches what a user can run today, names rough edges
honestly, and does not try to be a second language specification.

## Chapter outline

The tutorial is organized so that each concept arrives when a program needs
it, and it ends with one real program built across two chapters. Keep this
learning order when editing; a chapter may be split or merged, but a concept
should not be used before the chapter that teaches it.

| Order | Chapter | Teaches |
| --- | --- | --- |
| 1 | Why Rue | The design principles: locality of reasoning, ownership without lifetimes, errors as values and bugs as traps, explicit over clever. Says which are guarantees today. |
| 2 | Getting Started | Build from source, `scripts/rue exec`, direct compiler use with `RUE_STD_PATH`, `rue test`. |
| 3 | Hello, World | `main`, exit status, `println`/`print`, comments, `@dbg` as an aside. |
| 4 | Values and Types | Integers, `@intCast`, overflow traps, booleans, inference, `let mut`, floats, the `str`/`StrBuf` output idiom and why `std` is imported. |
| 5 | Functions and Control Flow | `fn`, expression bodies, `return`, unit, `if`, `while`, `loop`, `for`, `match`, blocks. |
| 6 | Structs and Methods | Struct literals, field access, methods with `borrow self`/`inout self`, `Self`, associated functions. |
| 7 | Enums and Matching | Variants, exhaustiveness, payloads, `Option`, `?` on `Option`. |
| 8 | Ownership and Access Modes | Moves, `@copy`, `borrow`, `inout`, exclusivity, destructors, `@drop`. |
| 9 | Arrays, Slices, and Buffers | `[T; N]`, `for`, bounds checks, `[T]` slices, `ArrayBuf`. |
| 10 | Strings and Text | `str` vs `StrBuf`, concatenation, byte literals, bytes vs `chars()`, `@read_line`, `@parse_i64`, `std.strings`. |
| 11 | Errors and Traps | `Result`, `?` on `Result`, the trap list, choosing between them. |
| 12 | Modules and the Standard Library | `@import`, `pub` and the directory visibility rule, `std` as a module, generics as type-returning functions. |
| 13 | Tests | `test` declarations, `@assert`/`@assert_eq`, `?` in tests, `rue test`. |
| 14 | Project: A Calculator | A reverse-Polish calculator in one file, walked through. |
| 15 | Project: Modules and Tests | The calculator split into a module, with tests in both files. |
| 16 | What's Next | What Rue does not have yet, rough edges, where to go. |

When the language grows, the usual move is to extend the chapter that owns
the concept, then update this table. A new feature big enough to need its own
chapter goes after chapter 13 and before the project, and the project should
use it if it can.

## Editorial rules

Every chapter should:

1. Start with a runnable complete program that demonstrates the chapter's main
   point, and show its output.
2. Keep examples self-contained unless the prose explicitly says they are
   fragments.
3. Add one positive example per new concept and, where it materially helps,
   one expected-error example with the diagnostic the reader will see.
4. Use `print`/`println` for user-facing output. `@dbg` is introduced once as
   a debugging aid and is not the tutorial's output mechanism.
5. Say when a feature is preview, incomplete, or a current limitation (for
   example, slices of narrow element types, `for` over `ArrayBuf`). Do not
   silently teach unstable syntax as if it were settled.
6. Avoid duplicating the specification. Link to the spec for exact normative
   rules after the tutorial has taught the concept operationally.
7. Keep prose truthful under the current compiler. If a diagnostic is
   compile-time today, do not describe it as runtime behavior, and do not
   describe a rule the compiler does not enforce as if it did.
8. Introduce `RUE_STD_PATH` wherever a direct compiler invocation is shown.
   `scripts/rue exec` sets it; a bare `"$RUE" file.rue` does not, and a reader
   who follows chapter 2's direct-invocation instructions must not fail in
   chapter 4.

The "Why Rue" chapter states design principles. Some are normative today (the
access model, trapping arithmetic, no prelude); others are recorded as
principles under discussion in Linear and the ADRs. Keep the chapter's "Where
things stand" section honest about which is which, and do not present an
undecided direction as a feature.

## Snippet verification

Every ```` ```rue ```` fence in the tutorial carries a marker in its info
string, and `scripts/check-tutorial-snippets.py` (Buck target
`//:tutorial-snippet-tests`, in the repository quality gates) verifies it:

- ```` ```rue run ```` compiles the program, runs it with empty stdin, requires
  exit status 0, and compares its stdout with the next ```` ```text ```` fence.
  Shell fences (```` ```bash ````) between the program and its output are
  skipped, since they only show the reader how to run it. Use
  `stdin="line\n..."` to feed input and `exit=N` when a nonzero status is the
  point (a trap exits 101).
- ```` ```rue check ```` compiles only. Use it for programs whose interesting
  behavior is under `rue test` rather than `main`.
- ```` ```rue compile-fail Edddd ```` must fail with the named diagnostic
  code(s). The prose should show the diagnostic text the reader will see.
- ```` ```rue file=dir/name.rue ```` is written next to the chapter's later
  snippets instead of being compiled itself, so a multi-file example can show
  each file once. Files accumulate within one chapter and reset between
  chapters.
- ```` ```rue skip ```` is not verified. Use it for fragments, and let the
  prose say that the code is a fragment.

An unmarked ```` ```rue ```` fence is an error. Prefer `run` over `check`
wherever the program prints something, so that "prints:" claims are tested.

Run the checker and its own tests with:

```bash
scripts/check-tutorial-snippets.py
python3 scripts/test-tutorial-snippets.py
./buck2 test //:tutorial-snippet-tests //:tutorial-snippet-tool-tests
```

The checker only exercises the compiler. The shell commands the tutorial tells
readers to type (chapters 2, 3, 13) are not verified automatically; when
changing them, run them from a fresh shell.

## Dogfood and preview stance

The tutorial aims at the minimum dogfoodable language, but it must not pretend
future library ergonomics already exist.

- Stable behavior can be taught without caveats.
- Preview behavior can appear when it is needed for the dogfood story, but the
  prose must name the preview/in-progress status and the likely migration path.
- Standard-library examples use the ADR-0042 model: `@import("std")`,
  namespace-qualified access, module-level `const` aliases, and no prelude.
- Collection and string examples follow ADR-0043 terminology: fixed arrays,
  second-class slices, and growable `ArrayBuf` / `StrBuf` library types.

## Known limitations the tutorial works around

Recorded here so the next author does not rediscover them. Remove an entry
when the limitation is lifted and update the chapter that mentions it.

- `[T]` slice parameters accept only 64-bit element types (E0908, RUE-2055);
  the slice examples use `i64`.
- `for` over a `borrow` `StrBuf` parameter is rejected (E0429, RUE-2052); the
  examples iterate a clone or take the string by value.
- A qualified enum path cannot appear inside another variant's payload
  pattern (`R.Err(E.A(x))` does not parse, RUE-2053); the examples use a
  nested `match`.
- `?` inside a function returning `Result(T, StrBuf)` hits an internal
  compiler error (RUE-2051); the examples use enum error types.
- An immutable `let` binding is accepted as an `inout` argument to a free
  function (RUE-2054); the ownership chapter says the caller "declares" the
  binding `let mut` rather than "must" until that is enforced.
- `for` does not iterate `ArrayBuf`; the examples index over `len()`.
