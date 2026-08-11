---
id: 0070
title: "rue test: agent-first test runner on the query graph"
status: proposal
tags: [tooling, testing, syntax, semantics, incremental, cli, language-shape]
feature-flag: test_declarations
created: 2026-08-11
accepted:
implemented:
spec-sections: []
superseded-by:
relates: ["RUE-506", "RUE-505", "RUE-504", "RUE-438", "ADR-0063", "ADR-0061", "ADR-0058", "ADR-0055", "ADR-0064", "ADR-0027", "ADR-0025", "ADR-0069"]
---

# ADR-0070: `rue test`: agent-first test runner on the query graph

## Status

Proposal. Drafted from the RUE-506 design capture, re-grounded against the
compiler as it exists after ADR-0063 (parallel demand-driven incremental
compilation, implemented) and ADR-0061 (supported facade, implemented). RUE-506
predates comptime generics, the `@syscall`-based std IO stack, C FFI, and the
query graph; this ADR replaces its aspirational mechanism sketches with
mechanisms the compiler now actually has, and sequences an MVP that ships
before the capability taxonomy is fine-grained.

Nothing here is ratified. The "Maintainer calls" section lists every decision
this document takes a position on that requires explicit sign-off, and the
"Spikes" section lists what must be measured or prototyped before its phase is
scheduled.

## Summary

Rue gets a first-class test runner: `test "name" { ... }` declarations in the
language, discovered and analyzed by the compiler as ordinary demand-driven
roots, executed by a `rue test` driver mode that emits a versioned NDJSON event
stream as its primary output, with human rendering as a consumer of that
stream. Hermeticity is a compiler-computed property: per-function capability
summaries are inferred bottom-up over the existing `BodyReferences`/reachability
query families, grounded in the three effect chokepoints the language already
has (the typed runtime-ABI helper manifest, `@syscall`, and `extern "C"`).
Verified-hermetic tests become cacheable build artifacts (skip-if-fingerprint-
unchanged) and support sound change-based selection; everything else runs
process-isolated, every time, with the runner making no claims it cannot
verify. The execution contract — isolation, independent lifecycle, per-test
timeout, output attribution, reproduction-as-data — is specified independently
of mechanism; the MVP mechanism is one linked test image per target plus one
process per test.

## Context

### What RUE-506 sketched

RUE-506 asks what a best-in-class, agent-first runner looks like designed from
scratch: functionally infinite parallelism, compiler-informed test selection,
machine-readable output first, verified (not documented) hermeticity via
capability sets over each test's transitive closure, execution as a contract
rather than a mechanism, a runner protocol so user-authored frameworks inherit
the infrastructure, and failure output as structured values rather than prose.

The issue also flags its own cost model: capability inference must be
per-function summaries computed bottom-up during normal compilation, cached and
cut off early when unchanged — not a per-test crawl of production bodies.

### What has changed since the issue was drafted

The sketch assumed most of the required infrastructure was hypothetical. It no
longer is:

- **The query graph exists.** ADR-0063 is implemented: revisioned typed
  queries, explicit observable root sets, per-body `BodyReferences`
  projections, a `Reachability(RootSetKey)` query family, red/green publication
  with early cutoff, per-function `CodegenUnit` terminal artifacts with content
  fingerprints, and a deterministic `ProgramImagePlan`. ADR-0063 §15 already
  names test selection as a planned consumer of exactly these queries.
- **Effects have chokepoints.** Every effectful operation a Rue program can
  perform flows through one of three doors: the closed, machine-validated
  46-helper runtime ABI manifest (ADR-0055, `rue-runtime-abi`); the `@syscall`
  intrinsic (legal only inside `checked {}`, spec §9.2); or `extern "C"` FFI
  (preview-gated, `checked`-only, ADR-0064). `std.fs`, `std.net`, and
  `std.exit` are pure Rue over `@syscall` and never touch the helper manifest —
  so the analysis must be interprocedural over reached bodies, but its leaves
  are exactly these three doors.
- **The call graph is static and total.** Rue today has no traits, no function
  pointers, no dynamic dispatch, and no threads. Generics are comptime
  functions returning types, fully monomorphized. RUE-506's "indirect calls are
  the real boundary" concern is currently vacuous: capability inference is
  sound and complete today, with FFI as the only opaque edge. The boundary
  problem returns the day traits or function values land, which is why the
  declaration surface is designed now (§4.5) even though inference needs no
  annotations yet.
- **Rue programs are already time-deterministic.** No clock API exists
  anywhere — not in the helper manifest, not in std. The only nondeterminism
  intrinsics are `@random_u32`/`@random_u64` (OS entropy, ADR-0027);
  `std.rand.Lcg` is a pure seeded PRNG. The "virtual clock by default" goal
  from RUE-506 costs nothing today and is instead a constraint on any future
  time API: it must be born behind a capability.
