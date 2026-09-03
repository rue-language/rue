---
id: 0083
title: "rue test MVP: test declarations, runner, and event protocol"
status: accepted
tags: [tooling, testing, syntax, semantics, incremental, cli, language-shape]
feature-flag: test_declarations
created: 2026-08-22
accepted: 2026-09-02
implemented:
spec-sections: []
superseded-by:
relates: ["RUE-506", "RUE-505", "RUE-504", "RUE-438", "ADR-0063", "ADR-0061", "ADR-0058", "ADR-0055", "ADR-0064", "ADR-0038", "ADR-0027", "ADR-0025", "ADR-0069", "ADR-0005"]
---

# ADR-0083: `rue test` MVP: test declarations, runner, and event protocol

## Status

Accepted 2026-09-02 for the MVP phase. This is the second ADR attempt for
the RUE-506 test runner: the first (rue-language/rue#2239, closed unmerged)
designed the full system — capability inference, verdict caching,
scheduling, a provider protocol — and review concluded it was too much to
ratify at once. This document is the MVP and stands alone; the deferred
layers are summarized in §6 with their follow-up issues.

Acceptance ratifies the four acceptance-level calls, ruled on review
2026-08-23 and recorded at their sites in the body as well as under Open
Questions: tests are language items, declared as `test "name" { ... }`
blocks (§1); discovery is the import closure for the MVP, with the
unimported-test-file warning (§1); `--filter` narrows the run set and never
the analysis root set (§2); and the structured failure channel is a
dedicated inherited pipe (§5.1). Phases 1 through 2.5 (RUE-1618, RUE-1619,
RUE-1620) are authorized to proceed on that basis.

Acceptance does not settle the lower-impact calls that Open Questions marks
decidable within their phase — exit codes, `@assert` stabilization, the
`skipped` verdict's producer, the `scripts/rue test` homonym, and naming —
nor the memo-database-pressure spike Phase 2 requires. The deferred layers
of §6 remain separate ADRs, ratified on their own.

## Summary

Rue gets a first-class test runner: `test "name" { ... }` declarations in
the language, discovered and analyzed by the compiler as ordinary
demand-driven roots, executed by a `rue test` driver mode that emits a
versioned NDJSON event stream as its primary output, with human rendering as
a consumer of that stream. Execution is a contract — isolation, independent
lifecycle, per-test timeout, exact output attribution, reproduction-as-data
— specified independently of mechanism; the MVP mechanism is one linked test
image per target plus one process per test, with the process's visible
inventory pinned to exact values. Failures are structured data from day one:
a documented failure channel that user assertion libraries share with the
built-ins, `?` in test bodies with unwrap-and-report semantics, and
structured assertion payloads (expected/actual with machine-computed diffs).

The MVP claims nothing it cannot verify: it ships zero hermeticity claims,
every test simply runs, and the event schema carries an explicit
`capability_summary: unavailable` status rather than a retrofitted optional.
Hermeticity inference, verdict caching and change-based selection,
scheduling policy, and the external-provider protocol are deferred to
focused follow-up ADRs (§6) — deferred, not rejected — and the contracts
here (pinned inventories, verdict taxonomy, reserved schema fields) are the
ones those ADRs need to land additively.

## Context

Facts about today's compiler and language that this design is built on:

- **The query graph exists** (ADR-0063, implemented): revisioned typed
  queries, explicit root sets, per-body reference projections, red/green
  publication with early cutoff, per-function `CodegenUnit` terminal
  artifacts with content fingerprints. ADR-0063 §15 names test selection as
  a planned consumer.
- **Effects have three chokepoints**: the closed 46-helper runtime ABI
  manifest (ADR-0055), the `@syscall` intrinsic, and `extern "C"` calls
  (ADR-0064). With no traits, function pointers, or threads, the call graph
  is total — which is what makes the deferred capability inference (§6)
  sound with FFI as the only opaque edge.
- **No clock API exists anywhere**; the only nondeterminism intrinsics are
  `@random_u32`/`@random_u64` (ADR-0027). Rue tests are time-deterministic
  for free.
- **The failure model is abort-only**: every trap, `@panic`, and `@assert`
  failure exits 101 with a pinned, machine-recognizable stderr message; no
  unwinding, no in-process catch. Process isolation between tests is a
  correctness requirement, not hygiene.
- **Working precedents in-tree**: `--error-format json` NDJSON diagnostics
  with deterministic ordering (docs/process/diagnostics.md); ADR-0061 §6's
  schema-versioning policy; and the `rue-test-runner` crate's process-group
  spawning, SIGKILL-on-timeout, bounded capture, and ICE detection, adopted
  here as contract.

Prior art, one lesson each: **cargo-nextest** — process-per-test is the
right contract, but own the machine-readable stream from day one instead of
retrofitting it. **Go** — cache-as-default UX and stream-first output;
avoid observed (unsound) hermeticity. **Zig** — `test "name" { }` blocks as
compiler-discovered language items; its gaps (undocumented protocol, events
sharing the child's stdout) are avoided by a documented contract and
runner-owned streams. **Swift Testing** — versioned event stream as the
tool ABI, with rich v1 events rather than retrofitted metadata. **Bazel** —
the hermetic environment contract, which Rue can eventually enforce rather
than document. **Buck2/tpx** — the runner as an explicit protocol client of
the build, not a subroutine. **Deno** — coarse capabilities with refinement
is the usable granularity. **Unison** — content-addressed caching of pure
tests, which the deferred verdict cache (§6) makes granular and inferred.
**Nim** — zero-annotation bottom-up effect inference works at compiler
scale.

## Scope

In scope: test declarations in the language; compiler discovery and
analysis; the `rue test` driver mode; the event stream schema and its
reserved fields; the execution contract and the process-per-test MVP
mechanism; the structured failure channel; structured assertion payloads.

Deferred to follow-up ADRs, deliberately (§6): capability inference and
hermeticity verification (RUE-1621), hermetic verdict caching and
change-based selection (RUE-1622), scheduling and flake policy (RUE-1623),
and the external test-provider protocol (RUE-1624). Deferred, not rejected:
the direction stands, and this ADR's contracts are shaped so each arrives as
an additive change.

Out of scope entirely: doctests (needs RUE-504's doc model), the
user-authored-framework wire protocol (needs RUE-505; only the seam is
reserved), benchmark/property/fuzz frameworks themselves, a
package/workspace model (the runner takes a root module exactly like the
compiler), and any persistent cross-process memo database (ADR-0063 future
work).

## The boundary: compiler CLI vs build integration

Everything this ADR specifies lives in the compiler. `rue test` is a driver
subcommand; discovery, analysis, the test image, process execution, capture,
and the event stream are all compiler behavior, fully usable with no build
system:

```
$ rue test app/main.rue
```

Build integration adds exactly one optional input in the MVP: a declared
candidate inventory (`--test-candidates`, which the `rue_program` build rule
feeds from its `srcs`) powering the unimported-test-file warning of §1.
Without it, that warning degrades to a one-line notice — nothing else
changes. No discovery, scheduling, or execution behavior lives in the build
system, and the deferred layers (§6) keep the same boundary. (The
maintainers' `scripts/rue test` compiler-suite wrapper is an unrelated
homonym; see Open Questions.)

## Decision

### 1. Tests are language items, discovered by the compiler

A test is a declaration, not a convention:

```rue
const std = @import("std");

fn parse_port(s: StrBuf) -> Option(u16) { ... }

test "parse_port accepts the loopback default" {
    @assert(parse_port(StrBuf.from("8080")).is_some());
}

test "parse_port rejects out-of-range values" {
    @assert(parse_port(StrBuf.from("70000")).is_none());
}
```

The declaration surface is settled: **by ruling (2026-08-23)**, tests are
language items spelled as `test "name" { ... }` blocks, not a `@test`
directive on a function. The directive alternative was weighed across the
design-capture rounds on rue-language/rue#2239 and is not revisited here.

- **Grammar**: `test_item = directives "test" STRING block ;` at item
  position. `test` is a contextual keyword (item-position `test` followed by
  a string literal), so existing identifiers named `test` — and they are
  live: `std/bitset.rue` has a `fn test` method — do not break. This is the
  language's first contextual keyword; Phase 1 prices that in.
- **Identity**: the string is the test's name, unique within its module
  (duplicates are a semantic error). The stable test ID is the module's
  canonical identity plus the name, under ADR-0063 §5's stable identity
  domain — insensitive to reordering and unrelated edits. The rendered ID
  spelling is pinned in the Phase 2 schema doc.
- **Body typing**: the block has type `()`. A test passes when its process
  exits 0 **and** the dispatcher's completion record (§3) was observed; it
  fails when it traps, exits nonzero, is killed, or exits 0 without the
  completion record.
- **`?` in test bodies: unwrap-and-report.** A test body may apply `?` to
  spec 4.15:3's standard `Option`/`Result` producers, with test-specific
  failure semantics: the success arm is ordinary, and the failure arm emits
  a structured `unhandled_error` record — the `?` site's span, plus the
  error payload rendered by a compiler-synthesized structural printer
  (variant name and primitive/byte-string payloads, bounded by the §2
  capture budgets; keyed by error type, like drop glue, so repeated sites
  share one instance) — and traps at the site. This is an additive spec
  rule: E0503/E0505 reject `?` in `()`-returning bodies today, so no
  existing program changes meaning. It is scoped to the test item's
  immediate block; helper functions keep ordinary `?` rules and compose by
  being `?`-ed at the boundary. The rule ships whole in Phase 2 — legality
  and failure-arm lowering together — because `analyze_try` builds the
  `Option`/`Result` match and early return against the enclosing return
  type *as it analyzes*, leaving no separable legality-only step: a
  `()`-typed test body cannot analyze successfully until the test-specific
  failure arm exists, so through Phase 1 test-body `?` simply remains the
  compile error it is today. Trapping rather than propagating buys
  `@assert`-grade site attribution with no backtrace machinery, per-site
  error types (no enclosing `Err` is constructed, so spec 4.15:4's
  identical-error-type rule never applies), and no signature surface. One
  consequence is accepted, not mitigated: a propagating `?` is an early
  return and runs drop elaboration; the trap does not, so a `drop fn`'s
  observable work is skipped on the failing path. That is consistent with
  `@assert`, `@panic`, and every trap; process death plus the retained
  scratch directory covers reclamation; recorded under Consequences.
  Expected failures are unaffected: asserting that a call returns `Err` is
  an ordinary match — `?` is for the errors a test does not expect.
- **Placement is the visibility model.** A test sees exactly what its
  module's other items see. Rue's visibility boundary is the directory
  (spec 10.3), so a sibling `parser_tests.rue` in the same directory
  exercises private items with no visibility loosening, while a test module
  in another directory sees only the public API and proves its sufficiency.
  "Internal or contract test?" is answered by where the file sits — no
  `pub` changes for testability, and no obligation to put test text in
  production files.
- **Test items are roots, not reachable code.** An executable request never
  roots them: no semantic analysis, no codegen, no linking. A test request
  roots every test item in the root module's transitive `@import` closure —
  including modules imported only for their tests. Multi-root requests are
  not new machinery (`extern "C"` exports already join `main` as co-equal
  roots).
- **Discovery is the import closure — so a test-only file must be wired
  into it.** A `parser_tests.rue` that nothing imports does not exist to
  any request. The MVP idiom keeps the wiring out of production bodies: one
  aggregator import, in the root file or a dedicated file the root imports:

  ```rue
  // main.rue
  const tests = @import("parser_tests.rue");
  ```

  (Phase 1 must ensure the unused-item scan treats such an import as used,
  or the idiom fights the linter.) This wiring requirement is a known
  papercut in Zig, and it is accepted for the MVP **by ruling
  (2026-08-23)**: discovery stays import-closure-only, and directory
  contents do not become compiler inputs. The convention-based alternative
  is recorded under Rejected alternatives; the package-model end state
  that may revisit it stays under Deferred design questions.

  Because forgetting the wiring produces silence, the MVP ships an
  **unimported-test-file warning**: given a declared candidate inventory
  (`--test-candidates`; see the boundary section), the runner reports
  declared files outside the closure that parse as containing test items.
  The derived source manifest cannot serve as that inventory — it omits
  never-read files (exactly the orphans) and contains all of std — so the
  inventory is the declared `srcs` set, and candidate acquisition is a
  bounded ADR-0063 host-input step: candidate bytes (or typed
  absent/unreadable outcomes) are published as revisioned inputs, consumed
  by a parse-only query that never mints semantic roots. It stays a
  warning, never an error: sibling build targets legitimately share a
  `srcs` glob, so an unread file may be another root's tree. Candidate
  parse failures are reported inside the warning; absent entries stay
  silent; with no inventory supplied, the run summary carries a one-line
  notice instead.
- **Warnings interaction, decided here**: test bodies are included in the
  whole-program syntactic reference scan that filters unused-item warnings,
  so test-only helpers do not warn in executable builds. Cost stated
  honestly: test items in the closure cost executable requests parse plus
  that syntactic scan — nothing more.
- **Preview gate**: `test_declarations`, required explicitly like every
  other preview feature — `rue test` does **not** enable it implicitly.
  The gate covers a parser change, so any request whose closure contains
  test items — executable builds included, which parse test items for the
  warnings scan — needs the flag to compile at all. Auto-enabling it in
  test mode alone would leave those same files failing ordinary builds
  while making `rue test` the first flag to bypass ADR-0005's explicit
  opt-in, for no net convenience. Declaring a test without the preview
  enabled is the standard preview-gate diagnostic.

### 2. `rue test` is a driver mode emitting a versioned event stream

```
rue test <root.rue> [--list] [--filter <pattern>]... [--format human|json]
         [--jobs N] [--timeout-ms N] [--shard K/N] [--target <t>] [-O<n>]
         [--seed N] ...
```

What a user sees (illustrative, not normative — exact spellings are pinned
by the Phase 2 schema doc and CLI cases):

```
$ rue test app/main.rue
FAIL app/parser_tests.rue: "parse_port rejects out-of-range values"
  panic: assertion failed  (app/parser_tests.rue:7)
  repro: rue test app/main.rue --filter "parse_port rejects out-of-range values"
41 passed, 1 failed (0.9s)

$ rue test app/main.rue --format json
{"event":"run_started","schema":"1.0","root":"app/main.rue","seed":417,...}
{"event":"test_started","id":"app/parser_tests.rue::parse_port rejects out-of-range values"}
{"event":"test_finished","id":"...","verdict":"fail","duration_ms":3,
 "capability_summary":{"status":"unavailable"},
 "failure":{"kind":"assert","message":"panic: assertion failed","span":"app/parser_tests.rue:7"},
 "stderr":{"encoding":"utf8","bytes_total":24,"data":"..."},
 "repro":["rue","test","app/main.rue","--filter","parse_port rejects out-of-range values"]}
{"event":"run_finished","passed":41,"failed":1,...}

$ rue test app/main.rue --list --format json     # discovery, no execution
$ rue test app/main.rue --filter parse_port      # run a subset
```

- **Dispatch rule**: the driver enters test mode when the first argument
  that is neither a flag nor a flag's value is exactly `test` — the scan is
  value-aware because eleven existing flags take a following value, and
  `rue -o test prog.rue` must keep meaning "output named `test`" (a root
  literally named `test` is spelled `./test`). This is the driver's first
  subcommand; it joins `--watch`'s flag-combination validation path, and
  all compile-mode flags that make sense in test mode keep their spellings.
- **Streams**: compiler diagnostics remain on stderr exactly as today
  (`--error-format json` unchanged and orthogonal). Test events are the
  runner's own surface on stdout: NDJSON with an explicit schema version in
  the head event, per ADR-0061 §6. Events are produced from session
  artifact views, never scraped from human output; the human renderer is a
  consumer of the same stream, which keeps the machine surface honest.
- **Event kinds** (sketch): `run_started` (schema version, root, target,
  plan, seed), `test_started` (stable ID), `test_finished` (ID, verdict,
  duration, capability summary, failure structure, captured output, repro
  argv), `run_finished` (counts, wall time). **Verdicts**: `pass`, `fail`,
  `timeout`, `crash` (killed by signal), `skipped` — the last with no MVP
  producing mechanism, an open question below. `compile_error` (as a
  per-test verdict, §3), `cached_pass`, and the `ice` failure kind are
  reserved in the schema for the deferred work (§6) and are unproducible in
  the MVP, whose whole-run compile failure is exit code `2`. A **failure
  record** is data: kind (`assert` / `unhandled_error` / `trap:<class>` /
  `exit` / `signal` / `timeout` / `output_overflow` / `incomplete` — the
  last for exit 0 with no completion record, §3), the pinned runtime
  message, exit code or signal, and a source location — the test
  declaration's span, except `unhandled_error`, which carries the failing
  `?` site (§1). Payload and location fields are extension points: richer
  expected/actual payloads arrive through the failure channel (§5.1) as
  additive minors, never by parsing prose. `capability_summary` is present
  from v1.0 as `{"status": "unavailable"}` — zero claims, stated in-band —
  and is populated by the deferred capability ADR (§6) as an additive
  change inside a field consumers already handle.
- **Captured output is bytes, budgeted.** Rue strings may carry arbitrary
  non-UTF-8 bytes written raw, so output fields carry an encoding tag
  (UTF-8 when valid, base64 otherwise) and are lossless within the
  retained window. Capture is bounded per stream as bytes arrive (the
  `rue-test-runner` limited-drain mechanics); exceeding a stream's limit
  kills the process group and yields `fail` with kind `output_overflow`,
  retained prefix attached. Failing tests carry retained capture inline;
  passing tests carry digests and byte counts, with a flag to opt passes
  into inline capture. The §5.1 failure channel is budgeted separately, so
  a test that floods its streams cannot truncate its own failure record.
- **Asymmetric verbosity**: the human renderer prints failures in full —
  structure, captured output, repro line — and passes as a count. No wall
  of green.
- **`--list`**: emits the inventory — IDs, declaration spans, the
  `capability_summary` state — with semantic analysis of test closures but
  no codegen, no linking, no execution. Two additive surfaces are reserved
  rather than shipped: a cache-status tier (`--list --cache-status`)
  arrives with the deferred verdict cache (§6), priced honestly as
  materializing closure terminal artifacts, which the default listing must
  never pay; and the inverse query `--list --reaches <item>` ("which tests
  reach item X" — the sound, static version of test-impact analysis) with
  reached-set provenance reserved in the schema from v1.
- **Filtering**: `--filter` matches test IDs; repeated filters union. A
  filter selecting zero tests is an error with a distinct exit code — an
  empty selection is how a typo becomes false evidence. **By ruling
  (2026-08-23), filtering narrows the run set, never the analysis root
  set**: the request still roots every test in the closure, so a filtered
  run's verdicts are identical to the same tests' verdicts in a full run —
  the property future selection soundness requires. A broken, unselected
  test therefore still fails the compilation, until per-test
  `compile_error` verdicts (§6) contain it.
- **Exit codes** (proposed): `0` all selected passed, `1` at least one
  failure, `2` compilation or runner error, `3` empty selection.
- **Sharding**: `--shard K/N` partitions deterministically by stable ID
  hash. Duration-aware bin-packing waits for recorded-duration history
  (deferred with scheduling, §6).

### 3. Execution is a contract; the MVP mechanism is a test image plus a process per test

The contract every mechanism must honor: each test observes fresh process
state; has an independent lifecycle (start, kill, timeout) enforceable by
the runner; gets its stdout/stderr captured and attributed exactly; and
receives best-effort process-tree cleanup. One clause is deliberately
scoped: **noninterference — one test cannot corrupt, mask, or abort another
test's result — will be guaranteed only for verified-hermetic tests**, a
claim that becomes available with the deferred capability ADR (§6); the MVP
verifies nothing and claims this for no test. A test doing raw syscalls or
FFI can signal arbitrary processes or contend on shared OS state, and a
process group is not containment (`setsid` leaves it). Process isolation
plus a private scratch directory narrows the accident surface well below
mainstream baselines, but it is not a sandbox, and this ADR does not
pretend otherwise.

The MVP mechanism:

- **One test image per target.** The compiler synthesizes a dispatcher
  `main` — an ordinary generated function with two in-tree precedents (drop
  glue, the export thunk) — that reads a selector from argv and invokes
  exactly one test body. The image links every selected test's
  `CodegenUnit` closure through the image-planning path (exposing a
  test-image request is ADR-0061 facade work inside Phase 2); per-function
  artifacts are shared with regular builds via the memo database, so the
  marginal cost is one link, not N compiles. Dispatcher code is runner
  plumbing: it is excluded by construction from the capability summaries
  and closure fingerprints the deferred ADRs compute (§6).
- **One process per test invocation**, in its own process group, with a
  per-test wall-clock timeout (default 10 s, matching `rue-test-runner`),
  SIGKILL to the group on expiry, reader threads draining both pipes with
  §2's capture bounds, and a post-exit group SIGKILL to reap stragglers
  (hygiene, not containment). Timeout kills, signal deaths (SIGPIPE's 141
  included), and exit-101 traps are distinguished in the verdict; ICE
  detection stays a separate failure class. The runner keeps both pipes
  open and drained until the child exits — a stdout-writing test must never
  die with SIGPIPE because a reader closed early. A body calling
  `std.exit(0)` before its assertions must not report as a pass — that is
  false-positive test evidence, not acceptable hygiene debt — so the
  dispatcher writes a **terminal completion record** on the §5.1 channel
  after the test body returns normally, and the runner treats exit 0 with
  end-of-stream but no completion record as a failure (kind `incomplete`,
  §2). Only the dispatcher's epilogue writes the record. The channel is
  not a security boundary (§5.1), so a test could deliberately forge
  completion — but the defect being closed is the accidental early exit,
  and deliberate self-deception is outside every runner's threat model.
- **The visible inventory is pinned to exact values.** The runtime captures
  loader-provided `argc`/`argv`/`envp` at entry and exposes them through
  `std.env`, so without a defined boundary a dispatched process would leak
  the image path, selector, and per-run scratch paths into test-visible
  state — values that vary while the test's closure does not, which would
  poison the deferred cache key (§6) or its soundness. Two inventories are
  therefore contract values. The **test-visible inventory**: fixed argv
  with a stable logical `argv[0]` and no selector; a fixed ordered
  environment list, with runner-set `RUE_TEST_*` variables carrying stable
  logical values; the scratch directory is always spelled `.` — the fresh
  private working directory each test starts in, deleted on pass, retained
  on failure; stdin is a fixed EOF stream. The dispatcher consumes the
  selector and presents the documented values, which is also what makes
  tests *of* `std.env` meaningful. The **loader-visible inventory**: the
  runner execs every test with a constant `argv[0]`, a fixed-width
  selector, exactly the pinned environment vector, and a run-constant image
  path spelling — because the loader lays the real strings on the initial
  process stack, so their sizes are stack consumption no later pointer swap
  can undo. Pinning them makes initial-stack consumption deterministic per
  keyed configuration, which is what makes a pinned `RLIMIT_STACK` a real
  determinism boundary. These exact values participate in the deferred
  verdict-cache key (§6); their stability is what will keep routine runs
  cache-hittable.
- **Parallelism**: up to `--jobs` concurrent test processes. In the MVP
  every test runs in parallel by default — a pragmatic default, not a
  guarantee: unverified tests *can* interfere through the OS, and a suite
  that observes it reaches for `--jobs 1` today, declared serial groups in
  the deferred scheduling work (§6), and a platform sandbox eventually.
  When capability inference lands, verified-hermetic parallelism becomes
  unconditional. The runner never silently serializes; scheduling changes
  are always visible policy.
- **Reproduction as data**: every failure event carries the exact argv to
  reproduce that single test under the same seed, target, opt level, and
  filter.
- **Per-test compile failure is a verdict, not a run abort** (deferred,
  §6). Because each test's closure is analyzed independently as a root, a
  semantic error in one closure can yield a `compile_error` verdict —
  exclusion from the image, not stubbing — while every other test runs.
  The MVP keeps the simpler whole-run-fails behavior; the per-test contract
  is specified now so Phase 2 does not bake in the coarser one. When it
  lands, **stderr remains the authoritative diagnostic stream** (byte-
  for-byte as docs/process/diagnostics.md pins it); the copy embedded in a
  `compile_error` event is an attribution convenience, and divergence
  between the two is a runner bug. Whether the event should carry full
  diagnostics or only identities is deferred with that work.

### 4. Determinism defaults

- The runner pins both §3 inventories; `env`- and `args`-observing tests
  are deterministic given values pinned by contract — and, later, in the
  deferred cache key (§6).
- There is no clock to virtualize; the absence is load-bearing. Any future
  time API must arrive behind a `clock` capability (the standing
  constraints section that travels with the deferred capability ADR
  records this and its siblings, §6).
- `--seed N` is accepted and reported in `run_started` and every repro
  argv, but in the MVP it feeds only the runner's own choices (shuffle
  order, scratch naming). Making `@random_*` seedable in test builds is a
  runtime/codegen change whose maintainer call is deferred with scheduling
  (§6; its cache-key interaction travels with RUE-1622).
- Execution order is shuffled by seed by default: shuffling keeps order
  dependence visible, and the seed makes any surprise reproducible. Once
  capability inference lands (§6), verified isolation makes order
  dependence impossible for hermetic tests.

### 5. Extensibility is tiered, in-language first, with no privileged built-ins

The built-in framework must be the default, not the ceiling: assertion
libraries, BDD layers, property testers, and replacement runners have to be
writable without reopening this ADR. What is deliberately *not* extensible:
the verdict taxonomy's meaning, the isolation contract (§3), and the
eject-don't-degrade soundness posture the deferred capability ADR ratifies
(§6).

#### 5.1 Assertion libraries are first-class by protocol, not by blessing

The channel through which a failing test reports structure — kind, message,
expected/actual payload, failing-call-site location — is a documented
runtime protocol. `@assert` today, `@assert_eq` in Phase 2.5, and the
test-body `?` failure arm (§1) are sugar over the same channel any Rue
function can invoke before aborting; user libraries emit the same records,
and the stream carries them without knowing who produced them.

The mechanism — **ruled 2026-08-23** — is a **dedicated inherited pipe**:
its own file descriptor, pinned in the §3 exec contract, written through a
runtime helper (an ABI-manifest addition under ADR-0055 rules), drained by
the runner with its own budget. The rejected shape was a framed region of
stderr: user streams are arbitrary bytes, so any in-band framing can be
forged, and a "separate budget" extracted from a shared capped stream is
separate in name only. The channel is not a security boundary; it prevents
accidental collision, which is what §2 promises. It also carries the §3
terminal completion record, written by the dispatcher's epilogue alone.

Its capability class is stated now rather than discovered later:
**hermetic-compatible, on the same grounds as stdout** — runner-pinned,
fully captured, budget in the future cache key. The helper ships in Phase 2
while the machine-checked manifest capability field arrives with the
deferred capability ADR (§6); until then this paragraph and the schema doc
carry the classification.

Two deliberate consequences: the failure payload is an open, versioned
field rather than an enum of built-in shapes, and location is carried *in*
the record so a library can attribute its caller (automatic call-site
capture wants a `@src()`-style intrinsic — deferred, nothing blocks it).
The record also reserves a **promotion** payload from v1: a failure may
carry a machine-applicable suggested fix (the expect-test pattern: new
expected value, target span, content hash of what it replaces), applied by
a future `rue test --accept` — the runner applying promotions, never the
test, is what keeps snapshot tests hermetic.

#### 5.2 In-language frameworks are ordinary Rue code; comptime is the generator

BDD vocabularies, table harnesses, and property-test case machinery are
plain functions and comptime constructs used inside test bodies from day
one; they will inherit capability inference, caching, and selection
automatically when the deferred layers land (§6), because their helpers are
reached bodies like any other. What v1 does not give them is per-case
identity: one `test` block looping over a table is one verdict, one
filterable unit (and, later, one cache entry). Two extensions are reserved
so that ceiling lifts without redesign: **test items in
comptime-instantiated types** (the Zig shape; ADR-0063 §5's identity domain
already covers specialization-anchored members, and the event schema
carries an opaque stable ID plus structured identity fields so IDs can grow
components as a schema minor), and **sub-results** — a running test may
emit named child results over the §5.1 channel, attributed as
`<test-id>/<sub-name>` rows; scheduling and caching stay at the item level.
Reserved in the schema from v1, implemented when demanded.

#### 5.3 Reporters and observers consume the stream

Custom reporters, CI adapters, dashboards, and IDE surfaces are NDJSON
consumers with no protocol negotiation. This works from Phase 2 and is the
intended default extension point; a JUnit adapter is the reference
consumer.

#### 5.4 Alternative runners and external providers use documented contracts

A replacement runner needs no new privileges: enumerate with
`--list --format json`, execute through the test image's documented
argv/exit/stream contract — public by commitment from Phase 2, and nothing
in this ADR or the deferred work may depend on it staying private — and
schedule however it likes. The reverse direction, external test
*providers*, is the deferred provider protocol (§6, RUE-1624), decided
together with RUE-505's format policy; this ADR reserves the seam.
Provider-supplied tests are not compiler-visible bodies, so they get no
verified summaries: always executed, never cached, unless the provider
generates real Rue test items.

Fixtures, honestly: setup is plain code, teardown is destructors — and the
abort-only runtime means destructors do not run on a failing path, so
teardown-on-failure is process death plus the retained scratch directory.
Expensive fixtures shared across tests are a runner-policy question
(serialized groups, deferred with scheduling, §6) — noted as not locked
out, not designed here.

### 6. What is deferred, and what keeps it additive

Each deferred layer becomes its own ADR with its own evidence, spikes, and
maintainer calls; the full design capture lives in rue-language/rue#2239
and is seeded into the follow-up issues. Deferral is a ratification
decision, not a direction change.

- **Capability inference** (RUE-1621). Per-function capability summaries
  inferred bottom-up over projections of the ADR-0063 query families,
  grounded in the three effect chokepoints (Context). Carries the
  highest-stakes maintainer call (summaries stay out of the type system),
  the RUE-967 `@ptr_to_int` disposition, and the "Constraints on future
  language evolution" standing section, which travels with it.
- **Hermetic verdict caching and selection** (RUE-1622). Verified-hermetic
  verdicts as cacheable artifacts keyed on closure fingerprints plus the
  §3 pinned values; `--changed-only` as the same predicate; allocation
  determinism via a test-build budgeted page mapper; per-test
  `compile_error` verdicts. The caching and selection items require
  capability inference; the per-test `compile_error` mechanism (§3) does
  not, and may land independently ahead of the rest.
- **Scheduling and flake policy** (RUE-1623). Declared serial groups,
  `--reruns` for non-hermetic tests, hermetic-mismatch reporting, and
  duration-fed sharding; the seedable `@random_*` maintainer call (§4)
  travels with it. Two of its four items — `--reruns` for non-hermetic
  tests and hermetic-mismatch reporting — require capability inference;
  serial groups and duration-fed sharding do not.
- **The public provider protocol** (RUE-1624). The versioned
  enumerate/execute-by-ID protocol for external providers, decided with
  RUE-505; JUnit and CTRF adapters as reference consumers.

What the MVP does now so those land additively: `capability_summary` ships
in v1.0 with an explicit `unavailable` status (§2); the verdict taxonomy
reserves `compile_error`, `cached_pass`, and the `ice` failure kind without
producing them, and §3 specifies the per-test compile-failure contract so
Phase 2 does not bake in the coarser shape; the §3 inventories are pinned
to exact values — precisely what makes verdicts keyable later; `--list`
reserves the cache-status tier and `--reaches` (§2); and the failure
channel reserves promotion and sub-result shapes with its capability
classification stated (§5.1, §5.2).

## Implementation Phases

Filed in the Linear project "rue test MVP"; the deferred layers live in the
companion project "rue test follow-ups" (§6).

- [x] **Phase 1: `test` declarations** - RUE-1618. Grammar/lexer/parser
      (first contextual keyword, directives allowed), RIR item, a new
      `Test` kind in the closed stable-definition taxonomy plus its
      namespace decision, semantic analysis as ordinary bodies rooted only
      by test requests, the warnings-scan inclusion decision (§1),
      duplicate-name diagnostics, `test_declarations` preview feature, spec
      sections + spec-test coverage, UI coverage. Test-body `?` remains the
      compile error it is today throughout this phase — the §1 rule ships
      whole in Phase 2, since `analyze_try` has no legality-only step. No
      runner: `--emit`-level verification that test items parse, analyze,
      and are invisible to executable requests.
- [x] **Phase 2: `rue test` MVP runner** - RUE-1619. Value-aware subcommand
      dispatch; test-request root sets; dispatcher `main` and per-target
      test image through the image-planning path plus the ADR-0061 facade
      work; the loader-visible exec contract with dispatcher normalization
      to the test-visible values (§3); process-per-test execution with
      process-group timeout/kill, bounded capture, post-exit cleanup
      (mechanics shared with `rue-test-runner`); import-closure discovery
      as ruled, with the unimported-test-file warning over
      `--test-candidates` and its bounded candidate-acquisition step (§1);
      `--list`, `--filter` (run-set semantics, as ruled), `--jobs`,
      `--shard`, `--timeout-ms`, `--seed` (shuffle), exit codes; NDJSON
      event stream v1.0 with schema doc — landed as
      `docs/process/test-events.md` (RUE-1920), covering the exec contract,
      encoding, budgets, payload asymmetry, the verdict taxonomy and its
      reserved values, the `--filter` rule, the shard hash and shuffle
      PRNG, and the `capability_summary` unavailable state (§2); the
      test-body `?`
      rule whole — legality and failure-arm lowering together (§1); the
      dispatcher completion record and its `incomplete` failure kind (§3);
      the failure-channel contract with reserved promotion and
      identity/sub-result shapes
      (§5.1, §5.2); the human renderer as a stream consumer; repro argv on
      every failure; CLI-suite coverage end to end. Includes the
      memo-database pressure spike (Open Questions). **This phase is the
      minimum usable runner: agent-first, zero capability claims — every
      test simply runs.**
      Shipped in four slices: RUE-1917 (failure channel, test image, dispatcher
      `main`), RUE-1918 (declared test-candidate inventory and the orphan
      warning), RUE-1920 (the `rue test` subcommand, runner, and event stream),
      RUE-1921 (`?` in test bodies with unwrap-and-report semantics).
- [ ] **Phase 2.5: structured assertion payloads** - RUE-1620. `@assert_eq`
      (and a minimal comparison family) as intrinsics producing
      expected/actual through the §5.1 channel; machine-computed diffs in
      `test_finished` events; human renderer built from the same payloads.
      Pulled ahead of all deferred work deliberately: unstructured failure
      output is the primary agent token sink, and a runner that is
      agent-first in transport but prose in content has not met the bar.

## Consequences

### Positive

- The MVP needs no capability system, no new persistence, and no
  spec-invasive machinery — a subcommand, a grammar item, a generated
  `main`, and a schema doc, on infrastructure that exists and is tested.
- The runner never promises what it cannot verify: zero hermeticity claims,
  stated in its own schema, with verification left to a follow-up that can
  prove it (§6).
- Agents get structure end to end: discovery without execution, failures as
  data with spans and repro argv, asymmetric verbosity, stable IDs,
  versioned schema.
- The event schema and execution contracts are forward-designed for the
  deferred layers (§6), so none arrives as a retrofit.
- Extensibility has no privileged built-ins (§5).

### Negative

- One more consumer-visible versioned surface to maintain under ADR-0061 §6
  discipline, plus a schema doc, plus CLI cases pinning it.
- Process-per-test puts a floor under per-test latency; batching waits for
  the deferred scheduling work (§6), which the abort-only runtime makes
  genuinely hard.
- Without caching or selection, every run executes every selected test —
  the MVP's answer is honest parallel speed, not skipped work; the
  economics wait for RUE-1622.
- Test-only files must be wired into the import closure (§1, as ruled) —
  the sharpest ergonomic edge in the MVP, mitigated by the orphan warning
  only where a build system supplies the inventory, and revisited only if
  the package model makes declared sources a canonical inventory.
- A generated dispatcher and per-target test image add a compiler-
  synthesized artifact to maintain across both backends; contextual-keyword
  parsing adds grammar subtlety.
- `?` in a test body skips destructors on the failing path (§1) — uniform
  with every trap, but a `drop fn` whose observable work is a flush or an
  external release does not perform it when a `?` fails.

### Neutral

- `scripts/rue test` and the user-facing `rue test` become homonyms; docs
  need a disambiguation pass whichever rename option is taken.
- Tests may live beside code or in same-directory siblings (spec 10.3) —
  production files need not carry test text; executable requests pay parse
  plus the syntactic warning scan for test bodies in the closure.
- If Rue ever gains catchable failures or unwinding, verdicts survive
  unchanged — they are defined by the §3 contract, not the abort mechanism
  — and failure reporting migrates into the §5.1 channel, which is
  mechanism-neutral. Destructors would then run on failing paths,
  revisiting §1's accepted posture and §5.4's fixture note. The full
  standing section on future language evolution travels with the
  capability ADR (§6).

## Rejected alternatives

### Assertions as Result-returning functions

Considered: assertions as ordinary functions returning
`Result((), AssertFailure)`, propagated with `?`. Rejected: an assertion
claims program state is correct, and a failed one is a bug — the language
already routes expected failures through `Result`/`Option` with must-check
linearity (ADR-0038) and invariant violations through traps (spec ch. 8).
A value-returning assert makes every asserting function transitively
fallible; must-check is not must-stop (the legal handlings are a verbose
re-implementation of the trap, or match-and-continue on a violated
invariant); and with no unwinding, a trap is the only non-local exit a leaf
helper has. The intrinsic form buys call-site attribution without
backtraces, comptime folding, optimizer-visible facts, and a spec-pinnable
message/exit contract. What is given up and where it is recovered: soft
assertions fall out of fail-fast — userland accumulators and §5.2
sub-results are the answers; destructor-based teardown does not run on the
trap path — process teardown plus the retained scratch directory covers it
(§3). The intrinsic is not a monopoly: §5.1 gives userland libraries the
same channel.

### Result-typed test bodies

Considered: typing test blocks `Result((), E)` so `?` propagates (the Rust
shape), or keeping bodies `()`-only with no `?` at all. Both rejected in
favor of §1's unwrap-and-report `?`. Propagation surrenders the failing
site: with no backtraces, an `Err` walking out of the body reaches the
dispatcher as a bare value — the report can name the test but not the line
— and recovering the span would require instrumenting `?` lowering anyway.
What early return does buy, and the trap does not, is drop elaboration;
§1 accepts that explicitly rather than claiming it away (every other
failure path already skips destructors; if Rue gains unwinding, all
failing paths get revisited together — recorded as a Neutral consequence).
Propagation also inherits spec 4.15:4's identical-error-type restriction
(no error conversion without traits) and needs a return-type annotation
serving only this feature. `()`-only without `?` taxes every fallible
call: ADR-0038 makes the match compulsory, and without a Display trait the
hand-written `Err` arm degrades to a static message — the adopted form is
that same ceremony machine-written, with the payload rendered. The cost
accepted knowingly: `?` in a test body has different dynamic semantics
than `?` elsewhere. The position is a compile error today, the divergence
is exactly the failure arm, and trapping remains the better end state even
after error conversion lands.

### Convention-based test-file discovery

Considered: auto-rooting conventionally named test files
(`foo_tests.rue`) in test mode by probing exactly the directories the
import closure occupies — a bounded, policy-scoped host demand of the same
input shape as candidate acquisition, not a recursive walk. It would
remove the import-wiring papercut for sibling tests and make orphan
detection self-contained: near-miss names could warn from the same
listing, with no build-system inventory needed. Rejected for the MVP
(ruled 2026-08-23): it makes directory contents compiler inputs — a new
host-input kind whose listings must join revisions and fingerprints —
pins a filename pattern in the spec permanently, and, until per-test
`compile_error` verdicts land (§6), gives one broken conventional file
whole-run blast radius. Cross-directory public-API test modules would
still need wiring or the package model under either answer. The ruling
forecloses ambient directory probing, not the end state: under a package
model the declared source set becomes a canonical candidate inventory,
and rooting declared test files automatically is the natural revisit
(Deferred design questions).

## Open Questions

### Ruled on review (2026-08-23)

The three acceptance-level questions, plus the failure-channel mechanism,
were ruled by Steve on the proposal PR; the body text records each at its
site.

- **Test declaration surface**: `test "name" { ... }` blocks, as drafted
  (§1).
- **File discovery**: import-closure-only for the MVP, with the optional
  declared-candidate inventory warning; directory contents do not become
  compiler inputs. The convention-based alternative is recorded under
  Rejected alternatives; the package-model end state stays a deferred
  question.
- **`--filter` narrows the run set, never the analysis root set** (§2).
- **The structured failure channel is a dedicated inherited pipe** (§5.1).

### Lower-impact decisions (decidable within their phase)

- **Exit-code and `@assert` stabilization**: promote `@assert`/`@panic`
  from the reserved intrinsic bucket (4.13:5b) to normative, and decide
  whether assertion failure keeps exit 101 (recommended; distinguished by
  pinned message) or gets a distinct code.
- **Exit-code contract** (§2): **settled in Phase 2c (RUE-1920)** as the
  0/1/2/3 proposal, empty-selection-as-error included, and published in
  `docs/process/test-events.md`. Everything that stops a `rue test` run from
  happening — a compile failure, a failing image link, an ICE, a bad flag
  combination — is `2`, so an agent branching on the status never has to
  also parse stderr to tell those apart. How a future per-test
  `compile_error` verdict maps onto exit codes is deferred with that verdict
  (§6, RUE-1622).
- **The `skipped` verdict has no producing mechanism**: **settled in Phase 2c
  (RUE-1920)** by reserving `skipped` **out** of the v1 taxonomy. `@skip` is
  deferred with directive-grammar work, and filtering removes tests from the
  selection rather than reporting them, so v1 has no producer — and an
  unproducible verdict in a published enum is a consumer trap. It is
  documented as reserved in the schema doc and emitted by nothing; naming a
  producer (platform scoping is the natural candidate) is an additive minor.
- **The `scripts/rue test` homonym**: the rename remains open. Phase 2c took
  the interim half of the choice — the quickstart entry now says the wrapper
  is the maintainers' compiler-suite runner and points at
  `docs/process/test-events.md` for the language's own subcommand — so the
  rename (e.g. `scripts/rue suite`) is a maintainer call that no longer
  blocks anything.
- **Naming**: `test_declarations` preview flag; `--test-candidates`; a new
  `test-events.md` under `docs/process/` as the schema doc home.

### Questions that need a spike during Phase 2

- **Memo-database pressure under test roots**: a test request roots a
  strict superset of `main`'s closure; measure the ADR-0063 §14 retention
  budgets against check-all-shaped root sets on the larger example
  corpora. The §14 calibration is `main`-rooted, so this is the first such
  measurement; the budgets are soft, so the failure mode is memory growth,
  not rejection.

The deferred layers carry their own maintainer calls and spikes — the
allocation-determinism mechanism, the `@ptr_to_int` disposition, seedable
`@random_*`, the verdict-cache key audit, and batching mechanics among
them — recorded in the follow-up issues (§6), not here.

### Deferred design questions

- Doctests (RUE-504 coordination); the §5.4 seam is where a doc-example
  provider plugs in.
- Per-test metadata (`@timeout(ms)`, `@skip`, `@known_bug("RUE-NN")` with
  XPASS-fails-loudly, `@tag(...)`): wanted, but directive arguments are
  identifier-only today, so literal-argument directives are a grammar and
  AST extension — deferred with that cost stated.
- Test items in comptime-instantiated types (§5.2) and a `@src()`-style
  intrinsic (§5.1): reserved as additive; scheduled when a real framework
  demands them.
- Comptime evaluation of pure test bodies ("compiling is testing"):
  deliberately not planned — it blurs compile failure with test failure;
  revisit with capability-inference evidence (§6).
- Workspace/multi-root invocation, and whether `rue test` without a root
  argument should discover one (needs the package-model discussion,
  ADR-0047's successor). Under a package model, the declared source set
  becomes a canonical candidate inventory, at which point rooting declared
  test files automatically becomes the natural revisit of the §1 discovery
  ruling (see Rejected alternatives).

## Future Work

The four deferred ADRs of §6 (RUE-1621 through RUE-1624), then: structured
assertion intrinsics beyond Phase 2.5's comparison family; capability
declarations in types and at trait boundaries (flagged from the capability
work); JUnit and CTRF adapters; test-aware `--watch` (rerun exactly the
dirtied selection on save — needs the deferred selection work plus the
existing watch loop).

## References

- rue-language/rue#2239: the full proposal this ADR was narrowed from —
  five review rounds of design capture on capability inference, verdict
  caching, determinism, scheduling, and protocol design (head commit
  `0a0e3884ae74`, retained on the closed PR).
- RUE-506 (design capture this ADR supersedes in mechanism), RUE-505,
  RUE-504, RUE-438; RUE-967 (pointer provenance split, gating the deferred
  capability work's `addr` leaf).
- ADR-0063 §1/§3/§8/§15; ADR-0061 §6 (schema versioning); ADR-0058;
  ADR-0055 (runtime ABI manifest); ADR-0064 (FFI); ADR-0038 (must-check
  linearity); ADR-0027 (random intrinsics); ADR-0025 (comptime); ADR-0069;
  ADR-0005 (preview features).
- docs/process/diagnostics.md (stream precedent),
  docs/spec/src/09-unchecked-code/ (the raw-intrinsic chokepoints),
  `crates/rue-test-runner` (execution mechanics), `crates/rue-runtime-abi`
  (the helper manifest).
- Prior art: cargo-nextest (and Rust RFC 3558's unstabilized libtest
  JSON); Go test caching and test2json; Zig test blocks and build-runner
  protocol (and ziglang/zig#15091); Swift Testing's event ABI history;
  Bazel Test Encyclopedia; Buck2's external test runner protocol; Deno
  permission scoping; Unison content-addressed test caching; Nim effect
  inference and `effectsOf` (RFC 404); CTRF.
