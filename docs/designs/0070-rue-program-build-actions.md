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
relates: ["RUE-1164", "RUE-1118", "RUE-1222", "RUE-1267", "ADR-0047", "ADR-0051", "ADR-0069"]
---

# ADR-0070: Rue Program Compilation as Declared Buck Actions

## Status

Proposal. This is the reviewed design RUE-1164 asks for and ADR-0069 Phase 5
defers to. Nothing here is implemented. The granularity rule under "Which
compiles become actions" and the four items under "Open questions" are maintainer
decisions rather than settled design.

## Summary

The compilation of a Rue program should be a first-class Buck artifact keyed on
its real inputs — root source, declared read closure, generated source manifest,
compiler build, flags, target architecture, and any linked archives — rather than
a side effect hidden inside a test process or an aggregate success stamp.

This ADR proposes two rules and one provider: `rue_program`, which owns exactly
one compilation and produces the executable; and `rue_program_test`, which
consumes that executable and runs one runtime scenario. Many scenarios share one
compile. The manifest that makes the compile hermetic is derived from a cheap
`--emit deps` scan rather than from the `srcs` glob, because a glob provably
cannot produce a valid manifest. A granularity rule keeps the action count
proportionate: a compile becomes its own action when it is expensive relative to
action overhead, or when more than one scenario consumes its output. The ~4,050
inline-source corpus cases stay inside their harnesses.

No compiler change is required. ADR-0047 Phases 3 and 4 (`--source-manifest`,
`--emit deps`) are implemented and currently unconsumed; this design is their
first client, and the scan-derived manifest is what makes both of them load-bearing
at once.

## Context

### Measurements

Taken on this tree with `//crates/rue:rue` built from 18def29. Absolute wall
clock is this host's and not CI's; the design rests on ratios and on which lane
pays a cost, not on the absolute seconds.

| Measurement | Value |
| --- | --- |
| `examples/caldera/main.rue` full compile | 243.0s (803 modules, 105k lines) |
| `examples/meridian/main.rue` full compile | 80.7s (266 modules, 36k lines) |
| `examples/caldera/canary.rue` full compile | 2.3s |
| `examples/meridian/canary.rue` full compile | 3.1s |
| `rue --emit deps` on caldera / meridian | 6.7s / 2.2s — 36x cheaper than compiling |
| Corpus cases in total | 4,132 (1,778 CLI, 2,109 spec, 245 UI) |
| CLI cases naming a checked-in root | 73 cases, 13 roots, 17 distinct (root, flags, target) tuples |
| Most-recompiled roots | mosaic 17x, rill 12x, lattice 7x, calculator 7x, meridian 6x, jsonfmt 5x |

**Where those costs are actually paid matters, and it is not where you would
guess.** The 243.0s and 80.7s figures are `main.rue` compiles, which occur only in
`release.yml`: the slow tier on a daily schedule, and the stress tier on
`workflow_dispatch` only. The required pull-request and merge-queue path compiles
only the two `canary.rue` roots, at 2.3s and 3.1s. The heavy corpora that were
91–97% of the merge-queue critical path are already cached by RUE-1118. This ADR
is therefore not a critical-path optimization, and the phase ordering below says
so plainly.

### Three defects, only one of which is about caching

**The artifact is a success bit.** `cached_corpus_suite` (RUE-1118) declares
`stamp.txt` as its output (`corpus.bzl:93`). A corpus action that compiles
`examples/mosaic/main.rue` produces no compiled mosaic, so the 17 cases naming
that root each compile it again and nothing outside the corpus can reuse any of
them. The rule is explicit about being a stamp; RUE-1164 is right that a stamp
cannot be the destination.

**The largest compiles were never in the mechanism.** The six
`large-example-{caldera,meridian}-{canary,slow,stress}` targets are plain
`rue_sh_test`s (`BUCK:397-447`). buck2 re-executes every test invocation — test
executions are not actions and never reach the action cache — so they re-run in
full on every invocation that selects them, cold and warm alike. Between them
they compile only two roots per program (`main.rue` for both the slow and stress
tiers, `canary.rue` for the canary), so the six targets represent **four distinct
compile roots**; `stress.rue` is `@import`ed by `main.rue`
(`examples/caldera/main.rue:12`) and is never a root.

