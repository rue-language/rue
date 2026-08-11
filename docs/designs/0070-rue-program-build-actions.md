---
id: 0070
title: "Rue program compilation as declared Buck actions"
status: proposal
tags: [build, ci, testing, tooling]
feature-flag: null
created: 2026-08-11
accepted:
implemented:
spec-sections: []
superseded-by:
relates: ["RUE-1164", "RUE-1118", "RUE-1222", "RUE-1267", "ADR-0047", "ADR-0069"]
---

# ADR-0070: Rue Program Compilation as Declared Buck Actions

## Status

Proposal. This is the reviewed design RUE-1164 asks for and ADR-0069 Phase 5
defers to. Nothing here is implemented, and the granularity rule in
"Which compiles become actions" plus the five items under "Open questions" are
maintainer decisions rather than settled design.

## Summary

The compilation of a Rue program should be a first-class Buck artifact keyed on
its real inputs — root source, declared import closure, generated source
manifest, compiler build, flags, and target architecture — rather than a side
effect hidden inside a test process or an aggregate success stamp.

This ADR proposes two rules and one provider: `rue_program`, which owns exactly
one compilation and produces the executable; and `rue_program_test`, which
consumes that executable and runs one runtime scenario. Many scenarios share one
compile. It also proposes a granularity rule that keeps the action count
proportionate: a compile becomes its own action when it is expensive relative to
action overhead, or when more than one scenario consumes its output. The ~4,050
inline-source corpus cases stay inside their harnesses and get finer input keys
instead.

No compiler change is required. ADR-0047 Phases 3 and 4 (`--source-manifest`,
`--emit deps`) are implemented and currently unconsumed; this design is their
first client.

## Context

### Measurements

Taken on this tree with `//crates/rue:rue` built from 18def29. Absolute wall
clock is this host's and not CI's; the design rests on the ratios, which are
large enough to survive any host.

| Measurement | Value |
| --- | --- |
| `examples/caldera/main.rue` full compile | 243.0s (803 modules, 105k lines) |
| `examples/meridian/main.rue` full compile | 80.7s (266 modules, 36k lines) |
| `rue --emit deps` on caldera / meridian | 6.7s / 2.2s — 36x cheaper than compiling |
| Corpus cases in total | 4,132 (1,778 CLI, 2,109 spec, 245 UI) |
| CLI cases compiling a checked-in root | 73 cases across 13 distinct roots |
| Most-recompiled roots | mosaic 17x, rill 12x, lattice 7x, meridian 6x |

### Three defects, only one of which is about caching

**The artifact is a success bit.** `cached_corpus_suite` (RUE-1118) declares
`stamp.txt` as its output. A corpus action that compiles caldera produces no
compiled caldera, so nothing downstream can consume one and every other consumer
compiles it again. The rule is explicit about being a stamp; RUE-1164 is right
that a stamp cannot be the destination.

**The largest compiles were never in the mechanism.** The six
`large-example-{caldera,meridian}-{canary,slow,stress}` targets are plain
`rue_sh_test`s (`BUCK:397-447`). buck2 re-executes every test invocation — test
executions are not actions and never reach the action cache — so the largest
compilations in the repository re-run in full on every invocation that selects
them, cold and warm alike. `-slow` and `-stress` compile the same `main.rue`
root for each program and neither reuses the other's result.

**Runtime scenarios are welded to compiles.** `crates/rue-cli-tests/cases/`
`examples_meridian.toml` declares six cases against
`source_path = "examples/meridian/main.rue"` with no `args` override. They differ
only in `program_args` — `demo`, `run query.sql`, `run schema.sql`,
`run unknown.sql`, `selftest`, `stress1`. The harness gives each case a fresh
temp directory and its own compiler invocation, and `rue-cli-tests` performs no
compile reuse anywhere, so testing six command-line behaviours of one binary
costs six full 80-second compiles.

The third is the shape of the whole problem. The case model already separates
compile inputs (`files`, `source_path`, `args`, `env`) from runtime scenario
(`program_args`, `program_env`, `stdin`, expectations). Only the execution
topology is monolithic.

### What the compiler already provides

`--source-manifest` (ADR-0047 Phase 3) restricts import resolution to a
line-oriented declared set and fails closed. Verified on this tree with a
two-file program whose manifest omitted the imported module:

