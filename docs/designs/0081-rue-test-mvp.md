---
id: 0081
title: "rue test MVP: test declarations, runner, and event protocol"
status: proposal
tags: [tooling, testing, syntax, semantics, incremental, cli, language-shape]
feature-flag: test_declarations
created: 2026-08-22
accepted:
implemented:
spec-sections: []
superseded-by:
relates: ["RUE-506", "RUE-505", "RUE-504", "RUE-438", "ADR-0063", "ADR-0061", "ADR-0058", "ADR-0055", "ADR-0064", "ADR-0027", "ADR-0025", "ADR-0069"]
---

# ADR-0081: `rue test` MVP: test declarations, runner, and event protocol

## Status

Proposal. This is the deliberately narrowed successor to the full agent-first
test-runner proposal (rue-language/rue#2239, five review rounds, never
ratified), scoped per review consensus to what an MVP must decide: tests as
language items, the `rue test` driver mode and its versioned event stream,
and process-per-test execution under a pinned contract. The full design
capture remains available in that PR and is partitioned into follow-up
issues (§6); capability inference, verdict caching and selection, scheduling
and flake policy, and the public provider protocol are each deferred to
their own future ADR — deferred, not rejected, and this document's contracts
are shaped so each lands additively.

Drafted from the RUE-506 design capture, re-grounded against the compiler as
it exists after ADR-0063 (parallel demand-driven incremental compilation,
Implemented) and ADR-0061 (supported facade, Accepted).

Nothing here is ratified. The "Maintainer calls" section lists every decision
this document takes a position on that requires explicit sign-off, and the
"Spikes" section lists what must be measured before its phase is scheduled.

## Summary

Rue gets a first-class test runner: `test "name" { ... }` declarations in the
language, discovered and analyzed by the compiler as ordinary demand-driven
roots, executed by a `rue test` driver mode that emits a versioned NDJSON
event stream as its primary output, with human rendering as a consumer of
that stream. Execution is a contract — isolation, independent lifecycle,
per-test timeout, exact output attribution, reproduction-as-data — specified
independently of mechanism; the MVP mechanism is one linked test image per
target plus one process per test, with both the loader-visible and
test-visible process inventories pinned to exact values. Failures are
structured data from day one: a documented failure channel that user
assertion libraries share with the built-ins, `?` in test bodies with
unwrap-and-report semantics, and structured assertion payloads
(expected/actual with machine-computed diffs) inside the MVP itself.

The MVP claims nothing it cannot verify: it ships zero hermeticity claims,
every test simply runs, and the event schema carries an explicit
`capability_summary: unavailable` status rather than a retrofitted optional.
Hermeticity inference, verdict caching and change-based selection,
scheduling policy, and the external-provider protocol are deferred to
focused follow-up ADRs (§6), and the contracts here — pinned inventories,
verdict taxonomy, reserved schema fields — are the ones those ADRs need to
land without breaking this one.

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
  full proposal designed the declaration surface early (reserved with the
  deferred capability work, §6) even though inference needs no annotations
  yet.
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
  (deferred with scheduling, §6).
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
  The deferred verdict-caching ADR (§6) is that idea made granular and
  *inferred*: Unison's purity comes from a type-system ability annotation
  (the authoring tax the next bullet describes), and it has no isolation
  contract, verdict taxonomy, event stream, or change-based selection. The
  contribution here is hermeticity inference in an effect-unannotated
  language plus the contract around the cache — not the cached-verdict idea
  itself.
- **Effect systems** (Koka, Pony, Austral, WASI — and Nim): fine-grained
  declared effect taxonomies impose an authoring tax that has kept them
  niche; coarse inferred summaries with declaration only at genuinely opaque
  boundaries is the adoptable point in the design space. Nim is the
  existence proof at compiler scale — zero-annotation bottom-up effect
  inference, with `effectsOf` (its RFC 404) as a ready-made shape for
  effect-polymorphic function parameters when Rue needs one. That
  inference-first shape is chosen here.

## Scope

In scope: test declarations in the language; compiler discovery and analysis;
the `rue test` driver mode; the event stream schema and its reserved fields;
the execution contract and the process-per-test MVP mechanism; the structured
failure channel; structured assertion payloads.

Deferred to follow-up ADRs, deliberately (§6): capability inference and
hermeticity verification (RUE-1621), hermetic verdict caching and
change-based selection (RUE-1622), scheduling and flake policy (RUE-1623),
and the external test-provider protocol (RUE-1624). These are deferred, not
rejected: the direction stands, and this ADR's contracts are shaped so each
arrives as an additive change.

Out of scope entirely: the doctest mechanism (needs RUE-504's doc model), the
user-authored-framework protocol's wire details (needs RUE-505's semantic API
decisions; only the seam is reserved here), benchmark/property/fuzz
frameworks themselves, a package/workspace model (the runner takes a root
module exactly like the compiler), and any promise about a persistent
cross-process memo database (ADR-0063 future work).

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
  division), exits nonzero, or is killed.
