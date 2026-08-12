---
id: 0071
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

# ADR-0071: `rue test`: agent-first test runner on the query graph

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
summaries are inferred bottom-up over projections of the existing
`BodyReferences`/reachability
query families, grounded in the three effect chokepoints the language already
has (the typed runtime-ABI helper manifest, `@syscall`, and `extern "C"`).
Verified-hermetic tests become cacheable build artifacts (skip-if-fingerprint-
unchanged) and support sound change-based selection; everything else runs
process-isolated, every time, with the runner making no claims it cannot
verify. The execution contract — isolation, independent lifecycle, per-test
timeout, output attribution, reproduction-as-data — is specified independently
of mechanism; the MVP mechanism is one linked test image per target plus one
process per test. Extensibility is tiered with no privileged built-ins: the
structured failure channel, test identity, and runner contracts are all
protocols that user-authored assertion libraries, frameworks, and runners can
speak. A standing section records the obligations this design places on
future language evolution — traits and dynamic dispatch, std and ABI growth,
concurrency, separate compilation, comptime inputs, failure-model changes —
all of which degrade to "affected tests always run," never to unsoundness.

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
  projections, a body-reachability query family over closure keys, red/green
  publication with early cutoff, per-function `CodegenUnit` terminal artifacts
  with content fingerprints, and a deterministic image plan (query names here
  and below are conceptual in ADR-0063 §6's sense; exact Rust names differ).
  ADR-0063 §15 already names test selection as a planned consumer of exactly
  these queries.
- **Effects have chokepoints.** Every effectful operation a Rue program can
  perform flows through one of three doors: the closed, machine-validated
  46-helper runtime ABI manifest (ADR-0055, `rue-runtime-abi`); the `@syscall`
  intrinsic; or `extern "C"` foreign calls (ADR-0064 — accepted, preview-gated,
  in progress). The `checked` boundary is *not* an effect proxy and the
  analysis never relies on it: std wraps its `checked` blocks inside
  ordinary safe functions, so callers never write `checked`, and effect
  leaves are extracted from analyzed bodies wherever they appear. The effectful operations of `std.fs`,
  `std.net`, and `std.exit` are pure Rue over `@syscall` whose I/O bypasses
  the helper manifest entirely (they still allocate through it) — so the
  analysis must be interprocedural over reached bodies, but its effect
  leaves are exactly these three doors.
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
- **The compiler's own suites are a working reference.** The shared
  `rue-test-runner` crate implements process-group spawning,
  SIGKILL-on-timeout with drained reader threads, ICE detection as a failure
  class, and platform scoping; the `known_bug` xfail-with-loud-XPASS
  semantics live in the oracle and CLI harnesses built on it; and the "an
  empty filter selection is an error, not a pass" principle is `rue-spec`'s
  own layer (RUE-1161) — the shared crate deliberately lets a zero-match
  user filter pass and fails only on an empty corpus. These behaviors are
  adopted here as contract, with those attributions; signal-death verdicts
  (e.g. SIGPIPE's status 141) are net-new runner work, not inherited.

### Prior art, compressed to the lessons taken

- **cargo-nextest**: process-per-test is the right *contract* when the runner
  cannot introspect tests; its structural losses (no doctests, in-process
  fixtures broken) stem from bolting onto a language it does not own.
  Reverse-engineering libtest output is its origin story, not its present:
  nextest now ships a stable machine-readable list format and an
  experimental run format of its own, while upstream libtest JSON (RFC 3558)
  remains unstable as of mid-2026 — evidence that the stream must be owned
  and designed by the runner, first. Steal: per-test timeouts, retries as
  policy, partitioned sharding, test groups for shared resources. Avoid:
  making the process the *definition* of a test rather than one execution
  strategy.
- **Go**: test result caching keyed on build inputs plus an observed log of
  files and env vars the test actually touched, at package granularity. The
  observation is best-effort — network and time are invisible to it — so the
  cache is unsound in exactly the corners users get burned by. Steal:
  cache-as-default UX, `test2json` as stream-first output. Avoid: observed
  hermeticity; Rue verifies statically, at item granularity.
- **Zig**: `test "name" { }` blocks as language items, discovered by the
  compiler; the build runner drives the test binary over a stdin/stdout binary
  protocol (execute-by-index, structured results); inferred error sets are the
  direct precedent for bottom-up per-function summaries with early cutoff.
  This is the closest existing shape to what Rue wants, and its two gaps are
  instructive: the protocol has never been a documented public contract, and
  it shares the child's stdout, so user writes can corrupt it (ziglang/zig
  #15091). Rue commits to a documented contract from Phase 2 and keeps the
  event stream on the runner's own stdout, with tests in their own
  processes.
- **Swift Testing**: traits (`.serialized`, `.timeLimit`, tags) as declarative
  per-test metadata; a versioned JSON event stream as the tool-integration
  ABI — whose own history argues for rich v1 events: tags and time limits
  were omitted from ABI v0 and had to be pitched back in after third-party
  tool pain. Steal: metadata-on-the-declaration, the versioned-stream
  posture, and capability/identity metadata in v1 events rather than
  retrofitted. Its in-process parallelism model depends on catchable
  failures and does not transfer to Rue's abort-only runtime.
- **Bazel**: the Test Encyclopedia states the hermeticity ideal — declared
  inputs only, `TEST_TMPDIR`, pinned env — but explicitly does not enforce it,
  and its caching is target-granular. Steal: the environment contract and
  result-caching semantics. Rue's point of departure: enforcement, at item
  granularity.
- **Buck2 / tpx**: tests handed to an external runner through an explicit
  protocol boundary — the runner is a client of the build system, not a
  subroutine. Validates RUE-506's "protocol, not a trait" conclusion, which
  Rust's stalled `custom_test_frameworks` confirms from the failure side.
- **Deno**: runtime-enforced per-test permission *scoping* — a test's grants
  can only narrow the process-level grant, never widen it
  (`--allow-read=path` granularity) — shows capability-aware testing is
  usable in practice, and that coarse capabilities with path/host refinement
  is the granularity users can actually author. Rue's enforcement is static
  rather than runtime, but the taxonomy lesson carries.
- **Unison**: the standing prior art for content-addressed test caching —
  pure tests are cached against the hash of the test's dependency graph and
  re-run only when a dependency's hash changes; IO-typed tests are excluded.
  §5 of this ADR is that idea made granular and *inferred*: Unison's purity
  comes from a type-system ability annotation (the authoring tax the next
  bullet describes), and it has no isolation contract, verdict taxonomy,
  event stream, or change-based selection. The contribution here is
  hermeticity inference in an effect-unannotated language plus the contract
  around the cache — not the cached-verdict idea itself.
- **Effect systems** (Koka, Pony, Austral, WASI — and Nim): fine-grained
  declared effect taxonomies impose an authoring tax that has kept them
  niche; coarse inferred summaries with declaration only at genuinely opaque
  boundaries is the adoptable point in the design space. Nim is the
  existence proof at compiler scale — zero-annotation bottom-up effect
  inference, with `effectsOf` (its RFC 404) as a ready-made shape for
  effect-polymorphic function parameters when Rue needs one. That
  inference-first shape is chosen here.

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
  position, alongside functions, structs, enums, drop functions, constants,
  and `extern` items. `test` is a contextual keyword (an item-position
  `test` followed by a string literal), so existing identifiers named `test`
  — and they are live: `std/bitset.rue` has a `fn test` method — do not
  break. Honest sizing: this is the language's first contextual keyword, a
  new parser category with no in-repo pattern to copy, and Phase 1's
  estimate prices that in.
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
- **Placement is the visibility model.** A test item sees exactly what its
  module's other items see — and because Rue's visibility boundary is the
  *directory* (spec 10.3: a private item is visible from any file in its
  defining file's directory), this is stronger than tests-in-the-same-file:
  a sibling `parser_tests.rue` in the same directory exercises private items
  with no visibility loosening (the shape of Go's in-package `_test.go`
  files), while a test module in a different directory sees only the public
  API and proves its sufficiency. No `pub` changes for testability, no
  special access grants, no test-only visibility syntax — and no obligation
  to put test text in production source files. This resolves RUE-506's
  visibility question structurally: "internal or contract test?" is answered
  by where the file sits, and the answer is visible in the test's module
  path.
- **Test items are roots, not reachable code.** An executable request never
  roots test items; under ADR-0063's demand-driven model their bodies are not
  semantically analyzed, code-generated, or linked into executables. A test
  request roots every test item declared in the root module's transitive
  `@import` closure (not merely modules reachable from `main` — a module
  imported only for its tests still contributes them). Multi-root requests
  are not new machinery: `extern "C"` exports already join `main` as
  co-equal reachability roots today, and a test request is the same shape
  with a different root set.
- **An unimported test file is an error to surface, not a silent nothing.**
  Discovery is the import closure, so a sibling `foo_tests.rue` that nothing
  imports would otherwise simply not exist — no diagnostic, no verdict, the
  same "typo becomes false evidence" failure the empty-filter rule guards
  against, and a trap agents will hit by adding a test file without wiring
  the import. But detecting the orphan requires a candidate inventory, and
  ADR-0063 deliberately gives a compilation no ambient one: imports are
  lazy, and the host performs only compiler-produced, policy-bounded
  demands. A driver-side recursive directory walk that parses candidate
  files would be a second source-discovery computation over files the
  canonical snapshot never demanded — a peer frontend, with its own
  parse-failure and multi-root complications as the symptoms of that
  mismatch. The warning is therefore manifest-gated: when
  `--source-manifest` supplies an explicit candidate inventory, a canonical
  compiler query over that inventory
  reports files that contain test items but sit outside the closure, with
  a parse failure in such a file reported inside the warning itself —
  never as a compile error of the request. One publication step is
  honestly new: the manifest's own bytes are a host-visible input today,
  but loading it canonicalizes entries into permission sets without
  reading them — an out-of-closure entry's *content* never reaches the
  snapshot, and a compiler query cannot parse what the host never
  published. Phase 2 therefore adds a bounded candidate-acquisition step
  to ADR-0063's host input protocol: for each manifest entry outside the
  demanded closure, the host reads and publishes the entry's bytes and
  content fingerprint — or a typed absent/unreadable outcome — as
  ordinary revisioned inputs, demanded only by test requests, and the
  orphan check is a parse-only query over those candidates that never
  turns one into a semantic root. Absent entries stay silent (a manifest
  grants an operation, not a claim that the candidate exists — the
  loader's own posture today); unreadable entries are reported inside
  the warning. Naming the protocol is the point: without it, "canonical
  query over the inventory" would quietly become a driver-side read and
  side table — the peer-computation shape this bullet exists to
  avoid. Without a manifest there is no
  scan and no warning; the run summary instead carries a one-line notice
  that orphan-test detection needs `--source-manifest`. The manifest is
  also the disciplined multi-root story: an inventory belongs to one root,
  so test-shaped files belonging to *other* roots never produce false
  warnings.
- **The warnings interaction is a Phase 1 decision, taken here.** Unused-item
  warnings in executable requests are filtered through a whole-program
  syntactic reference scan today, so the design must pick: include test
  bodies in that scan (test-only helpers do not warn; executable requests
  pay a small per-test-body syntactic scan) or exclude them (private helpers
  used only by tests would warn in executable builds, quietly reintroducing
  the visibility mutilation this design exists to avoid). This ADR picks
  inclusion, and states the cost honestly: test items in the import closure
  cost executable requests parse plus a syntactic reference scan — still no
  semantic analysis, no codegen, no linking.
- **Preview gate**: `test_declarations` (the `test_infra` flag name is already
  taken by compiler self-test machinery). Declaring a test item without the
  preview enabled is the standard preview-gate diagnostic. `rue test`
  enables the gate implicitly while the feature is in preview.

### 2. `rue test` is a driver mode emitting a versioned event stream

The driver grows its first subcommand:

```
rue test <root.rue> [--list] [--filter <pattern>]... [--format human|json]
         [--jobs N] [--timeout-ms N] [--shard K/N] [--target <t>] [-O<n>]
         [--seed N] [--no-cache] [--changed-only] [--keep-going] ...
```

- **Dispatch rule**: the driver enters test mode when the first argument
  that is neither a flag nor a flag's *value* is exactly `test` — the scan
  must be value-aware, because eleven existing flags take a following value
  and `rue -o test prog.rue` must keep meaning "output named `test`." A root
  source literally named `test` is spelled `./test`. This is the driver's
  first subcommand (`--watch` is the only alternate mode today, and test
  mode joins its flag-combination validation path). All existing
  compile-mode flags that make sense in test mode (`--target`, `--preview`,
  `-O`, `--source-manifest`, `--link-archive`, `--error-format`, `-j`,
  logging) keep their spellings and semantics.
- **Streams**: compiler diagnostics remain on stderr exactly as today
  (`--error-format json` unchanged and orthogonal). Test events are the
  runner's own surface on stdout: with `--format json`, one JSON object per
  line (NDJSON). Unlike the diagnostics stream, the event stream carries an
  explicit schema version in its head event, per ADR-0061 §6 — the
  diagnostics stream's missing version field is the outlier, not the model.
  Events are produced from session artifact views, never scraped from human
  output — the posture ADR-0061 records for the future RUE-439 machine
  interface, applied here.
- **Event kinds** (schema doc ships with Phase 2; sketch, not normative):
  `run_started` (schema version, root, target, plan summary, seed),
  `test_started` (stable ID — without it a consumer cannot attribute hangs
  or render progress), `test_finished` (stable ID, verdict, duration,
  capability summary, failure structure, captured stdout/stderr, exact
  reproduction argv), `run_finished` (counts, wall time, cache statistics). Verdicts: `pass`, `fail`, `timeout`,
  `crash` (killed by signal), `compile_error`, `skipped`, `cached_pass`. A
  failure record is data: failure kind (`assert` / `trap:<class>` / `exit` /
  `signal` / `timeout` / `output_overflow` / `ice`), the pinned runtime
  message (the abort-only
  runtime's fixed stderr strings are machine-recognizable by construction),
  exit code or signal, and a source location — in the MVP, the test
  declaration's span. The record's payload and location fields are extension
  points, not closed shapes: richer expected/actual payloads and
  failing-call-site locations arrive through the structured failure channel
  (§7.1) as additive schema minors, never by parsing prose. One
  sequencing state is pinned now rather than discovered at Phase 3: the
  `capability_summary` field is present from v1.0 with an explicit status
  discriminator — `{"status": "unavailable"}` throughout Phase 2, which
  ships zero capability claims, replaced by the populated `available`
  form when Phase 3 lands, an additive change inside a field consumers
  already handle rather than a retrofitted optional. `--list` output
  carries the same state, so neither surface ever contradicts the MVP or
  guesses at an absent field's meaning.
- **Captured output is bytes, budgeted** — v1 schema obligations, pinned in
  the Phase 2 schema doc. Rue strings may carry arbitrary non-UTF-8 bytes
  and the runtime writes them to the pipes raw, so captured streams cannot
  be assumed to be JSON-safe strings: output fields carry an explicit
  encoding tag — UTF-8 when the bytes validate, base64 otherwise — and are
  lossless within the retained window. Capture is bounded per stream *as
  bytes arrive* (the `rue-test-runner` mechanics already include the
  limited-drain variant alongside the unbounded one; Phase 2 adopts the
  limited variant), so a fast writer cannot consume unbounded runner
  memory inside its wall-clock budget; exceeding the per-stream limit
  kills the process group and yields a `fail` verdict with failure kind
  `output_overflow`, retained prefix attached. Verbosity asymmetry (below)
  applies to payloads too: a failing test's event carries the retained
  capture inline; a passing test's event carries digests and byte counts
  only, with a flag to opt passes into inline capture. The §7.1
  structured failure channel is carried and budgeted separately from user
  output, so a test that floods its streams cannot truncate its own
  failure record.
- **Asymmetric verbosity**: the default human renderer prints failures in
  full — structured failure, captured output, repro line — and passes as a
  count. No wall of green. The human renderer is implemented as a consumer of
  the same event stream it would emit under `--format json`, which keeps the
  machine surface honest.
- **Discovery without execution**: `rue test <root> --list --format json`
  emits the inventory — IDs, declaration spans, capability summaries, cache
  status — without running anything, performing semantic analysis of test
  closures but no codegen, no linking, no execution. The *inverse* query is
  reserved in the same surface: "which tests reach item X" is RUE-506's
  tests-for-function question and the sound, static version of what dynamic
  test-impact tools sell — the per-root reached sets already exist, so
  `--list --reaches <item>` (spelling illustrative) is nearly free and the
  schema reserves reached-set provenance for it from v1.
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
observes fresh process state; has an independent lifecycle (start, kill,
timeout) enforceable by the runner; gets its stdout/stderr captured and
attributed exactly; and receives best-effort process-tree cleanup when it
ends, however it ends. One clause is deliberately scoped rather than
universal: **noninterference — a test's execution cannot corrupt, mask, or
abort any other test's result — is guaranteed only for verified-hermetic
tests** (§4), whose summaries prove they cannot reach a channel that
touches another test. A test holding `syscall` or `ffi` can, by
definition, signal arbitrary processes, mutate shared absolute paths,
contend on ports, or spawn descendants that outlive it — and a process
group is not containment, because a child that calls `setsid` leaves it.
Process isolation plus a private scratch directory narrows the accident
surface well below every mainstream runner's baseline, but it is not a
sandbox, and this ADR does not pretend otherwise. Runs that need enforced
noninterference for unverified tests await a real platform sandbox
(future work behind this same contract) or the Phase 5 scheduling
controls.

The MVP mechanism:

- **One test image per target.** The compiler synthesizes a dispatcher `main`
  (an ordinary generated function, not a runtime feature): it reads a test
  selector from argv and invokes exactly one test body. Synthesized
  instances have two in-tree precedents — drop glue as a first-class
  synthesized function instance, and the export thunk as a compiler-owned
  link input with its own image-plan entry — and the dispatcher follows
  their shape. The image links every selected test's `CodegenUnit` closure
  through the image-planning path (internal today; exposing a test-image
  request is ADR-0061 facade work inside Phase 2's scope) — per-function
  codegen artifacts are shared with regular builds and across test runs by
  the existing memo database, so the marginal cost of the image is one link,
  not N compiles. Dispatcher code is runner plumbing, not the test's code:
  it is excluded from every test's capability summary and closure
  fingerprint by construction (§4.2).
- **One process per test invocation.** The runner spawns the image once
  per test under the loader-visible exec contract defined below (constant
  `argv[0]`,
  fixed-width selector, pinned environment), in its own process group,
  with a per-test wall-clock timeout
  (default 10 s, matching `rue-test-runner`), SIGKILL to the group on expiry,
  and reader threads draining both pipes with the per-stream capture bound
  of §2 (the mechanics `rue-test-runner` already has, including the
  limited-drain variant; Phase 2 extracts and reuses them rather than
  reimplementing). After the leader exits normally, the runner additionally
  SIGKILLs the remaining process group: lifecycle hygiene that reaps
  stragglers holding pipe fds, documented as exactly that — the contract
  above already concedes it is not containment.
  Timeout kills, signal deaths (including SIGPIPE's status 141), and exit 101
  with a pinned trap message are distinguished in the verdict, and ICE
  detection remains a separate failure class no marker can absorb. Two
  contract details the mechanics must honor: the runner keeps both pipes
  open and drained until the child exits — a stdout-writing test must never
  die with SIGPIPE because a reader closed early, since §4.1 classifies
  `stdout` as hermetic-compatible — and a body that calls `std.exit(0)`
  before its assertions is an accepted blind spot shared by every
  process-based runner; the dispatcher may later distinguish "returned" from
  "exited" (an epilogue sentinel on the structured channel) if it proves
  worth closing.
- **Execution environment contract** (Bazel's encyclopedia, enforced by
  construction where possible). The inventory a test can observe is defined
  separately from the runner's plumbing, because the runtime captures the
  loader-provided `argc`/`argv`/`envp` unchanged at entry and exposes them
  through `std.env` — without a defined boundary, a dispatched process
  would leak the real image path, the internal selector, and
  per-run scratch paths into test-visible state: values that vary while
  the body's closure does not, poisoning either the cache key (if
  fingerprinted, routine hits vanish) or its soundness (if not, cached
  verdicts can be stale). The **test-visible inventory** is pinned to
  exact values: argv is fixed and documented (a stable logical `argv[0]`,
  no selector, no image path); the environment is a fixed ordered list of
  exact `KEY=VALUE` entries, with runner-set `RUE_TEST_*` variables
  carrying stable logical values — the scratch directory is always spelled
  `.`, which is the fresh private working directory each test starts in
  (deleted on pass, retained on failure for post-mortem); and stdin is a
  fixed EOF stream unless a future explicit input joins the test's
  identity. The dispatcher consumes the selector and replaces the
  runtime's captured inventory with the pinned one before invoking the
  body, so `std.env.args()` inside a test observes the contract, not an
  incidental internal protocol — which is also what makes tests *of*
  `std.env` meaningful. That replacement is the *visibility* boundary,
  and it is not by itself a *resource* boundary: the loader lays the real
  argv and environment strings out on the initial process stack before
  `main` runs — the runtime captures exactly those loader vectors at
  entry — so their byte size is stack consumption no later pointer swap
  can undo, and a varying real `argv[0]` (the image path, under a naive
  `image --run <id>` launch) would let a near-limit recursive test cross
  the stack boundary with no keyed input changing. The **loader-visible
  inventory** is therefore pinned too: the runner execs every test
  process with a constant `argv[0]`, a fixed-width selector (index-
  shaped, never a path), and exactly the pinned environment vector, and
  it launches the image through a run-constant path spelling (a
  constant-named link in the per-test working directory), so
  loader-injected path strings — `AT_EXECFN` and its macOS analogue live
  on the same stack — are constant bytes as well; the remaining
  auxiliary-vector entries are fixed-size by platform contract. Initial-
  stack consumption is then deterministic per keyed configuration, which
  is what makes the pinned `RLIMIT_STACK` of §4.1 an actual determinism
  boundary rather than a bound over a varying baseline. (An inherited
  control fd is the recorded alternative selector transport.) With the
  environment pinned at exec time, the dispatcher's replacement reduces
  to argv normalization — consuming the selector and presenting the
  documented logical `argv[0]`. The exact visible values — ordered
  environment entries, argv values, stdin policy, and the loader-visible
  constants — participate in the cache key (§5),
  not merely an allowlist of names: exact values are what keep cached
  verdicts sound, and their stability is what keeps routine runs
  cache-hittable.
- **Parallelism**: the runner schedules up to `--jobs` test processes
  concurrently. Verified-hermetic tests are non-interfering by proof (§4),
  so their parallelism is unconditional. Tests with unverified capabilities
  also run in parallel by default in the MVP — but per the scoped contract
  above this is a pragmatic default, not a guarantee: such tests *can*
  interfere through the OS, and a suite that observes it reaches for
  `--jobs 1` today, declared serial groups in Phase 5, and a platform
  sandbox eventually. The runner never silently serializes on inference;
  scheduling changes are always visible policy.
- **Reproduction as data**: every failure event carries the exact argv to
  reproduce that single test under the same seed, target, opt level, and
  filter — copy-paste (or agent-invoke) ready.
- **Per-test compile failure is a verdict, not a run abort** (Phase 4).
  Because each test's closure is analyzed independently as a root, a semantic
  error in one test's closure can yield a `compile_error` verdict carrying
  those diagnostics while every other test still builds into the image and
  runs. The mechanism is exclusion, not stubbing: failed closures simply do
  not enter the image, and their tests report `compile_error` with the
  diagnostics attached. The MVP keeps the simpler whole-run-fails behavior;
  the per-test contract is specified now so nothing in Phase 2 bakes in the
  coarser one.
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
| `addr` | `@ptr_to_int` (address observation — see below) |
| `syscall` | any `@syscall` site (subsumes fs, net, clock, process, exit, ...) |
| `ffi` | any `extern "C"` foreign *call* (exports are ordinary Rue code) |

There is no `exit` bit: user-visible exit is `std.exit`, itself a raw
`@syscall`, so it joins `syscall`; the `__rue_exit` helper is emitted only by
the compiler's own `main`-return and dispatcher lowering — runner plumbing,
excluded from test summaries (§4.2), which also keeps a naive leaf walk from
joining it into every test.

`addr` exists because address observation is real nondeterminism reachable
with no syscall, no FFI, and no entropy intrinsic: `@ptr_to_int` returns the
raw address of an allocation the kernel placed (`mmap`, ASLR), so
`checked { @ptr_to_int(@raw(x)) % 2 }` varies run to run. Without this bit
the hermetic predicate is unsound — the exact failure mode this design
promised to eject rather than absorb. The bit is not `checked` blocks (not
a lattice input at all), not the raw-pointer family, not allocation.

It also cannot naively be "every `@ptr_to_int` site": today's intrinsic
conflates true address observation with three deterministic idioms std is
built on — null testing (`@ptr_to_int(p) == 0` is the spec's own stated
null test, 9.2), provenance-preserving rebase
(`@int_to_ptr(@ptr_to_int(p) + off)`, the `StrBuf` byte-copy path), and
pointee type-punning (documented in `std/rawbuf.rue` as the sanctioned
cast idiom). Forty-two such sites sit under `StrBuf`, `RawBuf`,
`ArrayBuf`, `mem.swap`, `sort`, and `binary_heap`; a bit on the bare
intrinsic would mark nearly every real test `addr` and collapse the
hermetic set to arithmetic-only. None of those idioms observes an address
— the integer flows straight back into `@int_to_ptr` or a null comparison;
the nondeterminism exists only where the integer *escapes* (branched on,
stored, hashed, printed).

The recommended disposition makes the distinction syntactic rather than
inferred, by resolving RUE-967 — the strict-provenance intrinsic split
already deferred from ADR-0059 — with this ADR as its first forcing
consumer: dedicated intrinsics for pointee casts and null tests, byte
offsets on `@ptr_offset` over `ptr u8`, and a mechanical migration of
std's sites. After the split, a surviving `@ptr_to_int` is rare and means
exactly "observe the address," and `addr` is the plain syntactic leaf this
section promises; the split also removes integer-roundtrip provenance
destruction from std ahead of future alias-analysis work, which is why
Rust made the same move. The fallback, if RUE-967 resolves against the
split: an escape-scoped `addr` that joins only when a `@ptr_to_int` result
flows anywhere other than `@int_to_ptr` operands or null/pointer-equality
comparisons — computed body-locally, any non-local escape joining
conservatively, no per-call-site special cases. That keeps std unchanged
at the price of a small soundness-critical dataflow rule where this
section otherwise promises syntactic leaves. Either way the decision must
land before Phase 3 ships summaries: the capability system's first visible
output must not be "everything is `addr`."

Allocation, traps, and the pure helper family (string ops, parsing, memcpy)
introduce no capability bit. For traps that is because the trap *is* the
verdict: it terminates the process into the captured, attributed failure
record, observable only through channels the runner owns. Allocation needs
the finer statement, because machine memory state is genuinely ambient: the
safe allocation path traps on exhaustion (verdict channel, covered), but the
raw intrinsics observe pressure as *values* — `@alloc`/`@alloc_zeroed`/
`@realloc` return null and `@resize` returns `false` (spec 8.6:4) — and
stack overflow depends on ambient `RLIMIT_STACK`. Pinning OS resource
limits is not sufficient to make those values deterministic, and the
shared test image (§3) is why: under a pinned `RLIMIT_AS`, the address
space left for the heap is the limit minus everything else mapped —
including the whole selected image, whose size varies when *unrelated*
tests are added or filtered. A test that branches on `@alloc` returning
null near the limit could then change verdict under an unchanged
per-test closure fingerprint — `RLIMIT_AS` exhaustion on an otherwise
non-exhausted machine, exactly the stale-cached-verdict shape this design
exists to eject. The posture taken instead pins the budget where Rue
already owns the boundary: the runtime heap is a runtime-owned recycling
allocator over raw page mapping, and test builds link a **budgeted page
mapper** at that boundary. The budget must do more than exist, because a
policy denial and an ambient mapping failure must not collapse into the
same observable — and today they would: the allocator's permit hook and a
permitted direct mapping that fails ambiently both surface to the program
as the same null pointer, a body can branch on that null and exit 0, and
the runner cannot tell afterward whether it observed the deterministic
budget or the machine (an epilogue check cannot close this either, since
`std.exit(0)` is an accepted blind spot). So the mechanism is reservation,
not rejection: at process startup, before the test body runs, the mapper
reserves the entire permitted arena from the OS in one mapping; every
subsequent map serves page ranges carved from that runtime-owned arena
and every unmap returns them, with no ambient mapping syscalls after
startup. An in-budget allocation therefore *cannot* fail ambiently — its
storage is already reserved — and an over-budget allocation fails by
policy, deterministically. A null observed by the body means exactly
"over budget," a deterministic function of the test's own allocation
sequence, independent of image size and machine state. The one remaining
ambient failure point is the startup reservation itself, which precedes
the body and is reported through a pinned pre-body protocol as an
infrastructure failure — never a verdict, never cacheable (a body that
mimics that report can only waste a re-run, never mint a cached pass, so
the telemetry needs no authentication). The budget's units are pinned
with the mechanism: denominated in bytes, rounded to whole pages,
accounted as pages carved from the reserved arena — small-allocation
arenas carve and are retained at high water (the allocator's existing
recycling design), direct mappings carve and return on deallocation — so
the accounting, like the results, is a function of the allocation
sequence. It is explicitly not an attempt count: the zero-argument
permit/deny hook the allocator carries today is evidence the chokepoint
exists, not the mechanism — it runs once per attempt before layout
classification and cannot express bytes, pages, or high water, and one
permitted enormous mapping under it could still fail ambiently. One
overcommit honesty note: on platforms that overcommit, the startup
reservation is address space, and ambient memory pressure can still
surface while touching reserved pages — as a kill, never as an in-budget
null. A killed test is a failure verdict and failures are never cached,
so cache soundness is unaffected; only run reliability is exposed, and
pre-faulting the arena is the available hardening if it matters.
`RLIMIT_STACK` is pinned (stack consumption is test-local given §3's
loader-visible pinning; the image does not eat it), and `RLIMIT_AS`
survives only as a generous out-of-key backstop sized so the arena
reservation fits under it by construction; failing the reservation at
startup is the infrastructure report above (§5's hermetic-mismatch
shape), never a cached or cacheable verdict. A cached hermetic pass
therefore claims determinism *given the pinned budget and the §3 visible
inventories* — stated, bounded, and in the key — at the price of one
honest divergence, recorded under Consequences: test builds fail
allocation at the budget where production would keep going. The budget is
generous by default, configurable, and part of the cache key. This
disposition is a maintainer call gating Phase 4 (see Open Questions); the
rejected shapes are keying every verdict on the whole image (forfeits
item-granular reuse, the design's central economy), per-test or per-shard
images (a link per test or a layout policy per selection), an inferred
"observes allocation failure" ejection bit (a second escape-analysis
obligation for a channel the allocator can simply close), and the two
detect-don't-remove variants — typed ambient-failure telemetry on a
runner channel, or marking ambient-failure runs non-cacheable after the
fact — which are sound but strictly weaker: they detect the ambiguity
rather than remove it, leaving the verdict itself machine-dependent and
converting detection into re-runs, where reservation keeps the verdict
deterministic and cacheable. One
further channel is closed by spec posture rather than analysis: reading
uninitialized `@alloc` storage is currently defined-but-unspecified, so this
ADR adopts "verdict caching is sound for UB-free programs" and asks for
uninitialized reads to move to the UB list (maintainer call), which disposes
of that channel the standard way.

`hermetic` is the derived predicate "no `syscall`, no `ffi`, no `random`,
no `addr`" (`stdio`, `env`, and `args` are compatible with hermeticity
because the runner pins and captures them — the §3 test-visible inventory
fixes their exact values, and those values are part of the cache key).

`syscall` is intentionally the coarse top of the OS hierarchy in v1. Splitting
it into `fs` / `net` / `clock` / `process` requires classifying syscall
numbers at comptime-constant `@syscall` sites (std.fs/std.net select numbers
via constant-foldable `@target_arch()`/`@target_os()` matches, so this is
plausible) with any non-constant number widening to full `syscall`; that
refinement is Phase 6, gated on a spike, and nothing before it depends on the
finer partition.

#### 4.2 The computation

Effect summaries ride the ADR-0063 graph — but not as per-function queries
that request their callees' summaries over the raw call graph. ADR-0063
explicitly rejected that dependency shape ("make body queries depend on
callee bodies") because it turns legal source recursion into query cycles,
and the query engine treats a cycle as an unconditional abort: there is no
fixpoint iteration anywhere in the runtime, and every family that meets a
cycle today converts it into a diagnostic. But the opposite shape — one
coordinator query that observes the reached graph *and* every canonical
body, condenses, and joins everything — is also wrong, in a subtler way:
per-identity output projections would keep *downstream* consumers green,
yet any leaf-only body edit still changes one of the coordinator's
dependencies and re-runs it over the whole graph. That is narrow
invalidation without narrow recomputation — the distinction ADR-0063 §8
itself draws when it notes the baseline reachability evaluator re-derives
the graph even when it republishes unchanged memberships — and it would
leave the warm-edit economics this section claims without a mechanism.
Incremental inference is a central premise inherited from RUE-506, so the
work is split so each piece re-runs only when its actual inputs change:

1. **A body-local edge projection** reads one body's `BodyReferences`
   projection and resolves it into that body's outgoing effect-graph
   edges. This is where drop glue is made a real edge rather than lost: a
   `DropGlue` body reference names the *type* whose value the body can
   destroy, not a callee — the destructor edge exists only after
   expansion through the per-type drop-glue facts family, traversing
   nested glue and collecting destructor instances, exactly as
   reachability expands it today. The edge projection observes those
   same configured drop-glue facts and publishes destructor edges
   alongside ordinary call edges, attributed to the destroying body; a
   change to one type's drop glue re-runs exactly the edge projections
   of bodies that can destroy it.
2. **A body-local leaf projection** reads exactly one canonical body and
   extracts its effect leaves — runtime-helper calls and intrinsic uses
   are explicit instruction payloads there (helper-manifest classes,
   `@syscall`, the `addr` leaf per the ratified §4.1 disposition, FFI
   calls). A body edit recomputes exactly one leaf, with cutoff when the
   leaf bitset is unchanged — the common case for behavior-preserving
   edits.
3. **`EffectGraph(RootSet)`** observes only the reached set's edge
   projections — never canonical bodies, never raw references — and
   computes SCC membership and the acyclic condensation over them.
   Component keys are content-derived (sorted member identities) *and
   configuration-bearing*, and the query publishes per-component
   projections: membership plus callee-component keys. A body edit that
   changes neither references nor destroyed types leaves every input
   green and the family never runs; an edge-changing edit re-derives the
   graph in reachability's §8 baseline economics, but components whose
   membership and edges are unchanged publish unchanged projections
   under unchanged keys.
4. **`ComponentEffect(SccKey)`** joins its members' leaf bitsets with its
   callee components' summaries, observing its members' leaf projections
   and the per-component projection the configured `EffectGraph`
   publishes — which is how an edge change reaches it. The condensation
   is acyclic by construction, so these are ordinary query dependencies —
   source recursion cannot re-enter as a query cycle — and a changed leaf
   re-runs only its component and that component's reverse callers,
   stopping at the first unchanged bitset (bounded, monotone, cheap).
5. **Per-function summary projections** project their component's result,
   so a downstream consumer observes one function's summary stamp rather
   than a whole-closure or whole-graph stamp.

Every family in the split is keyed by the semantic configuration as well
as its identity content: the edge and leaf projections adopt the
(instance, configuration) shape the in-tree body family already uses,
`EffectGraph` is keyed by root set plus configuration, and `SccKey` is
the configuration plus the sorted member identities. Member identities
alone are not a semantic address — the in-tree body and type query keys
carry the semantic configuration in equality and hashing for exactly this
reason, and two target or configuration requests can condense the same
member set with different outgoing edges and different canonical leaves.

Drop glue deserves the standalone statement the edge projection encodes:
a `drop fn` that performs `@syscall` is reached through value
destruction, not an ordinary call, and the body reference that records
the destruction names a type. A naive walk over `BodyReferences`
callables would silently drop destructor effects from the summary of
every body that can destroy the type; Phase 3 carries the drop-glue
expansion as an explicit obligation with its own unit coverage. Two accounting notes: the leaf projection
becomes the first production consumer of the retained canonical-bodies
family, so Phase 3's measurements must price that retention into the
test-request budget rather than assuming it free; and compiler-synthesized
dispatcher code is excluded from test summaries by construction. Family
and key names remain conceptual per ADR-0063 §6; the in-tree reachability
family is body-reachability over closure keys, and exact Rust names follow
the implementation.

Summaries are canonical artifacts with terminal fingerprints, and the
economics claim is now mechanism-backed: a leaf-only edit touches one leaf
projection, one component, and the reverse-caller chain until cutoff —
near-zero warm work, exactly the inferred-error-set economics RUE-506
predicted, and what the Phase 3 edit-scenario gate measures. The honest
residual cost: edge-changing edits (references or destroyed types) pay an
`EffectGraph` re-derivation, as reachability pays its §8 baseline; if measurement shows that misses the
warm budget, the same escape hatch applies — incremental SCC maintenance
behind the same query contract. Dependency summaries shipped in library
metadata (RUE-506's concern) are moot until Rue has separate compilation;
whole-program analysis sees every body today.

A test's capability set is the summary of its body instance. It appears in
`--list` output and on every test event — the analysis is visible from day
one of Phase 3, before anything acts on it.

Summaries are demanded, never pushed. The effect families (the edge and
leaf projections, `EffectGraph`, `ComponentEffect`, and the summary
projections) are
requested only by test requests (`rue test` execution and `--list`); an
executable request (or
any future check-style request) never demands them, and under
ADR-0063's demand-driven model an undemanded family simply never executes — capability tracking is free for
every compilation that does not consume it. Two implementation constraints
preserve that property: leaf extraction reads the canonical body artifacts
the normal pipeline already produces (no eager per-body effect recording is
added to semantic analysis on behalf of a query that may never run), and
speculative evaluation of summaries during non-test builds stays off
(ADR-0063 §1 permits speculation as scheduling policy; enabling it for
summaries would be an explicit future choice, not this ADR's default). If a
consumer outside testing later wants summaries — say an optimization pass
replacing the CFG's coarse all-intrinsics-are-opaque side-effect
classification, or an IDE surface — it brings the cost in by demanding the
query, and the query model attributes that cost to that consumer exactly.

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
declared narrowing ships (Phase 6), FFI is simply top. Declared narrowing
also has a floor: it informs scheduling, reporting, and selection pressure,
but an `ffi`-touching test never becomes *cacheable* on the strength of an
annotation — a lying or stale declaration must never produce a stale cached
pass, so "unsoundness ejects" stays literally true even against dishonest
annotations. The full obligations this places on future trait, dispatch,
and function-value designs are recorded in "Constraints on future language
evolution."

### 5. Hermetic verdicts are cacheable artifacts; selection is a consequence

- **The verdict cache** stores, per stable test ID: the closure fingerprint it
  passed under, and the verdict metadata (duration, per-stream byte counts,
  captured-output digest).
  The closure fingerprint covers the test body's reached artifact fingerprints
  (ADR-0063 terminal fingerprints over the canonical closure), the compiler's
  own build identity, target, opt level, the exact test-visible and
  loader-visible inventories
  (§3: argv values, ordered environment entries, stdin policy, the
  loader-visible constants), the pinned
  allocation budget and stack limit (§4.1), seed policy, **every runner
  policy that participates in verdict determination** — today the
  effective per-test timeout (`--timeout-ms`, and any per-test override
  the moment one exists) and the per-stream output limits, whose values
  decide `timeout` and `output_overflow` verdicts — and the
  link-relevant inputs
  (`--link-archive` contents). The dividing line is whether a setting can
  change a verdict, not where it is spelled: presentation-only settings
  stay out of the key, and a pass cached under a ten-second timeout must
  never replay as `cached_pass` under a one-millisecond one. Two monotone
  relaxations are permitted because the stored metadata proves them: a
  pass recorded under timeout T is valid under any effective timeout
  ≥ T (`pass` is a semantic verdict; wall clock is machine-relative and
  was never fingerprintable), and a pass recorded with per-stream byte
  counts is valid under any output limit ≥ those counts. Relaxations may
  widen reuse, never narrow soundness. Image identity is deliberately *not* in the
  key: the loader- and test-visible inventories and the allocator budget
  are what make a
  hermetic verdict image-independent, so item-granular reuse survives
  adding or filtering unrelated tests — and demonstrating that
  independence is an explicit obligation of the Phase 4 key audit, with
  the §4.1 backstop-limit escape (an infrastructure verdict, never
  cached) covering what the proof cannot. A
  test is skipped as `cached_pass` only when it is verified hermetic and its
  fingerprint is unchanged. Failures are never cached. `--no-cache` forces
  execution. The §2 flag that opts passing output into inline capture is
  defined against the cache rather than left to luck: requesting inline
  pass output forces execution for tests whose cached entries hold only
  digests — the machine and human surfaces must never depend on whether a
  cache happened to be warm — with storing retained pass output as a
  separate cache artifact a permitted later refinement, and silently
  omitting the output on a cache hit the rejected shape. The cache is a small content-addressed file the runner owns —
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
  tracker. The known economics objection to fine-grained selection — dynamic
  method-level RTS often loses end-to-end to coarser granularity because
  collection overhead outruns the savings (HyRTS, ICSE 2018) — dissolves
  here: fingerprints are byproducts of compilation the test request performs
  anyway, so the marginal collection cost of item granularity is zero.
- **Flakiness is localized by construction**: a verified-hermetic test is
  deterministic given the pinned visible inventories and allocation budget
  (§3, §4.1) — so when a hermetic test's
  verdict differs across runs of the same fingerprint, the runner reports it
  as an infrastructure or compiler-determinism defect, not a test defect (and the reproducibility harness's byte-identical-artifact
  guarantees make that report actionable). Rerun-based flake detection
  (`--reruns N`) is offered only for non-hermetic tests, aimed exactly where
  nondeterminism can live.

### 6. Determinism defaults

- The runner pins the loader-visible and test-visible inventories (§3) —
  exact argv values,
  ordered environment entries, stdin at EOF; `env`- and `args`-capability
  tests are therefore deterministic given values that are in the cache key.
- There is no clock to virtualize; the absence is load-bearing. Any future
  time API must arrive behind a `clock` capability so this ADR's guarantees
  survive it ("Constraints on future language evolution" records this and
  its siblings).
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

### 7. Extensibility is tiered, in-language first, with no privileged built-ins

The built-in framework must be the default, not the ceiling: a future BDD
layer, cucumber-style step harness, assertion library, property tester, or
replacement runner has to be writable without this ADR being reopened. Four
seams, ordered by how much of the verified story each preserves. What is
deliberately *not* extensible: the verdict taxonomy's meaning, the isolation
contract (§3), and the soundness posture (§4.3) — extensions change what tests
look like and how they report, never what "verified hermetic" claims.

#### 7.1 Assertion libraries are first-class by protocol, not by blessing

The channel through which a failing test reports structure — failure kind,
message, expected/actual payload, failing-call-site location — is a documented
runtime protocol, not a privilege of blessed intrinsics. `@assert` today, and
`@assert_eq` when it arrives, are sugar over the same channel any Rue function
can invoke before aborting; a user assertion library emits the same structured
failure records the built-ins do, and the event stream carries them without
knowing who produced them. The recommended mechanism is a **dedicated
inherited pipe**: its own file descriptor, pinned in the §3 exec
contract, written through a runtime helper (an ABI-manifest addition
under ADR-0055 rules, so the choice remains a maintainer call) and
drained by the runner with its own cap, independent of the user streams.
The rejected shape is a reserved framed region of stderr: Rue streams are
arbitrary bytes, so user output can reproduce any in-band framing
byte-for-byte, and a "separate budget" whose frames are extracted from a
shared, capped stream is separate in name only — making in-band framing
unambiguous would require escaping or authenticating user bytes,
complexity that buys nothing over a second pipe. The dedicated channel is
not a security boundary (unchecked code can write to any descriptor it
can name); it prevents *accidental* collision, which is exactly the
promise §2 makes, and it is the natural future carrier for the §3
epilogue sentinel and §7.2 sub-results. The channel is budgeted
separately from user output (§2): structured records carry their own
size cap, so a test that floods its streams cannot truncate its own
failure record. Two consequences
are deliberate: the failure payload is an open, versioned field rather than an
enum of built-in shapes, and location is carried *in* the record — so a
library can attribute its caller — rather than derived solely from the test
declaration. Automatic call-site capture wants a `@src()`-style comptime
intrinsic (deferred; nothing here blocks it); until then library-reported
locations are the library's responsibility. The record also reserves a
*promotion* payload from v1: a failure may carry a machine-applicable
suggested fix — the expect-test/snapshot pattern: new expected value, target
span, and a content hash of what it replaces — and a future
`rue test --accept` applies accepted promotions. The runner applying
promotions, never the test, is what keeps snapshot-style tests hermetic:
the test process still writes nothing. Reserving the field now is the cheap
part; the accept verb ships when a snapshot framework exists to use it.

#### 7.2 In-language frameworks are ordinary Rue code; comptime is the generator

A BDD vocabulary, a table-driven harness, a property tester's case machinery —
written in Rue, these are plain functions and comptime constructs used inside
test bodies from day one, and they inherit capability inference, caching, and
selection automatically because their helpers are reached bodies like any
other. What v1 does not give them is per-case identity: one `test` block
looping over a table is one verdict, one cache entry, one filterable unit. Two
extensions are reserved so that ceiling lifts without redesign:

- **Test items in comptime-instantiated types.** v1 grammar restricts `test`
  to module item position; permitting test items inside struct bodies produced
  by comptime functions (the Zig shape — a generic container's tests
  instantiated and run per specialization) is additive grammar work, and
  ADR-0063 §5's identity domain already covers specialization-anchored members
  (producing definition + canonical arguments + structural anchor), so stable
  test identity extends with no new scheme. The event schema therefore treats
  a test's identity as an opaque stable ID for matching *plus* structured
  identity fields alongside, so IDs can grow producer/argument components as a
  schema minor rather than a breaking re-spelling.
- **Sub-results.** The §7.1 channel generalizes to a sub-result record: a
  running test may emit named child results with their own payloads, which the
  runner attributes as `<test-id>/<sub-name>` rows in the stream. Scheduling,
  caching, and selection stay at the item level — sub-results are reporting
  granularity, which is what table tests and `describe`/`it` nesting need
  first. Reserved in the schema from v1, implemented when demanded.

#### 7.3 Reporters and observers consume the stream

Custom reporters, CI adapters, dashboards, and IDE surfaces are NDJSON
consumers with no protocol negotiation. This works from Phase 2 and is the
intended default extension point; a JUnit adapter is the reference consumer.

#### 7.4 Alternative runners and external providers use documented contracts

A replacement *runner* needs no new privileges at any phase: enumerate with
`rue test <root> --list --format json`, execute through the test image's
documented argv/exit/stream contract — public by commitment from Phase 2, and
nothing in Phases 1–6 may depend on it staying private — and schedule however
it likes. The reverse direction, external test *providers* (step harnesses,
fuzzers, snapshot tools presenting tests the compiler never saw), is Phase 7's
versioned enumerate/execute-by-ID protocol, whose wire format must be decided
together with RUE-505's semantic-API format policy; committing to it now would
prejudge that discussion, so this ADR reserves the seam and defers the
contract. Provider-supplied tests are not compiler-visible bodies, so they get
no verified capability summaries: they run as unverified — always executed,
never cached — unless the provider generates real Rue test items instead.
Eject-don't-degrade applies to extensions exactly as it applies to `@syscall`.

Fixtures deserve one honest note: setup is plain code in the test body and
teardown is destructors, but the abort-only runtime means destructors do not
run on a failing path — teardown-on-failure is process death plus the
retained scratch directory, which suffices for hermetic and fs tests alike.
Expensive fixtures *shared across* tests (a database, a compiled corpus) are a
runner-policy question — serialized groups (Phase 5) plus future setup
commands — and are noted as not locked out rather than designed here.

## Implementation Phases

Linear issues to be filed on acceptance (epic + one per phase, IDs recorded
here per docs/designs/README.md).

- [ ] **Phase 1: `test` declarations** - RUE-TBD. Grammar/lexer/parser (the
      language's first contextual keyword, directives allowed), RIR item, a
      new `Test` kind in the closed stable-definition taxonomy plus its
      namespace decision, semantic analysis as ordinary bodies rooted only by
      test requests, the warnings-scan inclusion decision (§1),
      duplicate-name diagnostics, `test_declarations` preview feature, spec
      sections + spec-test coverage, UI coverage for the gate and
      diagnostics. No runner yet: `--emit`-level verification that test
      items parse, analyze, and are invisible to executable requests.
- [ ] **Phase 2: `rue test` MVP runner** - RUE-TBD. Value-aware subcommand
      dispatch joining the existing mode-validation path; test-request root
      sets; synthesized dispatcher `main` and per-target test image through
      the image-planning path, plus the ADR-0061 facade work to expose a
      test-image request; the loader-visible exec contract (constant
      `argv[0]`, fixed-width selector, pinned environment vector,
      run-constant image spelling) with the dispatcher normalizing the
      runtime's captured inventory to the documented test-visible values
      (§3);
      process-per-test execution with process-group
      timeout/kill, bounded per-stream capture (the limited-drain
      variant), post-exit group cleanup, and pipes held open until child
      exit
      (mechanics shared with `rue-test-runner`); the manifest-gated
      unimported-test-file
      warning with its bounded candidate-acquisition host-input step
      (§1); `--list`, `--filter`, `--jobs`, `--shard`,
      `--timeout-ms`, `--seed` (shuffle), exit-code contract; NDJSON event
      stream v1.0 with schema doc (docs/process/test-events.md), including
      the byte-safe output encoding, capture budgets, pass/fail payload
      asymmetry, and the `capability_summary` unavailable state (§2),
      the structured failure-record channel contract (§7.1), the reserved
      promotion field, and the reserved identity/sub-result shapes (§7.2),
      with the human renderer as its consumer; repro argv in every failure;
      CLI-suite coverage end to end. **This phase is the MVP: usable,
      agent-first, zero capability claims — every test simply runs.**
- [ ] **Phase 2.5: structured assertion payloads** - RUE-TBD. `@assert_eq`
      (and a minimal comparison family) as intrinsics producing
      expected/actual through the §7.1 channel; machine-computed diffs in
      `test_finished` events; human renderer output built from the same
      payloads. Pulled ahead of capability work deliberately: RUE-506 names
      unstructured failure output as the primary agent token sink, and a
      runner that is agent-first in transport but prose in content has not
      met the bar.
- [ ] **Phase 3: effect summary queries** - RUE-TBD. The §4.2 query split:
      body-local edge projections (drop-glue expansion through the
      per-type facts family included, with unit coverage) and leaf
      projections from canonical bodies (helper
      manifest classification, `@syscall`, the `addr` leaf per the ratified
      §4.1 disposition, FFI calls); `EffectGraph` SCC condensation over
      the edge projections with configuration-bearing, content-derived
      component keys and per-component projections;
      `ComponentEffect` joins along the
      acyclic condensation; per-identity stamped summary projections;
      dispatcher code excluded; summaries surfaced
      in `--list` and `test_finished` events (replacing the v1
      `unavailable` placeholder as an additive change); determinism and cutoff
      behavior pinned by compiler unit tests and two measured gates: an
      edit-scenario measurement (ADR-0068 harness) proving near-zero warm
      cost in test mode — leaf-only and reference-changing edits measured
      separately (the latter pays the §4.2 `EffectGraph` re-derivation),
      including the retention cost of becoming the
      canonical-bodies family's first production consumer — and a zero-delta
      measurement on executable-request benchmarks (ADR-0067 harness)
      proving the families cost nothing when not demanded (§4.2). No
      scheduling or caching behavior change. Gated on the §4.1
      `@ptr_to_int` disposition (RUE-967).
- [ ] **Phase 4: verdict cache and selection** - RUE-TBD. Closure fingerprints
      for test roots; the test-build budgeted page mapper with up-front
      arena reservation and pre-body infrastructure reporting (§4.1,
      gated on its maintainer call); on-disk verdict cache
      with documented key composition
      (exact visible-inventory values, allocation budget, stack limit,
      effective timeout, and per-stream output limits
      included);
      `cached_pass`, `--no-cache`, `--changed-only`; the
      inline-pass-capture flag's force-execution semantics (§5);
      hermetic-only gating with
      eject-on-unknown; per-test `compile_error` verdicts (error-tolerant test
      images); cache-soundness audit checklist executed against the spike
      findings, including the image-independence demonstration (§5).
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
      decisions; external providers; JUnit and CTRF adapters as reference
      stream consumers.

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
- Extensibility has no privileged built-ins: assertion libraries share the
  built-ins' failure channel (§7.1), in-language frameworks inherit
  verification automatically (§7.2), and a replacement runner can exist from
  Phase 2 using only documented contracts (§7.4).
- Capability tracking is free when unused, by construction and by measured
  gate: the summary family is demanded only by test requests, and executable
  compiles never evaluate it (§4.2, Phase 3 acceptance).

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
- Test builds diverge from production at the allocation boundary: the §4.1
  budgeted mapper makes raw allocation fail by policy at a pinned budget
  where production would keep going, and each test process reserves the
  whole budget's address space up front (cheap on 64-bit targets; on
  overcommit platforms ambient pressure surfaces as a kill, never as an
  in-budget null). The divergence is the price of sound,
  image-independent verdict caching; it is generous by default,
  configurable, in the cache key, and stated rather than hidden — but it
  is real, and it would join seedable `@random_*` (§6) in the "test mode
  differs from production" column this design otherwise avoids.
- Future language designs inherit obligations from this ADR — capability
  classification for every new effect or dispatch mechanism, summary-bearing
  metadata for any separate-compilation design, a determinism decision for
  any concurrency design. That is a real tax on future work, accepted
  deliberately and recorded in "Constraints on future language evolution"
  so it is paid knowingly, in the open, at design time.

### Neutral

- `scripts/rue test` (the maintainers' compiler-suite wrapper) and the
  user-facing `rue test` become homonyms; docs and AGENTS.md references need a
  disambiguation pass regardless of which rename option is taken.
- Tests may live beside code in the same file or in same-directory sibling
  files (spec 10.3 directory visibility) — production files need not carry
  test text. Test bodies in the import closure cost executable requests
  parse plus the syntactic warning-reference scan (§1), and no semantic
  analysis, codegen, or linking.

## Constraints on future language evolution

This ADR's guarantees are purchased from specific properties of today's
language: a total static call graph, three closed effect doors, no clock, no
threads, abort-only failure, whole-program compilation. Each of these will
change. The design degrades soundly rather than breaking — an edge the
analysis cannot classify widens to ⊤ and the affected tests eject from
caching and selection, never from correctness — so the constraint on each
future design is not "do not do this" but "decide, at design time, how much
of the verified story your users keep." This section is the standing record
of those obligations, written to be citable from future ADRs.

**The standing rule.** Any feature that adds an input channel, a
nondeterminism source, or a call edge the compiler cannot statically resolve
must classify itself against the capability lattice (§4.1) as part of its own
design. The lattice is a checklist for new features, not a closed museum: new
bits are additive, but "unclassified" is not an option — an unclassified leaf
is a soundness hole in every cached verdict. A process hook enforcing this at
review time is proposed under maintainer calls.

### Traits, interfaces, and function-typed values

Comptime-static polymorphism — today's generics, and any future trait system
resolved entirely at monomorphization, Zig-style — costs nothing: every edge
still resolves per specialization and inference stays total. The obligation
lands on *runtime* polymorphism: vtables, function-typed values, closures. A
design adding those must choose a posture per dynamic edge: (a) declared
capability bounds at the abstraction boundary — the `@requires(...)` surface
(§4.5) is reserved for exactly this, on trait members and function types,
with the unannotated default bound an explicit design decision, and with
Nim's `effectsOf` (RFC 404) as the ready-made effect-polymorphic shape for
function-typed *parameters*, which avoids both an annotation wall and
blanket ⊤ for the higher-order common case; (b) no bounds, the edge joins
⊤, and tests reaching it always run — sound, zero-annotation, and the
automatic fallback; or (c) capabilities move into the type system (the
future ADR flagged in §4.4). What a dispatch design may
not do is make call targets unenumerable: every dynamic-dispatch table must
be recoverable from the reached artifact graph, so that posture (b) is at
worst conservative, never unsound.

### Standard library growth and the runtime ABI

Three obligations. New runtime helpers carry a capability class in the ABI
manifest row itself, machine-checked like every other manifest property
(ADR-0055) — Phase 3 adds the field, after which an unclassified helper does
not build. New effectful std APIs route through the existing doors (helpers,
`@syscall`, FFI); introducing a fourth effect mechanism requires amending
§4.1's leaf set in the same change and is otherwise a rejected shape. And two
reservations stand: any time API is born behind `clock` (the current absence
of a clock is load-bearing for determinism), and any env-mutation or
process-spawn API is born behind its own bit or an explicit join to ⊤ — a
spawned child can do anything, so `process` can never be hermetic.

### Floating point

When ADR-0065's floats land, IEEE-754 arithmetic is deterministic per target
and needs no capability bit — but this section exists to say the quiet
parts: NaN payload propagation and any future floating-environment surface
(rounding modes, flush-to-zero) are where float determinism reasoning goes
wrong. Basic arithmetic stays capability-free; an fenv-mutation API, if ever
proposed, is ambient process state and joins the lattice like any other
input channel.

### Concurrency: threads and async

The obligation is to classify scheduler nondeterminism, not to avoid
concurrency. If the exclusivity model (ADR-0037) lets a future structured-
parallelism design guarantee deterministic observable results, parallel tests
remain hermetic and cacheable — the best outcome, and worth weighing during
that design. Concurrency whose observable behavior can vary with scheduling
must carry a nondeterminism capability, ejecting affected tests from caching
exactly as OS entropy does. Mechanism notes, all additive: an async test body
changes how the synthesized dispatcher awaits completion (§3), not the
execution contract; timers want a clock and inherit `clock`'s constraint; a
trap in any thread still aborts the whole process, which keeps verdict
semantics intact; and process-group SIGKILL must remain sufficient to reap a
test regardless of what the runtime spawns.

### Separate compilation and packages

Whole-program visibility is why inference needs no annotations today (§4.2).
A library boundary that hides bodies must ship per-function capability
summaries and artifact fingerprints in its metadata — RUE-506 anticipated
exactly this — or every cross-library call joins ⊤ and dependent tests
always run. The verdict cache (§5) additionally requires that dependency
identity contribute to closure fingerprints: a package design that cannot
content-address its artifacts caps test caching at "eject anything crossing a
package boundary." Both are metadata-format obligations on the package
design, not runner changes.

### Comptime input channels

If comptime ever reads inputs beyond source text (embedded files, build-time
configuration), those reads must flow through ADR-0063's host input protocol
so they appear in revisions and closure fingerprints. A comptime input the
fingerprint cannot see is a stale-verdict bug by construction.

### Failure-model evolution

Verdicts are defined by the execution contract (§3), not by the abort
mechanism. If Rue gains catchable failures or unwinding, failure reporting
migrates into the structured channel (§7.1), which is mechanism-neutral by
design — the pinned-stderr-message taxonomy is an MVP implementation detail,
already replaceable. Destructors would then start running on failing paths;
the fixture note in §7.4 assumes they do not and would be revisited.

### Opaque code mechanisms

Inline assembly, a JIT surface, or any future arbitrary-instructions feature
is a fourth door by definition: it must be `checked`-gated like `@syscall`
and joins ⊤ unconditionally. There is no sound narrowing for code the
compiler cannot read; such a feature trades capability precision for power,
explicitly.

## Rejected alternatives

### Assertions as Result-returning functions

Considered: test assertions as ordinary functions returning
`Result((), AssertFailure)`, propagated with `?`, instead of trapping
intrinsics. Rejected — an assertion claims the program state is correct, and
a failed one is a bug. The language already separates the two channels in
practice: expected failures are `Result`/`Option` values with must-check
linearity (ADR-0038), while invariant violations — bounds, overflow,
division, `@panic` — trap (spec chapter 8). Assertions belong with the
second:

- **Propagation tax.** A value-returning assert makes every asserting
  function transitively fallible — test helpers and production preconditions
  alike — forcing `Result` signatures onto call graphs whose only failure
  mode is "the program is wrong."
- **No meaningful handler.** Linearity forces the `Result` to be checked, but
  must-check is not must-stop: the legal handlings are propagate-to-terminal
  (a verbose re-implementation of the trap) or match-and-continue (running
  code on a violated invariant — the exact thing assertions exist to
  prevent).
- **The abort-only runtime.** With no unwinding, a trap is the only non-local
  exit; a leaf helper cannot otherwise end a test without threading `Result`
  through every intermediate frame.
- **What the intrinsic form specifically buys**: call-site attribution in a
  runtime with no backtraces (an intrinsic lowers at the caller, so the
  failure span is the assertion site, not a library body); comptime folding
  (provably-false asserts become compile errors); optimizer-visible facts on
  the fall-through path; and a spec-pinnable message/exit contract.

What is genuinely given up, and where it is recovered: soft assertions
(check-everything-then-report) fall out of fail-fast — userland accumulators
and §7.2 sub-results are the answers; destructor-based teardown does not run
on the trap path — process teardown plus the retained scratch directory
covers it (§3). The intrinsic is not a monopoly: §7.1 gives userland
assertion libraries the same failure channel, with `@src()`-style call-site
capture as the reserved gap-closer. Using `?` in test bodies for *expected*
fallibility (setup I/O, not assertions) is orthogonal and remains the
Result-typed-test-bodies maintainer call, noting that spec 4.15:3 restricts
`?` to the trusted std producers.

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
- **Uninitialized reads to the UB list** (§4.1): this ADR adopts "verdict
  caching is sound for UB-free programs," but reading uninitialized `@alloc`
  storage is currently defined-but-unspecified — a nondeterminism channel no
  fingerprint can see. Moving it to the UB list is a spec change with
  independent merit; the fallback is an `uninit`-observation bit. Call
  needed before Phase 4 turns caching on by default.
- **Allocation determinism mechanism** (§4.1): ratify the test-build
  budgeted page mapper (recommended — the whole permitted arena is
  reserved at startup, in-budget allocations are served from
  runtime-owned storage and cannot fail ambiently, over-budget
  allocations fail by policy, and the budget is denominated in bytes
  rounded to pages with carve/return accounting) versus keying verdicts
  on the whole image, per-test/per-shard images, an
  inferred allocation-failure-observation ejection bit, a coarser
  observation bit on the raw allocation family (which would eject every
  collection-using test), or the detect-don't-remove variants (typed
  ambient-failure telemetry; post-hoc non-cacheable marking). Pinned OS
  limits alone are insufficient — the shared test image makes
  `RLIMIT_AS` headroom vary with unrelated selection changes — and a
  permit/deny hook alone cannot distinguish policy denial from ambient
  mapping failure: both reach the test as the same null (§4.1). Gates
  Phase 4 caching; the recommended form
  costs a stated test/production divergence at the budget boundary.
- **`@ptr_to_int` disposition** (§4.1): resolve RUE-967 — the
  strict-provenance intrinsic split plus mechanical std migration
  (recommended) — versus the escape-scoped `addr` recognizer. Gates
  Phase 3; the split carries independent memory-model and optimizer merit
  beyond testing.
- **Structured failure channel mechanism** (§7.1): a dedicated inherited
  pipe written through a runtime helper (recommended; touches the ABI
  manifest, so it
  is an ABI change under ADR-0055 rules) vs a reserved framed region of
  stderr (no ABI change, but rejected in §7.1: arbitrary user bytes can
  reproduce any in-band framing, so the separate-budget promise would
  need an escaping or authentication rule to be real).
  Shapes both the runtime surface and the event schema; call needed during
  Phase 2 design, and it gates how soon userland assertion libraries reach
  parity with `@assert`.
- **Exit-code and `@assert` stabilization**: promote `@assert`/`@panic` from
  the reserved intrinsic bucket (4.13:5b) to normative, and decide whether
  assertion failure keeps exit 101 (shared with all traps, distinguished by
  pinned message — recommended, no runtime change) or gets a distinct code.
- **The `scripts/rue test` homonym**: rename the maintainer wrapper subcommand
  (e.g. `scripts/rue suite`), or accept the context distinction and fix docs.
- **Exit-code contract of `rue test`** (§2): the 0/1/2/3 proposal, in
  particular empty-selection-as-error.
- **The standing rule as process**: ratify "Constraints on future language
  evolution" as citable policy, and add a capability/determinism/fingerprint
  impact item to the new-feature checklist (alongside AGENTS.md's seven-layer
  preview-feature list), so those obligations are applied when a feature is
  designed rather than rediscovered when its tests misbehave. Cheap, but it
  touches process docs owned outside this ADR.
- **Naming**: `test_declarations` preview flag; `@group`/`@requires` directive
  spellings; `docs/process/test-events.md` as the schema doc home.

### Questions that need a spike before their phase is scheduled

- **Provenance-split migration audit** (before the §4.1 disposition is
  ratified): enumerate std's 42 `@ptr_to_int` sites by idiom (null test /
  rebase / type-pun / other), and check the `copy_packed_bytes` comment in
  `std/strbuf.rue` claiming `@ptr_offset` cannot form byte-offset
  sub-range pointers — its stride argument does not obviously apply to
  `ptr u8`, where the slot size is 1; if the restriction is real, the
  split needs a byte-offset primitive as well. Output: the migration list
  and the final intrinsic set for RUE-967.
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
  whether the image-planning path can cheaply emit per-shard images instead.
- **Verdict-cache key audit** (during Phase 4, before enabling by default):
  enumerate every input that can affect a hermetic test's outcome (compiler
  build identity, target, opt level, seed policy, visible-inventory
  values, allocation budget and pinned limits, effective timeout and
  per-stream output limits, ASLR and address-layout
  variation, link archives, runner version,
  image identity) and pin each as in-key, irrelevant-by-proof, or
  eject-to-uncacheable. Image identity is the audit's named hard case: the
  §4.1 allocator budget and §3 visible inventories (loader and test) are
  what should make it
  irrelevant-by-proof, and the audit must demonstrate that proof rather
  than assume it. The reproducibility harness's perturbation list is
  the starting checklist.
- **Memo-database pressure under test roots** (during Phase 2): a test request
  roots a strict superset of `main`'s closure; measure the ADR-0063 §14
  retention budgets against check-all-shaped root sets on the larger example
  corpora. The §14 calibration is `main`-rooted, so this is the *first* such
  measurement, not a validation of an existing one; the budgets are soft, so
  the failure mode under pressure is memory growth, not rejection.

### Deferred design questions

- Comptime evaluation of pure test bodies ("compiling is testing") — RUE-506
  Q4. Deliberately not planned: it blurs compile failure with test failure and
  its incremental-cost story inside the query graph is unstudied. Revisit with
  evidence from Phase 3 summaries about how many tests are comptime-eligible.
- Doctests: examples in docs testable by construction (RUE-504 coordination);
  the protocol seam (§7.4) is where a doc-example provider would plug in.
- Per-test timeout/skip/xfail metadata (`@timeout(ms)`, `@skip`,
  `@known_bug("RUE-NN")` with XPASS-fails-loudly semantics inherited from the
  compiler's own suites), plus user-defined tags (`@tag("...")`) with a
  metadata map on test events (additive minor): wanted, but not free —
  directive arguments are identifier-only today, so literal-argument
  directives (`@timeout(5000)`, `@known_bug("RUE-NN")`) are a grammar and
  AST extension, not merely a scheduling choice. Deferred with that cost
  stated; `@requires(fs)` parses under the existing identifier-argument
  form.
- Test items in comptime-instantiated types (§7.2) and the `@src()`-style
  call-site intrinsic (§7.1): both reserved as additive; scheduled when a
  real framework or assertion library demands them, not speculatively.
- Workspace/multi-root invocation, and whether `rue test` without a root
  argument should discover one (needs the package-model discussion, ADR-0047's
  successor).

## Future Work

Structured assertion intrinsics beyond Phase 2.5's comparison family, as
sugar over the §7.1 failure channel — today's `@assert` carries an optional
message but no structure; the gap is structure, not messages; the public
provider protocol's wire format with RUE-505; capability declarations in types
and at trait boundaries (the future ADR flagged in §4.4); seeded-entropy test
profile; duration-aware sharding; JUnit and CTRF adapters; test-aware
`--watch` (rerun exactly the dirtied selection on save — the pieces are
Phase 4 selection plus the existing watch loop).

## References

- RUE-506 (design capture this ADR supersedes in mechanism), RUE-505, RUE-504,
  RUE-438 (machine-readable interface project); RUE-967 (pointer provenance
  split, gating the `addr` leaf disposition in §4.1).
- ADR-0063 §1/§3/§8/§15 (roots, fingerprints, reachability, test-selection
  consumer) and its rejected alternative "make body queries depend on callee
  bodies" (the shape §4.2's component queries avoid by joining along the
  acyclic condensation); ADR-0061 §6 (schema
  versioning policy); ADR-0058 (canonical artifacts); ADR-0055 (typed runtime
  ABI manifest); ADR-0064 (FFI boundary rules, accepted); ADR-0027 (random
  intrinsics); ADR-0025 (comptime); ADR-0069 (CI scheduling that names a
  test runner as future scope).
- docs/process/diagnostics.md (stream precedent), docs/spec/src/09-unchecked-code/
  (the raw-intrinsic chokepoints), `crates/rue-test-runner` (execution
  mechanics), `crates/rue-runtime-abi` (the helper manifest).
- Prior art: cargo-nextest (and Rust RFC 3558's unstabilized libtest JSON);
  Go test caching and test2json; Zig test blocks and build-runner protocol
  (and ziglang/zig#15091 on protocol/stdout sharing); Swift Testing traits
  and event ABI (and its v0-omitted-metadata history); Bazel Test
  Encyclopedia; Buck2 external test runner protocol; Deno permission
  scoping; Unison content-addressed test caching; Nim effect inference and
  `effectsOf` (RFC 404); HyRTS (Zhang, ICSE 2018) on test-selection
  granularity economics; CTRF as a cross-tool report format.