```text
error: [E1400]: invalid compiler input: import candidate
       '/tmp/hermetic-test/helper.rue' is not listed in the source manifest read policy
 --> main.rue:1:16
  |
1 | const helper = @import("helper.rue");
  |                ^^^^^^^^^^^^^^^^^^^^^
exit=1
```

`--emit deps` (ADR-0047 Phase 4) emits a JSON dependency envelope whose
`accepted_reads` array is the compiler's observed read set with canonical paths
and content fingerprints — 295 entries for meridian — plus a `topology` of
resolution outcomes and an `observations` list of every probe, including the ones
that missed.

## Decision

### Two rules and one provider

`rue_program` owns exactly one compilation. Its action is a `ctx.actions.run`
with category `rue_compile` and `allow_cache_upload = True`. Its inputs are the
root source, every declared source, the standard library, the compiler binary via
`$(exe_target //crates/rue:rue)`, and the manifest the rule generates; its
command line carries the flags and target, so those key the action too.

```python
RueProgramInfo = provider(fields = [
    "executable",      # the compiled artifact
    "manifest",        # generated source manifest (the declared closure)
    "root",            # root module
    "rue_target",      # x86-64-linux | aarch64-linux | aarch64-macos
    "opt_level",
    "runs_natively",   # False for a cross-target program
])

rue_program(
    name       = "meridian",
    root       = "examples/meridian/main.rue",
    srcs       = glob(["examples/meridian/**/*.rue"]),
    std        = "//:std",
    rue_target = "x86-64-linux",
    opt_level  = "0",
)

rue_program_test(
    name            = "meridian-selftest",
    program         = ":meridian",       # consumes RueProgramInfo — no recompile
    program_args    = ["selftest"],
    data            = ["examples/meridian/demo.sql"],
    stdout_contains = ["selftest checks=24", "valid=true"],
    tier            = "slow",
)
```

`rue_program_test` is an ordinary test target carrying exactly one tier label, so
`test_tiers.bxl` validation and the `rue_heavy_suite` discovery contracts keep
working unchanged.

### Hermeticity is enforced in-band

`rue_program` generates the source manifest from `srcs` and passes it as
`--source-manifest`. Membership in the declared set therefore becomes
load-bearing on every invocation — local and remote, cold and warm. An
under-declared `rue_program` cannot silently pass, because it cannot build.

This is deliberately stronger than ADR-0069's proposal to treat remote execution
as the undeclared-input detector. RE is a good detector but a partial one: it
catches only what RE actually runs. `corpus.bzl`'s header warns that under a
cached action an undeclared input becomes a false pass rather than an untracked
re-run; a generated manifest removes that hazard by construction rather than by
scheduling.

### Which compiles become actions

There are 4,132 corpus cases. Reading "every compilation is an action" naively
produces 4,132 actions, most compiling a ten-line program in milliseconds behind
action overhead, cache-key computation over the whole standard library, and —
under RE — a network round trip. That would be slower than today. It is the
failure mode this class of migration usually dies of, and avoiding it is the
load-bearing judgement in this ADR.

**Rule: a compile becomes its own action when it is expensive relative to action
overhead, or when more than one scenario consumes its output.** Everything else
stays inside a harness process whose input key gets tightened instead.

| Work | Scale | Disposition | Why |
| --- | --- | --- | --- |
| caldera, meridian — `main`, `canary`, stress roots | 6 roots | `rue_program` | 81–243s each, consumed by 5–6 scenarios apiece and by three suites |
| `source_path` CLI cases | 73 cases, 13 roots | `rue_program` | 13 compiles instead of 73 |
| Auto-discovered `examples/**` programs | ~43 roots | `rue_program` | Same roots reached again by reproducibility and frontend-diff |
| Reproducibility fixture roots | 5 | `rue_program` | Relocation and perturbation are runtime scenarios over declared compiles |
| Inline-source CLI / spec / UI cases | ~4,050 | harness | Sources live inside TOML, compiles are milliseconds; make the key finer, not the graph |

For the harness bucket the remedy is input precision rather than action count.
Today a one-line edit to `cases/modules.toml` invalidates all four CLI shards,
because every shard declares the whole `cases` filegroup. Declaring per-shard
case-file sets makes a case edit invalidate the one shard that owns it.