- **`?` in test bodies: unwrap-and-report.** A test body may apply `?` to an
  operand of a trusted-producer type (spec 4.15:3's standard
  `Option`/`Result`, unchanged), with test-specific dynamic semantics: the
  success arm is the ordinary one (`Some(v)`/`Ok(v)` evaluates to `v`), and
  the failure arm does not propagate — a `()`-typed body has no enclosing
  producer to construct — but emits a structured failure record and traps at
  the `?` site. This is an additive spec rule in a position that is a
  compile error today (E0503/E0505 reject `?` in any `()`-returning body),
  so no existing program's meaning changes, and it is scoped lexically to
  the test item's immediate block: helper functions keep ordinary `?`
  rules, and a `Result`-returning helper composes by being `?`-ed at the
  test-body boundary. Three properties fall out of trapping instead of
  propagating. *Attribution*: the trap lowers at the `?` site, so the
  failure span is that line of the test body — the same call-site
  attribution `@assert` gets, with no backtrace machinery. *Per-site error
  types*: no enclosing `Err` is ever constructed, so spec 4.15:4's
  identical-error-type requirement does not apply — one body can `?`
  through unrelated error enums on consecutive lines, which a
  `Result`-typed body could not offer until error conversion exists. *No
  signature surface*: the block stays `()`; there is nothing to annotate.
  The failure record (kind `unhandled_error`, §2) carries the `?` site's
  span and a best-effort rendering of the payload: the compiler
  synthesizes a structural printer for the operand's error type — variant
  name and primitive/byte-string payloads, recursion and length bounded by
  the §2 capture budgets — as a synthesized instance in the
  drop-glue/dispatcher family (§3), keyed **by error type, not by site**.
  The printer's behavior depends only on the type it renders, while the
  site belongs to the failure record's header, so one printer instance
  serves every `?` on that type: per-site monomorphization would duplicate
  identical code and `CodegenUnit`s across repeated sites for nothing.
  Drop glue is the in-tree precedent for exactly this keying — one
  synthesized instance per type, shared by every body that destroys it. `Err(e)` renders the payload; `None`
  reports the site alone. A future Display-style trait supersedes the
  synthesized printer without changing the record's shape. The net effect
  is deliberate: this is exactly the `match`-plus-`@assert` ceremony a
  `?`-less body would force at every fallible call — ADR-0038's must-check
  linearity makes the match compulsory, and without a Display trait the
  hand-written `Err` arm degrades to a static message — machine-written,
  with the payload rendered and the span pinned. One consequence is
  *accepted*, not mitigated: **trapping skips ordinary cleanup.** A
  propagating `?` lowers to an early return (spec 4.15:7 — the failure arm
  is literally `return None`/`return Err(e)`), and return paths run drop
  elaboration for live bindings, so a destructor that flushes a buffer or
  releases an external resource would run; the trap at the `?` site does
  not. Trapping is still chosen, because this is the posture every other
  failure path in this design already takes — `@assert`, `@panic`, and
  every trap skip destructors identically — so making `?` uniquely run
  them would be the inconsistency rather than the fix, and a test that
  fails is a test whose cleanup was already forfeit. The loss is bounded
  by the abort-only runtime: the process dies at once, the OS reclaims
  descriptors and memory, and the retained scratch directory preserves
  on-disk state for post-mortem. What is genuinely given up is a
  `drop fn`'s own observable work on the failing path. Recorded under
  Consequences. Expected-failure
  *testing* is unaffected: asserting that a call returns `Err` is still an
  ordinary match on the value; `?` is for the errors a test does not
  expect.
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
  mismatch.

  The inventory must therefore be supplied, and it must be an inventory of
  files *declared to the build* — which `--source-manifest` is not. The
  `rue_program` manifest is derived, not declared: `scripts/rue-program-derive-manifest.py`
  writes the scan's accepted reads (validated to fall within `srcs ∪ std`)
  unioned with every file of the declared std tree. Declared `srcs` enter
  only as the gate that *rejects* an out-of-srcs read. So the manifest gets
  both directions wrong for this purpose: a newly declared but unimported
  `foo_tests.rue` was never read, is therefore absent from the manifest,
  and is exactly the orphan this design promises to catch — while every std
  file is present, so a scan over manifest entries outside the closure would
  sweep the entire toolchain as root-owned test candidates. Using the
  generated source manifest as the candidate inventory would deliver a
  warning that fires on std and stays silent on the one mistake it exists
  to catch.

  The candidate inventory is the **declared `srcs` set**, passed explicitly
  — `--test-candidates <list>` (spelling illustrative; the flag is a Phase 2
  naming call), which the `rue_program` rule feeds from `ctx.attrs.srcs`,
  the same list it already writes as `srcs.list` for the derive step. The
  set difference this warning needs is one the build already computes:
  `scripts/rue-program-srcs-precision.py` reports `srcs` minus the scan's
  accepted reads as its advisory over-declaration report. Orphan-test
  detection is that same difference, narrowed to entries that parse as
  containing test items. Reusing the shape rather than the artifact is
  deliberate: the precision report is an optional build validation over one
  target's paths, while this warning is a compiler query over published
  content.

  Its caveat carries over and is why this stays a warning: sibling roots
  legitimately share a `srcs` glob, so one root's unread file may be another
  root's whole tree, and unread-ness alone is not orphan-ness. An inventory
  belongs to one root; a file declared to this root, containing test items,
  and outside this root's closure is *reported*, never an error of the
  request.

  One publication step is honestly new. The compiler cannot parse what the
  host never published, and an out-of-closure candidate's content never
  reaches the snapshot today. Phase 2 therefore adds a bounded
  candidate-acquisition step to ADR-0063's host input protocol: for each
  declared candidate outside the demanded closure, the host reads and
  publishes the entry's bytes and content fingerprint — or a typed
  absent/unreadable outcome — as ordinary revisioned inputs, demanded only
  by test requests, and the orphan check is a parse-only query over those
  candidates that never turns one into a semantic root. A parse failure in a
  candidate is reported inside the warning, never as a compile error of the
  request. Absent entries stay silent (a declaration is not a claim that the
  file exists — the loader's own posture today); unreadable entries are
  reported inside the warning. Naming the protocol is the point: without it,
  "canonical query over the inventory" would quietly become a driver-side
  read and side table — the peer-computation shape this bullet exists to
  avoid. Without a candidate inventory there is no scan and no warning; the
  run summary instead carries a one-line notice that orphan-test detection
  needs one.
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
         [--seed N] [--keep-going] ...
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
  `test_started` (stable ID — without it a consumer cannot attribute hangs or
  render progress), `test_finished` (stable ID, verdict, duration, capability
  summary, failure structure, captured stdout/stderr, exact reproduction
  argv), `run_finished` (counts, wall time). Verdicts: `pass`, `fail`,
  `timeout`, `crash` (killed by signal), `skipped` — with `skipped` carrying
  no producing mechanism in any MVP phase as written, which is an open
  question below rather than a settled v1 member; `compile_error` (as a
  per-test verdict, §3) and `cached_pass` are reserved in the schema for the
  deferred work (§6) and are not producible in the MVP, whose whole-run
  compile failure is exit code `2`. A failure record is data: failure kind
  (`assert` / `unhandled_error` / `trap:<class>` / `exit` / `signal` /
  `timeout` / `output_overflow` / `ice`), the pinned runtime message (the
  abort-only runtime's fixed stderr strings are machine-recognizable by
  construction), exit code or signal, and a source location — in the MVP, the
  test declaration's span, except `unhandled_error`, whose record carries the
  failing `?` site (§1). The record's payload and location fields are
  extension points, not closed shapes: richer expected/actual payloads and
  failing-call-site locations arrive through the structured failure channel
  (§5.1) as additive schema minors, never by parsing prose. One sequencing
  state is pinned now rather than discovered later: the `capability_summary`
  field is present from v1.0 with an explicit status discriminator —
  `{"status": "unavailable"}` throughout the MVP, which ships zero capability
  claims, replaced by the populated `available` form when the deferred
  capability-inference ADR lands (§6), an additive change inside a field
  consumers already handle rather than a retrofitted optional. `--list`
  output carries the same state, so neither surface ever contradicts the MVP
  or guesses at an absent field's meaning.
- **Captured output is bytes, budgeted** — v1 schema obligations, pinned in
  the Phase 2 schema doc. Rue strings may carry arbitrary non-UTF-8 bytes and
  the runtime writes them to the pipes raw, so captured streams cannot be
  assumed to be JSON-safe strings: output fields carry an explicit encoding
  tag — UTF-8 when the bytes validate, base64 otherwise — and are lossless
  within the retained window. Capture is bounded per stream *as bytes arrive*
  (the `rue-test-runner` mechanics already include the limited-drain variant
  alongside the unbounded one; Phase 2 adopts the limited variant), so a fast
  writer cannot consume unbounded runner memory inside its wall-clock budget;
  exceeding the per-stream limit kills the process group and yields a `fail`
  verdict with failure kind `output_overflow`, retained prefix attached.
  Verbosity asymmetry (below) applies to payloads too: a failing test's event
  carries the retained capture inline; a passing test's event carries digests
  and byte counts only, with a flag to opt passes into inline capture. The
  §5.1 structured failure channel is carried and budgeted separately from
  user output, so a test that floods its streams cannot truncate its own
  failure record.
- **Asymmetric verbosity**: the default human renderer prints failures in
  full — structured failure, captured output, repro line — and passes as a
  count. No wall of green. The human renderer is implemented as a consumer of
  the same event stream it would emit under `--format json`, which keeps the
  machine surface honest.
- **Discovery without execution**: `rue test <root> --list --format json`
  emits the inventory — IDs, declaration spans, and the `capability_summary`
  unavailable state — without running anything, performing semantic analysis
  of test closures but no codegen, no linking, and no execution. Two
  additive surfaces are reserved on this listing rather than shipped. Cache
  status arrives with the deferred verdict cache (§6) as an explicitly
  costlier request (`--list --cache-status`, spelling illustrative): cache
  status is decided by closure fingerprints built from ADR-0063 *terminal*
  artifacts — the per-function `CodegenUnit` (ADR-0063 §11) — so reporting
  it materializes the closure's codegen artifacts, cheap warm, a full
  closure codegen cold, and never merely "semantic analysis"; the default
  listing must never pay that, and consumers that only want the inventory —
  the common agent case — pay nothing for a field they did not ask for. And
  the *inverse* query is reserved in the same surface: "which tests reach
  item X" is RUE-506's tests-for-function question and the sound, static
  version of what dynamic test-impact tools sell — the per-root reached
  sets will already exist, so `--list --reaches <item>` (spelling
  illustrative) is nearly free and the schema reserves reached-set
  provenance for it from v1.
- **Filtering**: `--filter` matches against test IDs (module path and name);
  repeated filters union. A filter selecting zero tests is an error with a
  distinct exit code, adopting the spec-suite principle that an empty
  selection is how a typo becomes false evidence.
- **Exit codes** (proposed): `0` all selected tests passed, `1` at least one
  test failed, `2` compilation or runner error, `3` empty selection.
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
tests**, a claim that becomes available with the deferred
capability-inference ADR (§6), whose summaries prove a test cannot reach a
channel that touches another test; the MVP verifies nothing and therefore
claims this for no test. A test holding `syscall` or `ffi` can, by
definition, signal arbitrary processes, mutate shared absolute paths,
contend on ports, or spawn descendants that outlive it — and a process
group is not containment, because a child that calls `setsid` leaves it.
Process isolation plus a private scratch directory narrows the accident
surface well below every mainstream runner's baseline, but it is not a
sandbox, and this ADR does not pretend otherwise. Runs that need enforced
noninterference for unverified tests await a real platform sandbox
(future work behind this same contract) or the deferred scheduling
controls (§6).

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
  it is excluded, by construction, from the capability summaries and
  closure fingerprints the deferred ADRs compute (§6) — recorded now so
  neither inherits dispatcher noise.
- **One process per test invocation.** The runner spawns the image once per
  test under the loader-visible exec contract defined below (constant
  `argv[0]`, fixed-width selector, pinned environment), in its own process
  group, with a per-test wall-clock timeout (default 10 s, matching
  `rue-test-runner`), SIGKILL to the group on expiry, and reader threads
  draining both pipes with the per-stream capture bound of §2 (the mechanics
  `rue-test-runner` already has, including the limited-drain variant; Phase 2
  extracts and reuses them rather than reimplementing). After the leader
  exits normally, the runner additionally SIGKILLs the remaining process
  group: lifecycle hygiene that reaps stragglers holding pipe fds, documented
  as exactly that — the contract above already concedes it is not
  containment. Timeout kills, signal deaths (including SIGPIPE's status 141),
  and exit 101 with a pinned trap message are distinguished in the verdict,
  and ICE detection remains a separate failure class no marker can absorb.
  Two contract details the mechanics must honor: the runner keeps both pipes
  open and drained until the child exits — a stdout-writing test must never
  die with SIGPIPE because a reader closed early, since the deferred
  capability lattice classifies `stdout` as hermetic-compatible (§5.1 records
  the same classification for the failure channel) — and a body that calls
  `std.exit(0)` before its assertions is an accepted blind spot shared by
  every process-based runner; the dispatcher may later distinguish "returned"
  from "exited" (an epilogue sentinel on the structured channel) if it proves
  worth closing.
- **Execution environment contract** (Bazel's encyclopedia, enforced by
  construction where possible). The inventory a test can observe is defined
  separately from the runner's plumbing, because the runtime captures the
  loader-provided `argc`/`argv`/`envp` unchanged at entry and exposes them
  through `std.env` — without a defined boundary, a dispatched process would
  leak the real image path, the internal selector, and per-run scratch paths
  into test-visible state: values that vary while the body's closure does
  not, poisoning either the deferred cache key (§6 — if fingerprinted,
  routine hits vanish) or its soundness (if not, cached verdicts can be
  stale). The **test-visible inventory** is pinned to exact values: argv is
  fixed and documented (a stable logical `argv[0]`, no selector, no image
  path); the environment is a fixed ordered list of exact `KEY=VALUE`
  entries, with runner-set `RUE_TEST_*` variables carrying stable logical
  values — the scratch directory is always spelled `.`, which is the fresh
  private working directory each test starts in (deleted on pass, retained on
  failure for post-mortem); and stdin is a fixed EOF stream unless a future
  explicit input joins the test's identity. The dispatcher consumes the
  selector and replaces the runtime's captured inventory with the pinned one
  before invoking the body, so `std.env.args()` inside a test observes the
  contract, not an incidental internal protocol — which is also what makes
  tests *of* `std.env` meaningful. That replacement is the *visibility*
  boundary, and it is not by itself a *resource* boundary: the loader lays
  the real argv and environment strings out on the initial process stack
  before `main` runs — the runtime captures exactly those loader vectors at
  entry — so their byte size is stack consumption no later pointer swap can
  undo, and a varying real `argv[0]` (the image path, under a naive `image
  --run <id>` launch) would let a near-limit recursive test cross the stack
  boundary with no keyed input changing. The **loader-visible inventory** is
  therefore pinned too: the runner execs every test process with a constant
  `argv[0]`, a fixed-width selector (index- shaped, never a path), and
  exactly the pinned environment vector, and it launches the image through a
  run-constant path spelling (a constant-named link in the per-test working
  directory), so loader-injected path strings — `AT_EXECFN` and its macOS
  analogue live on the same stack — are constant bytes as well; the remaining
  auxiliary-vector entries are fixed-size by platform contract. Initial-
  stack consumption is then deterministic per keyed configuration, which is
  what makes a pinned `RLIMIT_STACK` (runner policy the deferred caching ADR
  will key on, §6) an actual determinism boundary rather than a bound over a
  varying baseline. (An inherited control fd is the recorded alternative
  selector transport.) With the environment pinned at exec time, the
  dispatcher's replacement reduces to argv normalization — consuming the
  selector and presenting the documented logical `argv[0]`. The exact visible
  values — ordered environment entries, argv values, stdin policy, and the
  loader-visible constants — participate in the deferred verdict-cache key
  (§6), not merely an allowlist of names: exact values are what will keep
  cached verdicts sound, and their stability is what will keep routine runs
  cache-hittable.
- **Parallelism**: the runner schedules up to `--jobs` test processes
  concurrently. In the MVP every test runs in parallel by default — per the
  scoped contract above a pragmatic default, not a guarantee: unverified
  tests *can* interfere through the OS, and a suite that observes it reaches
  for `--jobs 1` today, declared serial groups in the deferred scheduling
  work (§6), and a platform sandbox eventually. When capability inference
  lands (§6), verified-hermetic tests become non-interfering by proof and
  their parallelism unconditional. The runner never silently serializes on
  inference; scheduling changes are always visible policy.
- **Reproduction as data**: every failure event carries the exact argv to
  reproduce that single test under the same seed, target, opt level, and
  filter — copy-paste (or agent-invoke) ready.
- **Per-test compile failure is a verdict, not a run abort** (deferred,
  §6).
  Because each test's closure is analyzed independently as a root, a semantic
  error in one test's closure can yield a `compile_error` verdict carrying
  those diagnostics while every other test still builds into the image and
  runs. The mechanism is exclusion, not stubbing: failed closures simply do
  not enter the image, and their tests report `compile_error` with the
  diagnostics attached. The MVP keeps the simpler whole-run-fails behavior;
  the per-test contract is specified now so nothing in Phase 2 bakes in the
  coarser one.

  **Which copy of a diagnostic is authoritative** has to be answered, because
  this is the one place the design puts the same information on two
  guaranteed surfaces. §2 keeps compiler diagnostics on stderr exactly as
  today, and `docs/process/diagnostics.md` guarantees that under
  `--error-format json` every diagnostic goes to stderr, nothing else does,
  and batch ordering is deterministic and pinned by CLI cases. Embedding
  those same diagnostics in a stdout `test_finished` event does not violate
  that guarantee — stderr stays exactly as specified, and the invariant is
  about what stderr contains, not about exclusive publication — but it does
  create a second copy, and a consumer needs to know which to believe. The
  disposition: **stderr remains the authoritative diagnostic stream**, byte-
  for-byte unchanged and independently versioned; the copy embedded in a
  `compile_error` event is an attribution convenience, carrying the same
  diagnostics already published on stderr so a stream consumer can attribute
  them to a test without correlating two streams. The event copy is
  therefore never the only place a diagnostic appears, and any divergence
  between the two is a bug in the runner rather than a schema question.
  `diagnostics.md` gains a short test-mode note recording exactly that
  when per-test verdicts land (§6); whether the event should instead carry
  only diagnostic *identities* (codes and spans, with the stderr batch as
  the sole payload) is deferred with them.
- **Future mechanisms behind the same contract** (deferred with
  scheduling, gated on its batching spike — §6): batching many
  verified-hermetic tests into one process with fork-per-test or
  rerun-on-abort recovery; comptime evaluation of pure test bodies remains
  a deferred design question, not a plan.

### 4. Determinism defaults

- The runner pins the loader-visible and test-visible inventories (§3) —
  exact argv values,
  ordered environment entries, stdin at EOF; `env`- and `args`-observing
  tests are therefore deterministic given values pinned by contract — and,
  later, in the deferred cache key (§6).
- There is no clock to virtualize; the absence is load-bearing. Any future
  time API must arrive behind a `clock` capability so this design's guarantees
  survive it (the standing constraints section that travels with the
  deferred capability ADR records this and its siblings, §6).
- `--seed N` is accepted and reported in `run_started` and in every failure's
  repro argv from Phase 2, but in the MVP it only feeds the runner's own
  choices (shuffle order, scratch naming). Making `@random_*` seedable in
  test builds — lowering to a seeded PRNG per test process — is a
  runtime/codegen change whose maintainer call is deferred with scheduling
  and caching (§6): it would make `random`-observing tests deterministic and
  hence cacheable, at the cost of test-mode divergence from production
  entropy.
- Test execution order within a run is shuffled by seed by default (verified
  isolation makes order dependence impossible for hermetic tests; shuffling
  keeps everyone else honest, and the seed makes any surprise reproducible).

### 5. Extensibility is tiered, in-language first, with no privileged built-ins

The built-in framework must be the default, not the ceiling: a future BDD
layer, cucumber-style step harness, assertion library, property tester, or
replacement runner has to be writable without this ADR being reopened. Four
seams, ordered by how much of the verified story each preserves. What is
deliberately *not* extensible: the verdict taxonomy's meaning, the isolation
contract (§3), and the eject-don't-degrade soundness posture the deferred
capability ADR ratifies (§6) — extensions change what tests look like and
how they report, never what "verified hermetic" will claim.

#### 5.1 Assertion libraries are first-class by protocol, not by blessing

The channel through which a failing test reports structure — failure kind,
message, expected/actual payload, failing-call-site location — is a
documented runtime protocol, not a privilege of blessed intrinsics. `@assert`
today, and `@assert_eq` when it arrives, are sugar over the same channel any
Rue function can invoke before aborting; a user assertion library emits the
same structured failure records the built-ins do, and the event stream
carries them without knowing who produced them. The test-body `?` failure arm
(§1) is another built-in writer of the same channel: its record carries the
failing `?` site and the synthesized structural rendering of the error
payload. The recommended mechanism is a **dedicated inherited pipe**: its own
file descriptor, pinned in the §3 exec contract, written through a runtime
helper (an ABI-manifest addition under ADR-0055 rules, so the choice remains
a maintainer call) and drained by the runner with its own cap, independent of
the user streams. The rejected shape is a reserved framed region of stderr:
Rue streams are arbitrary bytes, so user output can reproduce any in-band
framing byte-for-byte, and a "separate budget" whose frames are extracted
from a shared, capped stream is separate in name only — making in-band
framing unambiguous would require escaping or authenticating user bytes,
complexity that buys nothing over a second pipe. The dedicated channel is not
a security boundary (unchecked code can write to any descriptor it can name);
it prevents *accidental* collision, which is exactly the promise §2 makes,
and it is the natural future carrier for the §3 epilogue sentinel and §5.2
sub-results. The channel is budgeted separately from user output (§2):
structured records carry their own size cap, so a test that floods its
streams cannot truncate its own failure record.

**The helper's capability class, stated rather than left implicit.** The full
proposal's standing rule — which travels with the deferred capability ADR
(§6) — says an unclassified manifest leaf is a soundness hole in every cached
verdict, and a new ABI helper is exactly such a leaf — so it is classified
here, not discovered later. "Trap is the verdict" covers `@assert` but does
not cover this channel: §5.2 sub-results are writes from a *running, possibly
passing* test, which makes it a real output channel rather than a terminal
one. The classification is **hermetic-compatible, on the same grounds as
`stdout`**: the descriptor is runner-pinned in the §3 exec contract,
everything written to it is captured by the runner, and its budget will join
the deferred cache key (§6) alongside the per-stream output limits — a record
truncated by that cap is a function of the test's own behavior and the keyed
budget, nothing ambient. It therefore introduces no new lattice bit and will
not eject a test from caching. The sequencing gap is real, and deferral
widens it: the helper ships in Phase 2 while the machine-checked manifest
capability field lands with the deferred capability ADR (§6), so until that
ADR the classification is carried by this paragraph and by the Phase 2 schema
doc; that ADR's manifest field must record it, and the ABI addition is a
maintainer call under ADR-0055 rules regardless. Two consequences are
deliberate: the failure payload is an open, versioned field rather than an
enum of built-in shapes, and location is carried *in* the record — so a
library can attribute its caller — rather than derived solely from the test
declaration. Automatic call-site capture wants a `@src()`-style comptime
intrinsic (deferred; nothing here blocks it); until then library-reported
locations are the library's responsibility. The record also reserves a
*promotion* payload from v1: a failure may carry a machine-applicable
suggested fix — the expect-test/snapshot pattern: new expected value, target
span, and a content hash of what it replaces — and a future `rue test
--accept` applies accepted promotions. The runner applying promotions, never
the test, is what keeps snapshot-style tests hermetic: the test process still
writes nothing. Reserving the field now is the cheap part; the accept verb
ships when a snapshot framework exists to use it.

#### 5.2 In-language frameworks are ordinary Rue code; comptime is the generator

A BDD vocabulary, a table-driven harness, a property tester's case machinery —
written in Rue, these are plain functions and comptime constructs used inside
test bodies from day one, and they will inherit capability inference, caching, and
selection automatically when the deferred layers land (§6), because their
helpers are reached bodies like any other. What v1 does not give them is per-case identity: one
`test` block looping over a table is one verdict, one filterable unit (and,
later, one cache entry). Two
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
- **Sub-results.** The §5.1 channel generalizes to a sub-result record: a
  running test may emit named child results with their own payloads, which the
  runner attributes as `<test-id>/<sub-name>` rows in the stream. Scheduling,
  caching, and selection stay at the item level — sub-results are reporting
  granularity, which is what table tests and `describe`/`it` nesting need
  first. Reserved in the schema from v1, implemented when demanded.

#### 5.3 Reporters and observers consume the stream

Custom reporters, CI adapters, dashboards, and IDE surfaces are NDJSON
consumers with no protocol negotiation. This works from Phase 2 and is the
intended default extension point; a JUnit adapter is the reference consumer.

#### 5.4 Alternative runners and external providers use documented contracts

A replacement *runner* needs no new privileges at any phase: enumerate with
`rue test <root> --list --format json`, execute through the test image's
documented argv/exit/stream contract — public by commitment from Phase 2, and
nothing in this ADR or the deferred work may depend on it staying private —
and schedule however it likes. The reverse direction, external test
*providers* (step harnesses, fuzzers, snapshot tools presenting tests the
compiler never saw), is the deferred provider protocol (§6, RUE-1624): a
versioned enumerate/execute-by-ID contract whose wire format must be decided
together with RUE-505's semantic-API format policy; committing to it now
would prejudge that discussion, so this ADR reserves the seam and defers the
contract. Provider-supplied tests are not compiler-visible bodies, so they
get no verified capability summaries: they run as unverified — always
executed, never cached — unless the provider generates real Rue test items
instead. Eject-don't-degrade applies to extensions exactly as it applies to
`@syscall`.

Fixtures deserve one honest note: setup is plain code in the test body and
teardown is destructors, but the abort-only runtime means destructors do not
run on a failing path — teardown-on-failure is process death plus the
retained scratch directory, which suffices for hermetic and fs tests alike.
Expensive fixtures *shared across* tests (a database, a compiled corpus) are a
runner-policy question — serialized groups (deferred scheduling, §6) plus future setup
commands — and are noted as not locked out rather than designed here.

### 6. What is deferred, and what keeps it additive

The full proposal this document was narrowed from designed four further
layers in detail. Each is deferred to its own future ADR with its own
evidence, spikes, and maintainer calls; the design capture — five review
rounds deep — lives in rue-language/rue#2239 and is seeded into the
follow-up issues below. Deferral is a ratification decision, not a
direction change.

- **Capability inference** (RUE-1621; original §4, Phases 3 and 6).
  Per-function capability summaries inferred bottom-up over projections of
  the existing ADR-0063 query families, grounded in the three effect
  chokepoints the language already has (the runtime-ABI helper manifest,
  `@syscall`, `extern "C"`). With no traits, function pointers, or threads,
  the call graph is total, so inference is sound today with FFI as the only
  opaque edge. Carries the highest-stakes maintainer call (summaries stay
  out of the type system), the RUE-967 `@ptr_to_int` disposition, and the
  "Constraints on future language evolution" standing section, which
  travels with it.
- **Hermetic verdict caching and selection** (RUE-1622; original §5, Phase
  4). Verified-hermetic verdicts as cacheable artifacts keyed on closure
  fingerprints plus the pinned execution values; `--changed-only` as the
  same predicate; allocation determinism via a test-build budgeted page
  mapper; per-test `compile_error` verdicts. Requires capability inference.
- **Scheduling and flake policy** (RUE-1623; original Phase 5). Declared
  serial groups, `--reruns` for non-hermetic tests, hermetic-mismatch
  reporting, duration-fed sharding, and the seedable `@random_*` maintainer
  call (§4). Two of its four items require capability inference.
- **The public provider protocol** (RUE-1624; original Phase 7). The
  versioned enumerate/execute-by-ID protocol for external test providers,
  decided together with RUE-505's format policy; JUnit and CTRF adapters as
  reference consumers.

What this ADR does now so those land additively, rather than as retrofits:

- The event schema carries `capability_summary` from v1.0 with an explicit
  `{"status": "unavailable"}` discriminator (§2); population is an additive
  change inside a field consumers already handle.
- The verdict taxonomy reserves `compile_error` (as a per-test verdict) and
  `cached_pass` without producing them, and the per-test compile-failure
  contract is specified in §3 so the MVP does not bake in the coarser
  whole-run-fails shape.
- The loader-visible and test-visible inventories (§3) are pinned to exact
  values — which is precisely what makes verdicts keyable later; the
  deferred cache key is a consumer of contracts the MVP already enforces.
- `--list` reserves the explicit cache-status tier and the
  `--reaches <item>` inverse query (§2) as additive surfaces.
- The structured failure channel reserves the promotion payload and
  sub-result shapes (§5.1, §5.2), and its capability classification
  (hermetic-compatible, on stdout's grounds) is stated in §5.1 so the
  deferred lattice inherits it rather than discovering an unclassified
  leaf.

## Implementation Phases

Filed in the Linear project "rue test MVP"; the deferred layers live in the
companion project "rue test follow-ups" (§6).

- [ ] **Phase 1: `test` declarations** - RUE-1618. Grammar/lexer/parser (the
      language's first contextual keyword, directives allowed), RIR item, a
      new `Test` kind in the closed stable-definition taxonomy plus its
      namespace decision, semantic analysis as ordinary bodies rooted only by
      test requests, the warnings-scan inclusion decision (§1), duplicate-name
      diagnostics, the test-body `?` legality rule (§1 — the additive spec
      rule where E0503/E0505 apply today; the failure-arm lowering ships with
      the Phase 2 runner), `test_declarations` preview feature, spec sections
      + spec-test coverage, UI coverage for the gate and diagnostics. No
      runner yet: `--emit`-level verification that test items parse, analyze,
      and are invisible to executable requests.
- [ ] **Phase 2: `rue test` MVP runner** - RUE-1619. Value-aware subcommand
      dispatch joining the existing mode-validation path; test-request root
      sets; synthesized dispatcher `main` and per-target test image through
      the image-planning path, plus the ADR-0061 facade work to expose a
      test-image request; the loader-visible exec contract (constant
      `argv[0]`, fixed-width selector, pinned environment vector,
      run-constant image spelling) with the dispatcher normalizing the
      runtime's captured inventory to the documented test-visible values
      (§3); process-per-test execution with process-group timeout/kill,
      bounded per-stream capture (the limited-drain variant), post-exit group
      cleanup, and pipes held open until child exit (mechanics shared with
      `rue-test-runner`); the unimported-test-file warning over a declared
      candidate inventory (`--test-candidates`, fed from `rue_program`'s
      `srcs` — explicitly *not* the derived `--source-manifest`, §1) with its
      bounded candidate-acquisition host-input step; `--list`, `--filter`,
      `--jobs`, `--shard`, `--timeout-ms`, `--seed` (shuffle), exit-code
      contract; NDJSON event stream v1.0 with schema doc (a new
      `test-events.md` under `docs/process/`), including the byte-safe output encoding,
      capture budgets, pass/fail payload asymmetry, and the
      `capability_summary` unavailable state (§2); the test-body `?`
      failure-arm lowering — synthesized structural error printers and
      `unhandled_error` records (§1); the structured failure-record channel
      contract (§5.1), the reserved promotion field, and the reserved
      identity/sub-result shapes (§5.2), with the human renderer as its
      consumer; repro argv in every failure; CLI-suite coverage end to end.
      Includes the memo-database pressure spike (Open Questions). **This
      phase is the MVP: usable, agent-first, zero capability claims — every
      test simply runs.**
- [ ] **Phase 2.5: structured assertion payloads** - RUE-1620. `@assert_eq`
      (and a minimal comparison family) as intrinsics producing
      expected/actual through the §5.1 channel; machine-computed diffs in
      `test_finished` events; human renderer output built from the same
      payloads. Pulled ahead of all deferred capability work deliberately:
      RUE-506 names unstructured failure output as the primary agent token
      sink, and a runner that is agent-first in transport but prose in
      content has not met the bar.

## Consequences

### Positive

- The MVP needs no capability system, no new persistence, and no
  spec-invasive machinery — it is a subcommand, a grammar item, a generated
  `main`, and a schema doc, on infrastructure that exists and is tested.
- The runner never promises what it cannot verify: the MVP makes zero
  hermeticity claims, states so in its own schema, and leaves verification
  to a follow-up that can prove it (§6).
- Agents get structure end to end: discovery without execution, failures as
  data with spans and repro argv, asymmetric verbosity, stable IDs, versioned
  schema — no reverse-engineering of prose at any layer.
- The design exploits what is genuinely unusual about Rue today — total
  static call graph, closed effect chokepoints, no clock, abort-only
  failures, fingerprinted demand-driven artifacts — instead of importing the
  compensating machinery other ecosystems needed.
- The event schema and execution contracts are forward-designed for the
  deferred layers (§6) and for the language features that will complicate
  them, so neither arrives as a retrofit.
- Extensibility has no privileged built-ins: assertion libraries share the
  built-ins' failure channel (§5.1), in-language frameworks are ordinary Rue
  code (§5.2), and a replacement runner can exist from Phase 2 using only
  documented contracts (§5.4).

### Negative

- One more consumer-visible versioned surface (test events) to maintain under
  ADR-0061 §6 discipline, plus a schema doc, plus CLI cases pinning it.
- Process-per-test puts a floor under per-test latency; "thousands of tests
  per second" claims wait for batching work deferred with scheduling (§6),
  which the abort-only runtime makes genuinely hard.
- Without verdict caching or selection, every run executes every selected
  test; the economic payoff RUE-506 wanted from compiler-informed selection
  waits for the deferred ADRs (§6). The MVP's answer is honest speed
  (parallel process-per-test on static images), not skipped work.
- A generated dispatcher `main` and test-image link per target adds a new
  compiler-synthesized artifact to maintain across both backends.
- Contextual-keyword parsing for `test` adds grammar subtlety (mitigated by
  its restriction to item position followed by a string literal).
- `?` in a test body skips destructors on the failing path, where a
  propagating `?` would run them (§1). This is uniform with `@assert`,
  `@panic`, and every trap rather than a new divergence, and process death
  plus the retained scratch directory covers resource reclamation — but a
  `drop fn` whose observable work is a flush or an external release does
  not perform it when a `?` fails.

### Neutral

- `scripts/rue test` (the maintainers' compiler-suite wrapper) and the
  user-facing `rue test` become homonyms; docs and AGENTS.md references need
  a disambiguation pass regardless of which rename option is taken.
- Tests may live beside code in the same file or in same-directory sibling
  files (spec 10.3 directory visibility) — production files need not carry
  test text. Test bodies in the import closure cost executable requests
  parse plus the syntactic warning-reference scan (§1), and no semantic
  analysis, codegen, or linking.
- If Rue ever gains catchable failures or unwinding, verdicts survive
  unchanged — they are defined by the execution contract (§3), not the abort
  mechanism — and failure reporting migrates into the structured channel
  (§5.1), which is mechanism-neutral by design. Destructors would then start
  running on failing paths, revisiting §1's accepted skipped-cleanup posture
  and the fixture note in §5.4. The full standing section on future language
  evolution travels with the capability-inference ADR (§6).

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
and §5.2 sub-results are the answers; destructor-based teardown does not run
on the trap path — process teardown plus the retained scratch directory
covers it (§3). The intrinsic is not a monopoly: §5.1 gives userland
assertion libraries the same failure channel, with `@src()`-style call-site
capture as the reserved gap-closer. Using `?` in test bodies for *expected*
fallibility (setup I/O, not assertions) is orthogonal and is adopted with
unwrap-and-report semantics in §1; the propagating alternative is rejected
below.

### Result-typed test bodies

Considered: typing test blocks as `Result((), E)` so `?` propagates out of
the body, with an `Err` return reported as the failure (the Rust shape); and,
as the null alternative, keeping bodies `()`-only with no `?` at all, every
fallible call hand-matched. Both are rejected in favor of §1's
unwrap-and-report `?`:

- **Propagation surrenders the failing site.** With no backtraces, an `Err`
  that walks out of the body reaches the dispatcher as a bare value: the
  report can name the test but not the line, where the trapping form pins
  the `?` site for free. Recovering the span under propagation would
  require instrumenting test-body `?` lowering anyway. What early return
  does buy, and the trap does not, is **drop elaboration**: the failure arm
  is an ordinary return (spec 4.15:7), so live bindings' destructors run.
  That is a real difference, not a wash, and §1 accepts it explicitly
  rather than claiming it away — every other failure path here already
  skips destructors, so uniform trap semantics is the coherent choice and
  the deterministic-cleanup argument is instead an argument for revisiting
  *all* failing paths if Rue ever gains unwinding (recorded as a Neutral
  consequence).
- **Propagation inherits the identical-`E` restriction.** Spec 4.15:4
  requires the enclosing producer's error type to be identical to the
  operand's — Rue has no error conversion until traits — so a
  `Result`-typed body could only `?` through one error type. The trapping
  form constructs no enclosing `Err` and carries no such constraint.
- **A signature surface with no other use.** Spec 4.15:4 keys off a
  *declared* return type, so `Result`-typed bodies need a return-type
  annotation on the test block — grammar and inference surface serving
  only this feature.
- **`()`-only without `?` taxes every fallible call.** ADR-0038's
  must-check linearity makes the match compulsory, and without a Display
  trait the hand-written `Err` arm cannot render the payload generically —
  in practice it degrades to `@assert(false, "setup failed")` and the
  error's content is lost. The adopted form is that same ceremony
  machine-written, with the payload rendered by the synthesized printer.

The cost accepted knowingly: `?` in a test body has different dynamic
semantics than `?` everywhere else. The mitigations: the position is a
compile error today, so nothing's meaning changes; the divergence is exactly
the failure arm, with the success arm untouched; and the trapping semantics
is the end state rather than a stopgap — even after error conversion lands,
propagating to the dispatcher would report strictly less than trapping at
the site, so no future revision wants the rejected shape back.

## Open Questions

### Maintainer calls needed (decisions this ADR takes a position on)

- **Test declaration surface**: `test "name" { }` blocks (recommended here,
  contextual keyword) vs `@test`-directive on ordinary functions vs `test fn
  name()`. Blocks match the language's Zig-adjacent shape and make the name a
  human sentence; a directive avoids any keyword question. Call needed before
  Phase 1.
- **Structured failure channel mechanism** (§5.1): a dedicated inherited
  pipe written through a runtime helper (recommended; touches the ABI
  manifest, so it is an ABI change under ADR-0055 rules) vs a reserved
  framed region of stderr (no ABI change, but rejected in §5.1: arbitrary
  user bytes can reproduce any in-band framing, so the separate-budget
  promise would need an escaping or authentication rule to be real). Shapes
  both the runtime surface and the event schema; call needed during Phase 2
  design, and it gates how soon userland assertion libraries reach parity
  with `@assert`.
- **Exit-code and `@assert` stabilization**: promote `@assert`/`@panic` from
  the reserved intrinsic bucket (4.13:5b) to normative, and decide whether
  assertion failure keeps exit 101 (shared with all traps, distinguished by
  pinned message — recommended, no runtime change) or gets a distinct code.
- **The `scripts/rue test` homonym**: rename the maintainer wrapper subcommand
  (e.g. `scripts/rue suite`), or accept the context distinction and fix docs.
- **Exit-code contract of `rue test`** (§2): the 0/1/2/3 proposal, in
  particular empty-selection-as-error. How a future per-test `compile_error`
  verdict maps onto exit codes is deliberately deferred with that verdict
  (§6, RUE-1622) — but the codes are pinned now, and agents will branch on
  them, so the proposal needs sign-off before Phase 2 ships.
- **Does `--filter` narrow the root set or only the run set?** (§2). As
  written, filtering selects which tests *run*, while the test request
  roots every test item in the import closure — so a broken, unselected
  test's closure still fails the compilation, and a filtered run cannot be
  used to work around a compile error elsewhere in the module. That may be
  the right semantics (it keeps a filtered run's verdicts identical to the
  same tests in a full run, which future selection soundness wants), but it
  is unstated, and the opposite reading — filter narrows the roots, so
  unselected tests are never analyzed — is what most users will assume from
  every other runner.
- **The `skipped` verdict has no producing mechanism.** §2 lists `skipped`
  in the verdict taxonomy, but no MVP phase produces one: `@skip` is
  deferred to the directive-grammar work and filtering removes tests from
  the selection rather than reporting them. Either a mechanism is named (the
  natural v1 candidate is platform scoping, which the compiler's own suites
  already have) or `skipped` should be reserved in the schema without
  appearing in the v1 taxonomy — an unproducible verdict in a published
  enum is a consumer trap.
- **`rue test` implicitly enabling the preview gate** (§1). The ADR has
  `rue test` enable `test_declarations` implicitly while the feature is in
  preview. That would be the first flag in the compiler to auto-enable a
  preview feature: ADR-0005's model is explicit `--preview <feature>`
  opt-in, with unflagged use producing a diagnostic that names the flag.
  The convenience is real — every test run would otherwise carry
  `--preview test_declarations` — but so is the precedent. The alternatives
  are requiring the flag like every other preview feature, or scoping
  auto-enable to test *requests* specifically and recording it in ADR-0005
  as a named exception.
- **Naming**: `test_declarations` preview flag; `--test-candidates` as the
  declared-candidate inventory flag (§1); a new `test-events.md` under
  `docs/process/` as the schema doc home.

### Questions that need a spike during Phase 2

- **Memo-database pressure under test roots**: a test request roots a strict
  superset of `main`'s closure; measure the ADR-0063 §14 retention budgets
  against check-all-shaped root sets on the larger example corpora. The §14
  calibration is `main`-rooted, so this is the *first* such measurement, not
  a validation of an existing one; the budgets are soft, so the failure mode
  under pressure is memory growth, not rejection.

The deferred layers carry their own maintainer calls and spikes — the
allocation-determinism mechanism, the `@ptr_to_int` disposition, seedable
`@random_*`, the verdict-cache key audit, batching mechanics, and the
analysis-only-capabilities ratification among them — recorded in the
follow-up issues (§6), not here.

### Deferred design questions

- Doctests: examples in docs testable by construction (RUE-504
  coordination); the protocol seam (§5.4) is where a doc-example provider
  would plug in.
- Per-test timeout/skip/xfail metadata (`@timeout(ms)`, `@skip`,
  `@known_bug("RUE-NN")` with XPASS-fails-loudly semantics inherited from
  the compiler's own suites), plus user-defined tags (`@tag("...")`) with a
  metadata map on test events (additive minor): wanted, but not free —
  directive arguments are identifier-only today, so literal-argument
  directives are a grammar and AST extension, not merely a scheduling
  choice. Deferred with that cost stated.
- Test items in comptime-instantiated types (§5.2) and the `@src()`-style
  call-site intrinsic (§5.1): both reserved as additive; scheduled when a
  real framework or assertion library demands them, not speculatively.
- Comptime evaluation of pure test bodies ("compiling is testing") — RUE-506
  Q4. Deliberately not planned: it blurs compile failure with test failure,
  and its incremental-cost story is unstudied; revisit with
  capability-inference evidence about how many tests are comptime-eligible
  (§6).
- Workspace/multi-root invocation, and whether `rue test` without a root
  argument should discover one (needs the package-model discussion,
  ADR-0047's successor).

## Future Work

The four deferred ADRs of §6 are the primary future work: capability
inference (RUE-1621), hermetic verdict caching and selection (RUE-1622),
scheduling and flake policy (RUE-1623), and the public provider protocol
(RUE-1624). Beyond those: structured assertion intrinsics beyond Phase 2.5's
comparison family, as sugar over the §5.1 failure channel; capability
declarations in types and at trait boundaries (a further ADR flagged from
the capability work); JUnit and CTRF adapters as reference stream consumers;
and test-aware `--watch` (rerun exactly the dirtied selection on save —
needs the deferred selection work plus the existing watch loop).

## References

- rue-language/rue#2239: the full agent-first test-runner proposal this ADR
  was narrowed from — five review rounds of design capture on capability
  inference, verdict caching, determinism, scheduling, and protocol design
  (head commit `0a0e3884ae74`, retained on the closed PR).
- RUE-506 (design capture this ADR supersedes in mechanism), RUE-505,
  RUE-504, RUE-438 (machine-readable interface project); RUE-967 (pointer
  provenance split, gating the deferred capability work's `addr` leaf).
- ADR-0063 §1/§3/§8/§15 (roots, fingerprints, reachability, test-selection
  consumer); ADR-0061 §6 (schema versioning policy); ADR-0058 (canonical
  artifacts); ADR-0055 (typed runtime ABI manifest); ADR-0064 (FFI boundary
  rules, accepted); ADR-0027 (random intrinsics); ADR-0025 (comptime);
  ADR-0069 (CI scheduling that names a test runner as future scope).
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