**Runtime scenarios are welded to compiles, and the bill was paid in coverage.**
`cases/examples_meridian.toml` declares six cases against
`source_path = "examples/meridian/main.rue"` with no compile-flag override. The
harness gives each case a fresh temp directory and its own compiler invocation,
and `rue-cli-tests` performs no compile reuse anywhere — so the section costs six
full 80-second compiles. It is not slow in CI, because **CI does not run it**:
RUE-1083 disabled the section and the automatic example after per-case budget
kills at 120.025s/120s and 300.022s/300s, via the `--skip` list at `BUCK:256-261`
(`cases/examples_meridian.toml:6-12`). The same `--skip` list excludes caldera.

That third defect is the shape of the whole problem, and its honest form is
stronger than a wasted-seconds argument: **the welded topology did not cost time,
it cost coverage.** Six scenarios of a 36k-line application are exercised nowhere
in CI, because the only way to run the sixth was to compile the program a sixth
time. A compile that is a cached artifact makes that coverage affordable again.

### What the compiler already provides, and what it demands in return

`--source-manifest` (ADR-0047 Phase 3) restricts import resolution to a declared
set and fails closed. Verified on this tree with a two-file program whose manifest
omitted the imported module:

```text
error: [E1400]: invalid compiler input: import candidate
       '/tmp/hermetic-test/helper.rue' is not listed in the source manifest read policy
 --> main.rue:1:16
```

Enforcement sits at the single read choke point (`crates/rue/src/source_loader.rs:406-431`),
before any filesystem probe, with no warn-only mode.

**The demand it makes in return is the hard part of this design, and an earlier
draft of this ADR got it wrong.** Import resolution probes candidate arms in
order — importer-relative before root-relative, file-module before directory
facade (ADR-0051). A probe of an *undeclared* path returns `DeniedLexical`, which
`is_failure()` treats as a failure (`import_discovery.rs:245-309`) — **even when a
later, declared arm would have resolved**. A manifest listing only the files that
exist therefore rejects programs that compile fine without a manifest. Reproduced
directly:

```text
$ cat sub/types.rue
const shared = @import("shared.rue");     # resolves root-relative to ./shared.rue

$ rue root.rue -o prog                                   # no manifest
Compiled root.rue -> prog                                # exit 0

$ printf 'root.rue\nshared.rue\nsub/types.rue\n' > glob.manifest
$ rue root.rue --source-manifest glob.manifest -o prog   # manifest from existing files
error: [E1400]: import candidate '/tmp/absent-arm/sub/shared.rue'
       is not listed in the source manifest read policy   # exit 1
```

The tree already knows this. `cases/modules.toml:105` declares
`utils/_utils.rue # declared absent ambiguity arm` — a file that does not exist —
and `scripts/test-reproducible-output.sh:40-44` hand-appends two nonexistent
importer-relative arms with a comment citing the ADR-0051 read policy. The
standard library is subject to the same rule: `@import("std")` goes through the
policy, so an unlisted std fails identically.

`--emit deps` (ADR-0047 Phase 4) is what closes this. Its envelope carries
`accepted_reads` (the observed read set, with both `requested_path` and
`canonical_path`) and `observations` (every probe, including the ones that came
back `absent`, with the path that was requested).

## Decision

### Two rules and one provider

`rue_program` owns exactly one compilation and produces the executable.
`rue_program_test` consumes it and runs one runtime scenario.

```python
RueProgramInfo = provider(fields = [
    "executable",      # the compiled artifact
    "manifest",        # scan-derived source manifest
    "deps_envelope",   # the --emit deps output the manifest came from
    "root",
    "rue_target",      # x86-64-linux | aarch64-linux | aarch64-macos
    "opt_level",
    "runs_natively",   # False for a cross-target program
])

rue_program(
    name       = "meridian",
    root       = "examples/meridian/main.rue",
    srcs       = glob(["examples/meridian/**/*.rue"]),   # the declared read bound
    std        = "//:std",
    rue_target = "x86-64-linux",
)

rue_program_test(
    name            = "meridian-rejects-unknown-query-table",
    program         = ":meridian",       # consumes RueProgramInfo — no recompile
    program_args    = ["run", "unknown.sql"],
    files           = [{"path": "unknown.sql", "source": "SELECT * FROM nope;\n"}],
    exit_code       = 1,
    stderr_contains = ["unknown table"],
    tier            = "slow",
)
```