### What contributes to the action key

RUE-1164 enumerates these; each maps to a mechanism.

| Required in the key | Mechanism | Notes |
| --- | --- | --- |
| Root source | action input | `attrs.source()` |
| Transitive imports | action inputs | declared `srcs`; audited below |
| Source manifest | generated input | `ctx.actions.write`; changes when `srcs` changes |
| Compiler build | action input | `$(exe_target //crates/rue:rue)`; release vs debug already distinct via `//platforms:*` |
| Compiler flags | command line | `-O`, `--preview`, `--linker`, `--link-archive` |
| Target architecture | command line | `--target`; see open question 1 |
| Runtime inputs | test-target inputs | `data`, `stdin`, `program_env` on `rue_program_test` |
| Expected outputs | test-target attrs | expectations key the test, not the compile — which is the point of the split |

### `--emit deps` as a declaration auditor

The manifest makes under-declaration impossible. It does nothing about
over-declaration, the quieter defect: a `rue_program` whose `srcs` glob is wider
than its real import closure takes cache misses on files it never reads. Left
unchecked, a broad glob turns a precise action back into a corpus stamp.

A `rue_program_declaration_audit` target runs `rue --emit deps` and compares
`accepted_reads` against the declared `srcs`, failing when a program declares
inputs it provably never reads. At 2.2s against meridian's 80.7s compile the
audit is affordable as an ordinary target.

### Negative controls

A hermeticity claim nobody executes decays, so each control is a real target.

1. **Under-declared program must fail.** A fixture `rue_program` whose `srcs`
   deliberately omits one imported module, asserted to fail with `E1400`. A
   build-system compile-fail test; runs everywhere.
2. **Clean-root / remote materialization.** Build the `rue_program` targets under
   the existing no-fallback `//platforms:remote_execution` platform with
   action-cache reads disabled, as the RUE-320 merge-group canary already does.
   RE materializes only declared inputs, so an undeclared read fails with
   file-not-found rather than passing.
3. **Digest sensitivity.** Mutate one declared source and assert the action
   re-executes; mutate an undeclared neighbour and assert it does not. This is
   the direct test of the property the design claims, and nothing tests it today.

Remote test-result caching stays off until 1–3 pass on a real branch. RUE-1164
makes that ordering an acceptance criterion and this ADR keeps it strictly.

## Implementation Phases

Ordered so the first shippable phase is the biggest measurable win and each phase
is independently revertible.

- [ ] **Phase 0: Rules and controls** — `rue_rules.bzl` with `rue_program`,
      `rue_program_test`, `RueProgramInfo`, manifest generation, and the three
      negative controls. No existing target changes.
- [ ] **Phase 1: Large examples** — convert the six `large-example-*` `sh_test`s.
      One `rue_program` per (program, root); each scenario in
      `scripts/run-large-example.sh` becomes its own `rue_program_test`, retiring
      the script. Largest, entirely uncached, currently duplicated between the
      slow and stress tiers.
- [ ] **Phase 2: `source_path` CLI cases** — the 73 checked-in-root cases across
      13 roots, meridian's six first. Depends on open question 4.
- [ ] **Phase 3: Corpus input precision** — per-shard case-file declaration.
      Independent of Phases 1 and 2 and safe to reorder.
- [ ] **Phase 4: Test-result caching decision** — only after the negative
      controls pass on a real branch. See open question 3.

Linear issues are filed per phase under RUE-1164 once this ADR is accepted.

## Consequences

### Positive

- Large programs compile once per compiler and architecture, and the result is a
  real artifact many scenarios consume.
- Warm merge-queue runs serve Rue compilation from BuildBuddy rather than
  re-running it inside a harness.
- Undeclared inputs become build failures instead of false passes, everywhere,
  not only where RE runs.
- Scenarios become individually selectable, schedulable and shardable targets,
  which is what RUE-1267's packer needs and an opaque harness cannot offer.
- ADR-0047 Phases 3 and 4 acquire their first consumer.

### Negative

- Two new rules and a provider are build-system surface area that did not exist,
  and `rue_program` targets must be maintained alongside the programs they
  compile.
- Migrating `source_path` cases out of TOML splits the CLI authoring surface
  between two mechanisms during Phases 2 and 3.