- **The failure model is abort-only.** Every trap, `@panic`, and `@assert`
  failure exits with status 101 and a pinned, machine-recognizable stderr
  message; there is no unwinding and no way to catch a failure in-process.
  This forces process-level isolation between tests for correctness, not just
  hygiene, until a batching mechanism that tolerates mid-batch aborts exists
  (§3.4).
- **A machine-output precedent exists.** `--error-format json` publishes
  diagnostics as NDJSON arrays on stderr with deterministic ordering
  (docs/process/diagnostics.md), and ADR-0061 §6 sets the schema policy:
  explicit major/minor versions, additive minors, unknown majors rejected.
  The diagnostics stream itself carries no version field; the test event
  stream will not repeat that gap.
- **The compiler's own harness is a working reference.** `rue-test-runner`
  (the Rust crate driving spec/UI/CLI suites) already implements process-group
  spawning, SIGKILL-on-timeout with drained reader threads, ICE detection as a
  failure class that xfail markers cannot absorb, `known_bug` markers whose
  unexpected pass fails loudly, platform scoping, and the "an empty filter
  selection is an error, not a pass" principle. These behaviors are adopted
  here as contract, not reinvented.

### Prior art, compressed to the lessons taken

- **cargo-nextest**: process-per-test is the right *contract* when the runner
  cannot introspect tests; its structural losses (no doctests, in-process
  fixtures broken, output format reverse-engineered from libtest) all stem
  from bolting onto a language it does not own. Steal: per-test timeouts,
  retries as policy, partitioned sharding, test groups for shared resources.
  Avoid: making the process the *definition* of a test rather than one
  execution strategy.
- **Go**: test result caching keyed on build inputs plus an observed log of
  files and env vars the test actually touched. The observation is best-effort
  — network and time are invisible to it — so the cache is unsound in exactly
  the corners users get burned by. Steal: cache-as-default UX, `test2json` as
  stream-first output. Avoid: observed hermeticity; Rue verifies it statically
  instead.
- **Zig**: `test "name" { }` blocks as language items, discovered by the
  compiler; the build runner drives the test binary over a stdin/stdout binary
  protocol (execute-by-index, structured results); inferred error sets are the
  direct precedent for bottom-up per-function summaries with early cutoff.
  This is the closest existing shape to what Rue wants; Rue adds verified
  capabilities, caching, and a stable public event schema on top.
- **Swift Testing**: traits (`.serialized`, `.timeLimit`, tags) as declarative
  per-test metadata; a versioned JSON event stream as the tool-integration
  ABI. Steal: metadata-on-the-declaration and the versioned-stream posture.
  Its in-process parallelism model depends on catchable failures and does not
  transfer to Rue's abort-only runtime.
- **Bazel**: the Test Encyclopedia states the hermeticity ideal — declared
  inputs only, `TEST_TMPDIR`, pinned env — but explicitly does not enforce it,
  and its caching is target-granular. Steal: the environment contract and
  result-caching semantics. Rue's point of departure: enforcement, at item
  granularity.
- **Buck2 / tpx**: tests handed to an external runner through an explicit
  protocol boundary — the runner is a client of the build system, not a
  subroutine. Validates RUE-506's "protocol, not a trait" conclusion, which
  Rust's stalled `custom_test_frameworks` confirms from the failure side.
- **Deno**: runtime-enforced per-test permission grants (`--allow-read=path`
  granularity) show capability-aware testing is usable in practice, and that
  coarse capabilities with path/host refinement is the granularity users can
  actually author. Rue's enforcement is static rather than runtime, but the
  taxonomy lesson carries.
- **Effect systems** (Koka, Pony, Austral, WASI): fine-grained declared effect
  taxonomies impose an authoring tax that has kept them niche; coarse inferred
  summaries with declaration only at genuinely opaque boundaries is the
  adoptable point in the design space. That is the shape chosen here.

## Scope

In scope: test declarations in the language; compiler discovery, analysis, and
capability summaries; the `rue test` driver mode; the event stream schema
posture; execution, caching, selection, and scheduling; the phased plan.

Out of scope, deliberately: the doctest mechanism (needs RUE-504's doc model),
the user-authored-framework protocol's wire details (needs RUE-505's semantic
API decisions; only the seam is reserved here), benchmark/property/fuzz
frameworks themselves, a package/workspace model (the runner takes a root
module exactly like the compiler), and any promise about a persistent
cross-process memo database (ADR-0063 future work; the test verdict cache in
§5 deliberately does not depend on it).

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