Two properties of `rue_program_test` follow from what the cases actually do and
must not be designed away. Runtime fixtures are frequently *inline and
deliberately malformed* rather than checked-in files, so the rule needs a `files`
mechanism of its own rather than only a `data` attribute pointing at repository
paths. And several programs write output through relative paths, so the scenario
needs a **writable working directory**, which a read-only runfiles tree is not.

For `test_tiers.bxl` discovery to keep working the rule must satisfy two
conditions explicitly, not incidentally: its name must keep the `_test` suffix so
it matches the `^(.*_test|test_suite)$` kind regex, and it must expose a `labels`
attribute that carries exactly one tier (`test_tiers.bxl:11-22`).

### Generating the manifest: a scan action, not a glob

`rue_program` runs two actions.

1. **Scan.** `rue --emit deps <root>` with no manifest, declaring `srcs` + `std`
   as its Buck inputs. Because reads are unrestricted, resolution reports every
   arm it probes — present and absent alike.
2. **Compile.** `rue <root> --source-manifest <generated> -o <out>`, where the
   manifest is `{requested_path of every accepted read} ∪ {requested_path of every
   absent observation}`.

Because the manifest content depends on an action output, the rule uses
`dynamic_output`. Verified end to end on the reproducer above, and on a program
importing the real standard library — the scan-derived manifest picks up all 30
std modules without the rule naming them:

```text
$ rue --emit deps main.rue | derive-manifest > m.manifest    # 31 entries
$ rue main.rue --source-manifest m.manifest -o prog
Compiled main.rue -> prog                                    # exit 0
```

Deriving the manifest from the compiler's own report rather than from the file
system also disposes of a spelling hazard: the manifest's first membership check
is lexical and never resolves symlinks, so a hand-built manifest over a Buck
symlink-tree materialization can contain spellings the compiler never asks for.
Using `requested_path` — literally the string the compiler will ask for — makes
the two agree by construction. Manifest entries are written absolute so that the
manifest-relative resolution rule (entries resolve against the manifest file's
directory, which for a generated manifest is buck-out) cannot misfire.

**This is where the design's hermeticity actually lives, and it needs stating
precisely.** The scan reads without a manifest, so on a local build it could in
principle read a file outside `srcs` and quietly launder it into the manifest.
Two things prevent that, and both are load-bearing rather than optional:

- Under remote execution only declared inputs are materialized, so an undeclared
  read fails outright.
- The declaration audit below compares the scan's `accepted_reads` against `srcs`
  and **fails when the compiler read anything the rule did not declare**. That
  makes the audit part of the mechanism, not a nicety attached to it.

The compile action then enforces the manifest in-band on every invocation, local
and remote, cold and warm. `corpus.bzl`'s header warns that under a cached action
an undeclared input becomes a false pass rather than an untracked re-run; the
combination above removes that hazard by construction rather than by scheduling.
This is deliberately stronger than ADR-0069's proposal to treat remote execution
as the sole undeclared-input detector, which catches only what RE actually runs.

No compiler change is involved: both flags exist and behave correctly today.

### Which compiles become actions

There are 4,132 corpus cases. Reading "every compilation is an action" naively
produces 4,132 actions, most compiling a ten-line program in milliseconds behind
action overhead, cache-key computation over the whole standard library, a scan
action each, and — under RE — a network round trip. That would be slower than
today. It is the failure mode this class of migration usually dies of, and
avoiding it is the load-bearing judgement in this ADR.

**Rule: a compile becomes its own action when it is expensive relative to action
overhead, or when more than one scenario consumes its output.**

| Work | Scale | Disposition | Why |
| --- | --- | --- | --- |
| caldera, meridian — `main` and `canary` roots | 4 roots | `rue_program` | `main` is 81–243s; all four are consumed by several scenarios and by more than one suite |
| CLI cases naming a checked-in root | 73 cases → 17 programs | `rue_program` | 17 distinct (root, flags, target) tuples; the fixture roots in `abi_conformance.toml` and `linker.toml` compile at three `--target`s each and stay three programs |
| Auto-discovered `examples/**` roots not already covered | ~30 | `rue_program` | Reached again by the automatic-examples pass and by frontend-diff |
| Inline-source CLI / spec / UI cases | ~4,050 | harness | Sources live inside TOML; compiles are milliseconds |
| Reproducibility fixture | 4 roots | harness | See below — modelling these as actions is self-defeating |