- A `rue_program` whose `srcs` glob drifts wider than its import closure degrades
  quietly into a coarse key; the declaration audit exists to catch that, and is
  itself a target somebody must keep green.

### Neutral

- The compiler is a universal dependency, so a compiler change still invalidates
  every `rue_program`. This ADR does not change that regime and does not claim
  to; ADR-0069 measures it as roughly three quarters of changes.

## Open Questions

1. **Target architecture: attribute or configuration?** `//platforms:release`
   exists because a bare modifier does not change a configured target's hash
   (RUE-277), and the same argument could be made for a `//constraints:rue-target`
   setting plus platforms. Recommendation: a plain attribute. The Rue target is a
   property of the artifact requested, not of the machine building it, and one
   package legitimately wants x86-64 and AArch64 programs side by side, which
   configurations make awkward. The flag appears on the command line, so it keys
   the action either way; RUE-277's problem was a modifier that keyed nothing.
2. **How far to push corpus input precision.** Per-shard case-file declaration is
   clearly right. Splitting `//:std` so a change to one module does not
   invalidate every suite is a real cost/benefit call. Recommendation: stop at
   per-shard — splitting std chases a change class ADR-0069 measures as rare and
   buys complexity in the filegroup that most needs to stay obviously correct.
3. **Test-result caching has no `sh_test` path.** ADR-0069 notes that buck2
   carries `supports_test_execution_caching` on `ExternalRunnerTestInfo` and that
   Rue's noop toolchains mean nothing honours it. Reading the bundled prelude for
   the pinned 2026-07-15 buck2: the attribute exists only on the Java, Kotlin,
   Python, C++ and Android test rules, and `sh_test_impl` never passes it — so
   even with a non-noop test toolchain, no `sh_test` or `rue_sh_test` in this
   repository can opt in. Enabling it means writing a Rue test rule that emits
   `ExternalRunnerTestInfo` with the field set and replacing the noop toolchains.
   Recommendation: do not. Once `rue_program` owns the expensive half, what
   remains in a test execution is running a binary and comparing output; caching
   that is a large toolchain change for a small remainder. Revisit only if
   Phases 1–3 leave test execution on the critical path.
4. **Does the CLI harness consume prebuilt artifacts, or do cases migrate out?**
   Either `rue-cli-tests` learns to accept a prebuilt executable for a
   `source_path` case, or those 73 cases become `rue_program_test` targets. The
   first preserves one authoring surface and the RUE-924 corpus-omission audit;
   the second gives the per-scenario targets the milestone is for.
   Recommendation: migrate them out — they are already the least TOML-shaped
   cases in the corpus, being a root and a scenario with no inline sources. This
   is the decision with the largest blast radius on authoring ergonomics.
5. **Stale remote-execution guidance.** `AGENTS.md` and the `./buck2` wrapper
   still say not to use `--prefer-remote` "while RUE-320 remains open", while
   `docs/process/build-cache.md` documents remote execution as supported and a
   merge-group RE canary already runs. ADR-0069 flagged the contradiction and
   left it; negative control 2 depends on which is true. Recommendation:
   reconcile inside Phase 0.

## Non-Goals

- **Specification traceability stays independent of action caching.**
  `//:spec-traceability` runs `rue-spec --traceability` over the case corpus and
  `docs/spec/src`. It is not a compilation, must not become a scenario over one,
  and keeps its own plain test target. RUE-1164 makes this an acceptance
  criterion and this ADR reads it as load-bearing.
- **No compiler changes.** `--source-manifest` and `--emit deps` both exist and
  behave correctly. If this design needs a compiler change, something in it is
  wrong.
- **No change to what is tested.** Every scenario that runs today runs after; the
  conversion is topology, not coverage. The RUE-924 corpus-omission audit and the
  tier-labelling contracts must survive each phase.
- **Not a package system.** ADR-0047 leaves package resolution outside the
  compiler; `rue_program` is a build rule over resolved inputs, not a step toward
  one.

## References

- ADR-0047 — root-module compilation units and build-system inputs
- ADR-0069 §4 and Phase 5 — CI work scheduling for a compiler monorepo
- `docs/process/build-cache.md` — remote cache and execution contracts
- `corpus.bzl` — RUE-1118's cacheable corpus actions and their input contract
- RUE-1164, RUE-1118, RUE-1222, RUE-1267