- **Grammar**: `test_item = directives "test" STRING block ;` at item
  position, alongside `function | struct_def | enum_def | drop_fn |
  const_decl`. `test` is a contextual keyword (an item-position `test`
  followed by a string literal), so existing identifiers named `test` —
  including `fn test()` — do not break.
- **Naming and identity**: the string is the test's name and must be unique
  within its module (duplicate names are a semantic error). The stable test ID
  is the module's canonical identity plus the name, under ADR-0063 §5's stable
  identity domain — insensitive to reordering, whitespace, and unrelated
  edits. The exact rendered ID spelling is pinned in the schema doc that ships
  with Phase 2.
- **Body typing**: the block has type `()`. A test passes when its process
  exits 0 and fails when it traps (`@assert`, `@panic`, bounds, overflow,
  division), exits nonzero, or is killed. Result-typed test bodies (so `?`
  works directly in tests) are an open question (§Open Questions), not v1.
- **Placement is the visibility model.** A test item lives in a module and
  sees exactly what that module's other items see: tests written next to the
  code exercise private items with no visibility loosening; tests written in a
  separate importing module exercise the public API and prove its
  sufficiency. No `pub` changes for testability, no special access grants, no
  test-only visibility syntax. This resolves RUE-506's visibility question
  structurally: the question "internal or contract test?" is answered by where
  the file puts it, and the answer is visible in the test's module path.
- **Test items are roots, not reachable code.** An executable request never
  roots test items; under ADR-0063's demand-driven model their bodies are not
  semantically analyzed, code-generated, or linked into executables — tests
  in production files cost production builds nothing but parse time. A test
  request roots every test item declared in the root module's transitive
  `@import` closure (not merely modules reachable from `main` — a module
  imported only for its tests still contributes them). Unused-item warnings
  are computed against the union of the request's roots, so helpers used only
  by tests do not warn in test requests and do not silently rot; whether they
  warn in plain executable requests follows existing warning semantics and is
  noted as an implementation detail for Phase 1.
- **Preview gate**: `test_declarations` (the `test_infra` flag name is already
  taken by compiler self-test machinery). Declaring a test item without the
  preview enabled is the standard `require_preview()` error. `rue test`
  enables the gate implicitly while the feature is in preview.

### 2. `rue test` is a driver mode emitting a versioned event stream

The driver grows its first subcommand:

```
rue test <root.rue> [--list] [--filter <pattern>]... [--format human|json]
         [--jobs N] [--timeout-ms N] [--shard K/N] [--target <t>] [-O<n>]
         [--seed N] [--no-cache] [--changed-only] [--keep-going] ...
```

- **Dispatch rule**: when the first non-flag argument is exactly `test`, the
  driver enters test mode; a root source literally named `test` must be
  spelled `./test`. All existing compile-mode flags that make sense in test
  mode (`--target`, `--preview`, `-O`, `--source-manifest`, `--link-archive`,
  `--error-format`, `-j`, logging) keep their spellings and semantics.