**The reproducibility suite must stay a harness, and the reason is instructive.**
Its perturbations are compile-*time* (relocated source roots, mtimes, umask, `-j`,
manifest order) and its assertion is that two compiles of the same declared inputs
produce identical bytes. Those two compiles have the same action key by
construction, so Buck would dedupe or cache-serve exactly the duplication the
suite exists to perform. This is the same reason compiler reproducibility is
cache-free by design (RUE-617/RUE-1019). A `rue_program` could supply one side of
the comparison, but the suite must own the second compile itself.

### What contributes to the action key

| Required in the key | Mechanism | Notes |
| --- | --- | --- |
| Root source | action input | `attrs.source()` |
| Transitive imports | action inputs | declared `srcs`; audited below |
| Source manifest | generated input | scan-derived; changes when any probed path changes |
| Compiler build | action input | `$(exe_target //crates/rue:rue)`; release vs debug already distinct via `//platforms:*` |
| Compiler flags | command line | `-O`, `--preview`, `--target` |
| Linked archives | **action inputs** | `--link-archive` bytes are read at link time, so the flag must carry a `$(location ...)` input, not a bare path string |
| Linker | pinned | `rue_program` pins `--linker internal`; see below |
| Runtime inputs | test-target inputs | `files`, `data`, `stdin`, `program_env` on `rue_program_test` |
| Expected outputs | test-target attrs | expectations key the test, not the compile — which is the point of the split |

`--linker clang|gcc` executes an undeclared `$PATH` binary and writes through
`TMPDIR`, neither of which is a declared input. The default internal linker is
genuinely hermetic — in-process, with the runtime embedded via `include_bytes!` —
so `rue_program` pins it and external linking stays out of scope. Cases that
exist to test external linkers remain harness cases.

### `--emit deps` as a declaration auditor

The manifest makes under-declaration impossible at compile time; the audit makes
it impossible at scan time and catches over-declaration besides. A
`rue_program`'s `srcs` glob that is wider than its real read closure takes cache
misses on files it never reads, which turns a precise action back into a corpus
stamp.

The audit compares the scan envelope's `accepted_reads` against `srcs` in both
directions. Three constraints on how, all of which matter:

- Compare **path and fingerprint sets, never envelope bytes**: the envelope
  embeds device/inode identity and mtimes (`dependency_envelope.rs:320-331`) and
  is not machine-stable.
- Compare against `srcs`, **not** the generated manifest — the manifest
  legitimately contains absent-arm entries that are never read.
- `accepted_reads` is the observed read set *of that run*; trusted-std
  acquisition for fallible intrinsics happens only on semantic runs, so a program
  that reaches std without `@import("std")` can read std files at compile time
  that a scan does not list. The audit is `srcs`-scoped, so this is harmless, but
  the envelope should not be described as a complete read set.

Because the scan is already an action of `rue_program`, the audit is an assertion
over an artifact the rule produces anyway rather than a separate compile.

### Negative controls

RUE-1164 asks for clean-root and undeclared-input controls. An earlier draft
claimed all three could be Buck targets; only the first can be, and the honest
shape matters because the repository has strong precedent for the other two
living outside the graph.

1. **Under-declared program must fail** — a Buck target. A fixture `rue_program`
   whose `srcs` omits one imported module, wrapped so that the expected build
   failure is *contained*: an uncontained failing target breaks
   `buck2 build //...`. This is the direct test of E1400 enforcement and it runs
   everywhere.
2. **Clean-root / remote materialization** — a CI job, not a target. The RUE-320
   merge-group canary is a workflow job that builds only `//crates/rue:rue`
   (`ci.yml`), so covering `rue_program` targets is a workflow change rather than
   a reuse of an existing target.
3. **Digest sensitivity** — a CI script in the `scripts/check-reproducible-compiler.sh`
   mould. Mutating a source and asserting the action re-executes requires driving
   buck2 and reading its event log; every Buck-visible test in this repository
   that touches buck2 stubs it, and the closest precedents deliberately live
   outside the graph.

Remote test-result caching stays off until all three pass on a real branch.
RUE-1164 makes that ordering an acceptance criterion and this ADR keeps it
strictly.

## Implementation Phases

- [ ] **Phase 0: Rules, scan-derived manifest, audit, controls** — `rue_rules.bzl`
      with `rue_program`, `rue_program_test`, `RueProgramInfo`, the two-action
      scan/compile shape, the declaration audit, and the three controls. Also the
      stale-`--prefer-remote` documentation fix (see open questions). No existing
      target changes.
- [ ] **Phase 1: Large examples** — convert the six `large-example-*` `sh_test`s
      over their four roots; each scenario in `scripts/run-large-example.sh`
      becomes its own `rue_program_test`, retiring the script. **The justification
      is not critical-path seconds** — the required path pays only 2.3s + 3.1s of
      canary compiles. It is: the canary compiles stop re-executing on every
      invocation and become shareable between `pull_request` and `merge_group`;
      the scheduled release lane stops paying 243.0s and 80.7s twice when both the
      slow and stress tiers run; and these are the clearest targets on which to
      establish the mechanism.
- [ ] **Phase 2: CLI cases naming a checked-in root** — 73 cases into 17
      programs, meridian's six first, which is where the disabled coverage is
      restored. Depends on open question 3.
- [ ] **Phase 3: Corpus input precision** — see open question 2; this phase is
      *not* specified here, because the obvious version of it does not work.
- [ ] **Phase 4: Test-result caching decision** — only after the negative controls
      pass on a real branch. See open question 4.

Linear issues are filed per phase under RUE-1164 once this ADR is accepted.

## Consequences

### Positive

- Large programs compile once per compiler, flag set and architecture, and the
  result is a real artifact many scenarios consume.
- Coverage disabled by RUE-1083 becomes affordable again, which is a correctness
  gain rather than a speed one.
- Undeclared inputs become build failures everywhere rather than only where RE
  runs, and the declaration audit closes the scan's laundering hole.
- Scenarios become individually selectable and schedulable targets, which is the
  "split" remedy in RUE-1267's alarm.
- ADR-0047 Phases 3 and 4 acquire their first consumer, and the design needs both
  of them, not just one.

### Negative

- `rue_program` is two actions and a `dynamic_output`, not one action. The scan is
  36x cheaper than the compile, but it is not free, and it runs on every cache
  miss.
- The declaration audit is load-bearing rather than advisory, which means a target
  that must stay green. Each audit is invalidated by every compiler change — the
  ~74.5% class — so audit tier assignment is a real scheduling question, not a
  formality.
- Migrating the 73 cases splits the CLI authoring surface between two mechanisms
  during Phases 2 and 3.
- Migrated targets **escape** RUE-924's corpus-omission audit rather than break
  it: that audit discovers `rue_heavy_suite` labels, so a `rue_program_test` that
  does not join the discovery set is silently unaudited. Phase 2 must extend the
  discovery set in the same change.

### Neutral

- The compiler is a universal dependency, so a compiler change still invalidates
  every `rue_program` and every audit. This ADR does not change that regime;
  ADR-0069 measures it as roughly three quarters of changes.
- `runs_natively = False` programs (cross-target compiles) have no execution
  story here. They are build-only targets with a structural-validation scenario
  at most, and they inherit whatever platform scope ADR-0069 Phase 4 lands.

## Open Questions

1. **Target architecture: attribute or configuration?** `//platforms:release`
   exists because, as `platforms/BUCK:23-33` records, a bare
   `--modifier //constraints:release` left debug and release resolving to the same
   configured output path while the toolchain's `rustc_flags` select never saw the
   constraint. The same argument could be made for a `//constraints:rue-target`
   setting plus platforms. Recommendation: a plain attribute. The Rue target is a
   property of the artifact requested, not of the machine building it — the
   `abi_conformance` and `linker` fixtures want all three targets side by side in
   one package, which attributes make trivial and configurations make awkward —
   and `--target` is on the command line, so it keys the action either way. (Cite
   the `platforms/BUCK` comment rather than RUE-277 itself: the issue records the
   symptom, the comment records the mechanism.)