- **Streams**: compiler diagnostics remain on stderr exactly as today
  (`--error-format json` unchanged and orthogonal). Test events are the
  runner's own surface on stdout: with `--format json`, one JSON object per
  line (NDJSON). Unlike the diagnostics stream, the event stream carries an
  explicit schema version in its head event, per ADR-0061 §6 — the
  diagnostics stream's missing version field is the outlier, not the model.
  Events are produced from session artifact views, never scraped from human
  output (ADR-0061's RUE-439 rule applies verbatim).
- **Event kinds** (schema doc ships with Phase 2; sketch, not normative):
  `run_started` (schema version, root, target, plan summary, seed),
  `test_finished` (stable ID, verdict, duration, capability summary, failure
  structure, captured stdout/stderr, exact reproduction argv), `run_finished`
  (counts, wall time, cache statistics). Verdicts: `pass`, `fail`, `timeout`,
  `crash` (killed by signal), `compile_error`, `skipped`, `cached_pass`. A
  failure record is data: failure kind (`assert` / `trap:<class>` / `exit` /
  `signal` / `timeout` / `ice`), the pinned runtime message (the abort-only
  runtime's fixed stderr strings are machine-recognizable by construction),
  exit code or signal, and source span of the test declaration. Richer
  expected/actual payloads arrive with structured assertion intrinsics
  (§Future Work), not by parsing prose.
- **Asymmetric verbosity**: the default human renderer prints failures in
  full — structured failure, captured output, repro line — and passes as a
  count. No wall of green. The human renderer is implemented as a consumer of
  the same event stream it would emit under `--format json`, which keeps the
  machine surface honest.
- **Discovery without execution**: `rue test <root> --list --format json`
  emits the inventory — IDs, declaration spans, capability summaries, cache
  status — without running anything. This is the "what tests exist for X"
  query surface of RUE-506, and it must stay cheap: listing performs semantic
  analysis of test closures but no codegen, no linking, no execution.
- **Filtering**: `--filter` matches against test IDs (module path and name);
  repeated filters union. A filter selecting zero tests is an error with a
  distinct exit code, adopting the spec-suite principle that an empty
  selection is how a typo becomes false evidence.
- **Exit codes** (proposed): `0` all selected tests passed (cached passes
  count), `1` at least one test failed, `2` compilation or runner error,
  `3` empty selection.
- **Sharding**: `--shard K/N` partitions the selected set deterministically by
  stable ID hash. Duration-aware bin-packing is explicitly deferred until the
  runner has recorded-duration history to pack with (RUE-506 Q7: v1 is
  deterministic partitioning, not a scheduler).

### 3. Execution is a contract; the MVP mechanism is a test image plus a process per test

The contract every mechanism must honor, current and future: each test
observes a fresh, isolated execution; has an independent lifecycle (start,
kill, timeout) enforceable by the runner; gets its stdout/stderr captured and
attributed exactly; and its failure cannot corrupt, mask, or abort any other
test's result.

The MVP mechanism:

- **One test image per target.** The compiler synthesizes a dispatcher `main`
  (an ordinary generated Rue function, not a runtime feature): it reads a test
  selector from argv and invokes exactly one test body. The image links every
  selected test's `CodegenUnit` closure through the ordinary
  `ProgramImagePlan` path — per-function codegen artifacts are shared with
  regular builds and across test runs by the existing memo database, so the
  marginal cost of the image is one link, not N compiles.
- **One process per test invocation.** The runner spawns `image --run <id>`
  per test, in its own process group, with a per-test wall-clock timeout
  (default 10 s, matching `rue-test-runner`), SIGKILL to the group on expiry,
  and reader threads draining both pipes (the mechanics `rue-test-runner`
  already has; Phase 2 extracts and reuses them rather than reimplementing).
  Timeout kills, signal deaths (including SIGPIPE's status 141), and exit 101
  with a pinned trap message are distinguished in the verdict, and ICE
  detection remains a separate failure class no marker can absorb.
- **Execution environment contract** (Bazel's encyclopedia, enforced by
  construction where possible): pinned minimal environment (a fixed allowlist
  plus runner-set variables; the pin set is part of the cache key), a fresh
  private working directory per test, deleted on pass and retained on failure
  for post-mortem, and a `RUE_TEST_*` namespace reserved for runner-provided
  variables (seed, scratch dir).
- **Parallelism**: the runner schedules up to `--jobs` test processes
  concurrently. Because isolation is process-level and hermetic tests are
  verified non-interfering (§4), the default is full parallelism; tests with
  unverified capabilities still run in parallel by default in the MVP —
  process isolation plus private scratch directories is already stronger than
  every mainstream runner — with serialization arriving as declared groups in
  Phase 5, and only then inference-driven.
- **Reproduction as data**: every failure event carries the exact argv to
  reproduce that single test under the same seed, target, opt level, and
  filter — copy-paste (or agent-invoke) ready.
- **Per-test compile failure is a verdict, not a run abort** (Phase 4).
  Because each test's closure is analyzed independently as a root, a semantic
  error in one test's closure can yield a `compile_error` verdict carrying
  those diagnostics while every other test still builds into the image and
  runs. The MVP keeps the simpler whole-run-fails behavior; the per-test
  contract is specified now so nothing in Phase 2 bakes in the coarser one.
- **Future mechanisms behind the same contract** (Phase 5+, gated on the
  batching spike): batching many verified-hermetic tests into one process with
  fork-per-test or rerun-on-abort recovery; comptime evaluation of pure test
  bodies remains listed under Open Questions, not planned.

### 4. Capability summaries: inferred bottom-up, grounded in the three doors

#### 4.1 The lattice

A capability summary is a bitset over a deliberately coarse taxonomy:

| Capability | Leaves that introduce it |
| --- | --- |
| `stdout` / `stderr` / `stdin` | print/println/dbg/read_line helper family |
| `random` | `@random_u32` / `@random_u64` (helpers `RandomU32`/`RandomU64`) |
| `env` | `@env_count` / `@env_ptr` / `@env_len` helpers |
| `args` | `@arg_count` / `@arg_ptr` / `@arg_len` helpers |
| `exit` | `__rue_exit` helper |
| `syscall` | any `@syscall` site (subsumes fs, net, clock, process, ...) |
| `ffi` | any `extern "C"` call |

Allocation, traps, and the pure helper family (string ops, parsing, memcpy)
introduce no capability: they are deterministic, process-local, and observable
only through the test's own verdict and captured output. `hermetic` is not a
bit; it is the derived predicate "no `syscall`, no `ffi`, no `random`"
(`stdio`, `env`, and `args` are compatible with hermeticity because the runner
pins and captures them — they are deterministic given the runner's controlled
inputs, and those controls are part of the cache key).

`syscall` is intentionally the coarse top of the OS hierarchy in v1. Splitting
it into `fs` / `net` / `clock` / `process` requires classifying syscall
numbers at comptime-constant `@syscall` sites (std.fs/std.net select numbers
via constant-foldable `@target_arch()`/`@target_os()` matches, so this is
plausible) with any non-constant number widening to full `syscall`; that
refinement is Phase 6, gated on a spike, and nothing before it depends on the
finer partition.

#### 4.2 The computation

`EffectSummary(FunctionInstanceKey)` is a new query family on the ADR-0063
graph, not a peer analysis: for each reached function instance, the summary is
the join of its directly used leaves (helper references are visible in the
body's resolved references against the typed ABI manifest; `@syscall` and
foreign calls are visible in AIR) with the summaries of its
`BodyReferences`-resolved callees. Ordinary call recursion is a legal cycle in
reachability and resolves to a least fixed point over the bitset join —
bounded, monotone, and cheap. Summaries are canonical artifacts with terminal
fingerprints: editing a body recomputes one summary, and red/green cutoff
stops propagation when the bitset is unchanged — the common case, exactly the
inferred-error-set economics RUE-506 predicted. Dependency summaries shipped
in library metadata (RUE-506's concern) are moot until Rue has separate
compilation; whole-program analysis sees every body today.

A test's capability set is the summary of its body instance. It appears in
`--list` output and on every test event — the analysis is visible from day
one of Phase 3, before anything acts on it.

#### 4.3 Soundness posture

The summary must be sound (never claims a capability absent that the test can
exercise): `@syscall` with any arguments is full `syscall`; any FFI call is
full `ffi`; there are no indirect calls to lose track of. If the analysis
cannot see a body (which today can only mean FFI), the edge is opaque-top. The
runner's caching and selection privileges are extended only to tests whose
summaries are closed under this conservatism — a test that is `syscall` or
`ffi` is simply always executed. Unsoundness ejects; it never degrades.

#### 4.4 Not a type-system feature (yet)

Capability summaries are compiler-internal analysis artifacts surfaced through
tooling, like warnings — they do not appear in the type system, in function
signatures, or in the spec's semantic rules, and programs cannot observe them.
This defers RUE-506's open question 1 ("test infrastructure or language
feature?") without foreclosing it: if capabilities later become part of
function types or trait obligations, the inference machinery and taxonomy
transfer, and the spec work starts from measured experience. Taking the
type-system step is a separate future ADR by design.

#### 4.5 The declaration surface is reserved, not required

Today nothing needs annotation. The day traits, function-typed values, or
dynamic dispatch land, their call edges stop being statically resolvable and
must carry declared capability bounds at the abstraction boundary — the
directive namespace reserves `@requires(<capability>...)` for that role (on
`extern` blocks immediately, on trait members when traits exist). FFI is the
first user: an `extern "C"` block may declare `@requires(fs)` to narrow its
summary from opaque-top, on the author's honor, which is exactly the trust
level FFI already has for memory safety (`checked` blocks, ADR-0064). Until
declared narrowing ships (Phase 6), FFI is simply top.

### 5. Hermetic verdicts are cacheable artifacts; selection is a consequence

- **The verdict cache** stores, per stable test ID: the closure fingerprint it
  passed under, and the verdict metadata (duration, captured-output digest).
  The closure fingerprint covers the test body's reached artifact fingerprints
  (ADR-0063 terminal fingerprints over the canonical closure), the compiler's
  own build identity, target, opt level, the runner's pinned environment set,
  seed policy, and the link-relevant inputs (`--link-archive` contents). A
  test is skipped as `cached_pass` only when it is verified hermetic and its
  fingerprint is unchanged. Failures are never cached. `--no-cache` forces
  execution. The cache is a small content-addressed file the runner owns —
  Go-style results caching — deliberately independent of ADR-0063's future
  persistent memo database: when that lands the fingerprints get cheaper to
  produce, but the verdict cache's soundness story does not change.
- **Selection** (`--changed-only`) is the same predicate used negatively:
  select the tests whose closure fingerprint differs from the cache. Because
  fingerprints ride the demand-driven graph, an edit to one function dirties
  exactly the tests that reach it — item-granular, sound for hermetic tests by
  construction. Non-hermetic tests are always selected (their inputs are not
  fully fingerprinted, so "unaffected" cannot be proven). This is RUE-506's
  "sound test selection" delivered as a cache-diff, with no second dependency
  tracker.
- **Flakiness is localized by construction**: a verified-hermetic test cannot
  be flaky through any channel the OS offers except resource exhaustion; when
  a hermetic test's verdict differs across runs of the same fingerprint, the
  runner reports it as an infrastructure or compiler-determinism defect, not a
  test defect (and the reproducibility harness's byte-identical-artifact
  guarantees make that report actionable). Rerun-based flake detection
  (`--reruns N`) is offered only for non-hermetic tests, aimed exactly where
  nondeterminism can live.

### 6. Determinism defaults

- The runner pins the child environment to a fixed allowlist plus
  `RUE_TEST_*`; `env`-capability tests are therefore deterministic given the
  pin set, which is in the cache key.
- There is no clock to virtualize; the absence is load-bearing. Any future
  time API must arrive behind a `clock` capability so this ADR's guarantees
  survive it (recorded as a standing constraint, not enforced by this ADR).
- `--seed N` is accepted and reported in `run_started` and in every failure's
  repro argv from Phase 2, but in the MVP it only feeds the runner's own
  choices (shuffle order, scratch naming). Making `@random_*` seedable in test
  builds — lowering to a seeded PRNG per test process — is a runtime/codegen
  change requiring a maintainer call (it makes `random`-capability tests
  deterministic and hence cacheable, at the cost of test-mode divergence from
  production entropy).
- Test execution order within a run is shuffled by seed by default (verified
  isolation makes order dependence impossible for hermetic tests; shuffling
  keeps everyone else honest, and the seed makes any surprise reproducible).

### 7. The protocol seam for user-authored frameworks is reserved

The built-in runner speaks to test images through an argv/exit-code/stream
contract that Phase 2 defines and documents. Phase 7 promotes a versioned
subset of that contract to a public protocol — enumerate (IDs, metadata,
capabilities) and execute-by-ID with structured results — so property testers,
snapshot harnesses, and fuzzers can present as test providers and inherit
scheduling, caching, filtering, and reporting. Its wire format must be decided
together with RUE-505's semantic-API format policy; committing to it now would
prejudge that discussion, so this ADR reserves the seam (nothing in Phases 1–6
may depend on the dispatcher protocol staying private) and defers the wire
contract. Custom reporters need no protocol at all: they consume the NDJSON
stream from day one.

## Implementation Phases

Linear issues to be filed on acceptance (epic + one per phase, IDs recorded
here per docs/designs/README.md).

- [ ] **Phase 1: `test` declarations** - RUE-TBD. Grammar/lexer/parser
      (contextual keyword, directives allowed), RIR item, semantic analysis as
      ordinary bodies rooted only by test requests, duplicate-name diagnostics,
      `test_declarations` preview feature, spec sections + spec-test coverage,
      UI coverage for the gate and diagnostics. No runner yet: `--emit`-level
      verification that test items parse, analyze, and are invisible to
      executable requests.
- [ ] **Phase 2: `rue test` MVP runner** - RUE-TBD. Driver subcommand dispatch;
      test-request root sets; synthesized dispatcher `main` and per-target test
      image through `ProgramImagePlan`; process-per-test execution with
      process-group timeout/kill and output capture (mechanics shared with
      `rue-test-runner`); `--list`, `--filter`, `--jobs`, `--shard`,
      `--timeout-ms`, `--seed` (shuffle), exit-code contract; NDJSON event
      stream v1.0 with schema doc (docs/process/test-events.md) and the human
      renderer as its consumer; repro argv in every failure; CLI-suite coverage
      end to end. **This phase is the MVP: usable, agent-first, zero capability
      claims — every test simply runs.**
- [ ] **Phase 3: `EffectSummary` query family** - RUE-TBD. Bottom-up bitset
      summaries over `BodyReferences` with red/green cutoff; leaves = helper
      manifest classification, `@syscall`, FFI; least-fixed-point over
      recursion; summaries surfaced in `--list` and `test_finished` events;
      determinism and cutoff behavior pinned by compiler unit tests and an
      edit-scenario measurement (ADR-0068 harness) proving near-zero warm
      cost. No scheduling or caching behavior change.
- [ ] **Phase 4: verdict cache and selection** - RUE-TBD. Closure fingerprints
      for test roots; on-disk verdict cache with documented key composition;
      `cached_pass`, `--no-cache`, `--changed-only`; hermetic-only gating with
      eject-on-unknown; per-test `compile_error` verdicts (error-tolerant test
      images); cache-soundness audit checklist executed against the spike
      findings.
- [ ] **Phase 5: scheduling and flake policy** - RUE-TBD. Declared serial
      groups (`@group("name")` directive) honored by the scheduler;
      `--reruns N` for non-hermetic tests with flake reporting;
      hermetic-mismatch reporting as infrastructure defect; recorded durations
      feeding `--shard` bin-packing.
- [ ] **Phase 6: capability refinement** - RUE-TBD. Syscall-number
      classification at comptime-constant sites splitting `syscall` into
      `fs`/`net`/`clock`/`process` (gated on the spike); `@requires(...)`
      declared narrowing on `extern "C"` blocks; taxonomy review against real
      usage before any further splitting.
- [ ] **Phase 7: the public protocol** - RUE-TBD. Versioned
      enumerate/execute-by-ID protocol aligned with RUE-505's format
      decisions; external providers; JUnit/CI adapter as a reference stream
      consumer.

Each phase is independently shippable; Phases 3–7 can reorder behind 2 if
priorities shift, except that 4 requires 3 and 6 requires 3.

## Consequences

### Positive

- An MVP (Phases 1–2) needs no capability system, no new persistence, and no
  spec-invasive machinery — it is a subcommand, a grammar item, a generated
  `main`, and a schema doc, on infrastructure that exists and is tested.
- Hermeticity claims are verified or absent; the runner never promises what
  the compiler cannot prove. Caching and selection are sound by construction
  rather than by convention (Bazel) or observation (Go).
- Agents get structure end to end: discovery without execution, failures as
  data with spans and repro argv, asymmetric verbosity, stable IDs, versioned
  schema — no reverse-engineering of prose at any layer.
- The design exploits what is genuinely unusual about Rue today — total static
  call graph, closed effect chokepoints, no clock, abort-only failures,
  fingerprinted demand-driven artifacts — instead of importing the
  compensating machinery other ecosystems needed.
- The capability lattice, declaration directive, and event schema are all
  forward-designed for the features that will complicate them (traits,
  function values, time APIs, separate compilation), so none of those arrive
  as retrofits.

### Negative

- One more consumer-visible versioned surface (test events) to maintain under
  ADR-0061 §6 discipline, plus a schema doc, plus CLI cases pinning it.
- Process-per-test puts a floor under per-test latency; "thousands of tests
  per second" claims wait for Phase 5+ batching, which the abort-only runtime
  makes genuinely hard (rerun-on-abort or fork tricks, each with costs).
- The coarse `syscall` bit makes every fs-touching test uncacheable until
  Phase 6, and `std.fs`/`std.net` usage is common in the example corpus —
  the MVP's cache hit rate on real projects will be modest until refinement
  lands.
- A generated dispatcher `main` and test-image link per target adds a new
  compiler-synthesized artifact to maintain across both backends.
- Contextual-keyword parsing for `test` adds grammar subtlety (mitigated by
  its restriction to item position followed by a string literal).

### Neutral

- `scripts/rue test` (the maintainers' compiler-suite wrapper) and the
  user-facing `rue test` become homonyms; docs and AGENTS.md references need a
  disambiguation pass regardless of which rename option is taken.
- Tests-next-to-code means production source files carry test text; parse cost
  is whole-file already, and demand-driven analysis makes the semantic cost
  zero for executable requests.

## Open Questions

### Maintainer calls needed (decisions this ADR takes a position on)

- **Test declaration surface**: `test "name" { }` blocks (recommended here,
  contextual keyword) vs `@test`-directive on ordinary functions vs `test fn
  name()`. Blocks match the language's Zig-adjacent shape and make the name a
  human sentence; a directive avoids any keyword question. Call needed before
  Phase 1.
- **Analysis-only capabilities**: ratify that capability summaries stay out of
  the type system and spec semantics for this ADR (§4.4), with the type-system
  question explicitly deferred to a future ADR. This is the RUE-506 open
  question 1 disposition and the highest-stakes call here — it is cheap now
  and expensive to reverse if trait design later assumes untyped effects.
- **Result-typed test bodies**: `()`-only (recommended for v1) vs allowing
  `Result`-typed bodies so `?` works in tests directly. Interacts with trusted
  producer rules for `?` (spec 4.15:3).
- **Seedable `@random_*` under test builds** (§6): runtime divergence between
  test and production builds in exchange for determinism and cacheability of
  `random` tests. Not needed for MVP; call needed before Phase 5 flake policy
  treats `random` as permanently nondeterministic.
- **Exit-code and `@assert` stabilization**: promote `@assert`/`@panic` from
  the reserved intrinsic bucket (4.13:5b) to normative, and decide whether
  assertion failure keeps exit 101 (shared with all traps, distinguished by
  pinned message — recommended, no runtime change) or gets a distinct code.
- **The `scripts/rue test` homonym**: rename the maintainer wrapper subcommand
  (e.g. `scripts/rue suite`), or accept the context distinction and fix docs.
- **Exit-code contract of `rue test`** (§2): the 0/1/2/3 proposal, in
  particular empty-selection-as-error.
- **Naming**: `test_declarations` preview flag; `@group`/`@requires` directive
  spellings; `docs/process/test-events.md` as the schema doc home.

### Questions that need a spike before their phase is scheduled

- **Syscall-number classification coverage** (before Phase 6): over
  `std/fs.rue` and `std/net.rue` on all three targets, what fraction of
  `@syscall` sites have comptime-constant numbers under the existing
  const-evaluation machinery, and does the classification survive the macOS
  carry-flag errno rework that std.fs needs anyway? Output: a table of sites
  vs classifiability, and a decision on whether `clock` can be carved out
  reliably enough to matter.
- **Process-spawn throughput baseline** (before Phase 5 is prioritized):
  measured per-test overhead of spawn/run/reap for a trivial test on all three
  targets at realistic `--jobs`, to size how much batching is actually worth
  and where the crossover is. Static no-interpreter executables should make
  this cheap; measure, don't assume.
- **Abort-tolerant batching mechanism** (before Phase 5 commits to one):
  fork-per-test from a warm image (no threads exist, which makes fork
  unusually safe for Rue; macOS behavior needs validation) vs
  rerun-remaining-after-abort batches vs staying process-per-test. Includes
  whether `ProgramImagePlan` can cheaply emit per-shard images instead.
- **Verdict-cache key audit** (during Phase 4, before enabling by default):
  enumerate every input that can affect a hermetic test's outcome (compiler
  build identity, target, opt level, seed policy, env pin set, link archives,
  runner version, image layout?) and pin each as in-key, irrelevant-by-proof,
  or eject-to-uncacheable. The reproducibility harness's perturbation list is
  the starting checklist.
- **Memo-database pressure under test roots** (during Phase 2): a test request
  roots a strict superset of `main`'s closure; validate the ADR-0063 §14
  retention budgets against check-all-shaped root sets on the larger example
  corpora.

### Deferred design questions

- Comptime evaluation of pure test bodies ("compiling is testing") — RUE-506
  Q4. Deliberately not planned: it blurs compile failure with test failure and
  its incremental-cost story inside the query graph is unstudied. Revisit with
  evidence from Phase 3 summaries about how many tests are comptime-eligible.
- Doctests: examples in docs testable by construction (RUE-504 coordination);
  the protocol seam (§7) is where a doc-example provider would plug in.
- Per-test timeout/skip/xfail metadata (`@timeout(ms)`, `@skip`,
  `@known_bug("RUE-NN")` with XPASS-fails-loudly semantics inherited from the
  compiler's own suites): wanted, directive syntax fits, deferred to keep
  Phase 1 grammar minimal.
- Workspace/multi-root invocation, and whether `rue test` without a root
  argument should discover one (needs the package-model discussion, ADR-0047's
  successor).

## Future Work

Structured assertion intrinsics (`@assert_eq` and friends emitting
expected/actual as data through a runtime channel the event stream can carry —
today's `@assert` gives only a boolean and a fixed message); the public
provider protocol's wire format with RUE-505; capability declarations in types
and at trait boundaries (the future ADR flagged in §4.4); seeded-entropy test
profile; duration-aware sharding; JUnit and CI-surface adapters; test-aware
`--watch` (rerun exactly the dirtied selection on save — the pieces are
Phase 4 selection plus the existing watch loop).

## References

- RUE-506 (design capture this ADR supersedes in mechanism), RUE-505, RUE-504,
  RUE-438 (machine-readable interface project).
- ADR-0063 §1/§3/§8/§15 (roots, fingerprints, reachability, test-selection
  consumer); ADR-0061 §6 (schema versioning policy); ADR-0058 (canonical
  artifacts); ADR-0055 (typed runtime ABI manifest); ADR-0064 (FFI boundary
  rules); ADR-0027 (random intrinsics); ADR-0025 (comptime); ADR-0069
  (CI tiers expecting a test runner).
- docs/process/diagnostics.md (stream precedent), docs/spec/src/09-unchecked-code/
  (the `checked` chokepoints), `crates/rue-test-runner` (execution mechanics),
  `crates/rue-runtime-abi` (the helper manifest).
- Prior art: cargo-nextest; Go test caching and test2json; Zig test blocks and
  build-runner protocol; Swift Testing traits and event ABI; Bazel Test
  Encyclopedia; Buck2 external test runner protocol; Deno permissions.