2. **Phase 3 needs re-deriving; the obvious version does not work.** An earlier
   draft called per-shard case-file declaration "clearly right". It is not
   implementable as stated: shard assignment is per-*case* LPT cost-balancing over
   `shard-weights.json` (`crates/rue-cli-tests/src/sharding.rs:76-105`), so one
   TOML file's cases scatter across all four shards and the mapping churns on
   every weights refresh (RUE-1222). Declaring per-shard file sets would require
   either file-aligned packing — which ADR-0069 §6's skew analysis argues against
   — or a generated shard→files mapping, which by ADR-0069's own ledger standard
   needs a gate to count. Note also that the blast radius is wider than stated:
   the `cases` filegroup keys **seven** corpus actions (four shards plus
   `//:cli-tests`, `//:cli-tests-slow`, `//:release-smoke`), not four. Options:
   make shard assignment a build-time fact with a gate; accept coarse keys for the
   harness bucket; or drop the phase. I do not have a recommendation I trust here.
3. **Do the CLI cases migrate out, or does the harness consume prebuilt
   artifacts?** Either `rue-cli-tests` learns to accept a prebuilt executable for a
   case naming a checked-in root, or those 73 cases become `rue_program_test`
   targets. The first preserves one authoring surface and keeps RUE-924's audit
   working unchanged; the second gives the per-scenario targets the milestone is
   for, at the cost of extending the discovery set and reimplementing inline
   runtime fixtures and a writable cwd. Recommendation: migrate them out — they
   are the least TOML-shaped cases in the corpus. This has the largest blast
   radius on authoring ergonomics of anything here.
4. **Test-result caching: what actually blocks it.** ADR-0069 §4 suggests buck2's
   `supports_test_execution_caching` is available and blocked only by Rue's noop
   toolchains. Two corrections. For the *existing* corpora it is not available at
   all: reading the bundled prelude for the pinned 2026-07-15 buck2, the attribute
   appears only on the java/kotlin/cxx/python/android test decls, and
   `sh_test_impl` constructs `ExternalRunnerTestInfo` without it — so no `sh_test`
   or `rue_sh_test` here can opt in even with a live toolchain. But for
   `rue_program_test` the objection is moot: it is a new rule emitting its own
   `ExternalRunnerTestInfo`, so setting the field is one line. The real costs are
   therefore the non-noop remote test toolchain and the trust decision RUE-1164
   already gates behind the negative controls. Recommendation: still don't, but on
   those grounds — once `rue_program` owns the expensive half, what remains in a
   test execution is running a binary and comparing output, and caching that is a
   toolchain change for a small remainder. Revisit if Phases 1–2 leave test
   execution on the critical path.

Note that RUE-1164 carries no comments, so none of these is pre-decided
elsewhere.

## Non-Goals

- **Specification traceability stays independent of action caching.**
  `//:spec-traceability` runs `rue-spec --traceability` over the case corpus and
  `docs/spec/src`. It is not a compilation, must not become a scenario over one,
  and keeps its own plain test target. RUE-1164 makes this an acceptance criterion
  and this ADR reads it as load-bearing.
- **No compiler changes.** `--source-manifest` and `--emit deps` both exist and
  behave correctly, and the absent-arm problem is solved by using the second
  rather than by relaxing the first. Tolerating undeclared *absent* probes would
  be a compiler change and would weaken the read policy ADR-0051 defines; this
  design does not ask for it.
- **External linkers.** `rue_program` pins `--linker internal`. Cases covering
  `clang`/`gcc` linking stay in the harness.
- **No change to what is tested.** Every scenario that runs today runs after, and
  Phase 2 restores scenarios that currently run nowhere. The RUE-924 audit and the
  tier-labelling contracts must survive each phase — see Consequences for how
  Phase 2 threatens the first.
- **Not a package system.** ADR-0047 leaves package resolution outside the
  compiler; `rue_program` is a build rule over resolved inputs, not a step toward
  one.

## References

- ADR-0047 — root-module compilation units and build-system inputs
- ADR-0051 — canonical import resolution authority (the arm-probing order that
  makes a glob-derived manifest invalid)
- ADR-0069 §4 and Phase 5 — CI work scheduling for a compiler monorepo
- `docs/process/build-cache.md` — remote cache and execution contracts
- `corpus.bzl` — RUE-1118's cacheable corpus actions and their input contract
- RUE-1164, RUE-1118, RUE-1222, RUE-1267; RUE-320 (remote execution, Done
  2026-07-18); RUE-1083 (the budget kills that disabled the meridian section)
