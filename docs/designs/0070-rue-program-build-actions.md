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
decisions rather than settled design; each open question now carries a
recommendation and its evidence. Question 2's proposal is large enough to deserve
its own ADR and issue, and — after review — it no longer gates any phase here:
question 3's recommendation stands on the existing corpus actions alone.

## Summary

The compilation of a Rue program should be a first-class Buck artifact keyed on
its real inputs — root source, declared read closure, generated source manifest,
compiler build, flags, target architecture, and any linked archives — rather than
a side effect hidden inside a test process or an aggregate success stamp.

This ADR proposes two rules and one provider: `rue_program`, which owns exactly
one compilation and produces the executable; and `rue_program_test`, which
consumes that executable and runs one runtime scenario. Many scenarios share one
compile. The source manifest is derived from a cheap `--emit deps` scan rather
than from the `srcs` glob, because a glob provably cannot produce a valid
manifest; the derivation step is also where the declared boundary is enforced, by
failing the build when the compiler read anything outside `srcs`, and where the
envelope's absolute paths are re-anchored so that a shared scan-cache hit is not a
build failure on another machine. A granularity
rule keeps the action count proportionate: a compile becomes its own action when
it is expensive relative to action overhead, or when more than one scenario
consumes its output. That yields 12 programs; the 34 auto-discovered example
smokes, the one-scenario wordfreq root, and the ~4,050 inline-source corpus
cases stay inside their harnesses.

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
| CLI cases naming a checked-in root | 73 cases, 13 roots (10 example + 3 fixture), 17 distinct (root, flags, target) tuples |
| Most-recompiled roots | mosaic 17x, rill 12x, lattice 7x, calculator 7x, meridian 6x, jsonfmt 5x |
| Distinct `rue_program` targets proposed | 12 (4 large-example + 8 CLI) |

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
    "root",
    "rue_target",      # the RESOLVED target: x86-64-linux | aarch64-linux | aarch64-macos
    "opt_level",
    "runs_natively",   # resolved target == configured platform's target
])

# Mechanism internals, deliberately NOT in the consumer-facing provider.
_RueProgramInternalInfo = provider(fields = ["manifest", "deps_envelope"])

rue_program(
    name       = "meridian",
    root       = "examples/meridian/main.rue",
    srcs       = glob(["examples/meridian/**/*.rue"]),   # the declared read bound
    # No rue_target: the native target is resolved from the configured platform
    # via the toolchain. Setting rue_target explicitly means intentional
    # cross-compilation. Compiler, std, default flags and the platform's native
    # target arrive as one resolved unit:
    # _toolchain = attrs.toolchain_dep(default = "toolchains//:rue")
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

**Target resolution is hybrid: platform-derived by default, attribute on
purpose.** The same target labels must build and run natively on all three CI
platforms — Linux x86-64, Linux AArch64, macOS AArch64 — exactly as today's
`sh_test`s do by never passing `--target` at all. A hardcoded
`rue_target = "x86-64-linux"` (which an earlier draft's example showed) cannot
run on two of the three, and per-architecture target labels would multiply the
program count by platform. So: `RueToolchainInfo` carries the configured
platform's native Rue target, resolved the same way `toolchains//:rust` already
resolves per platform; a `rue_program` that sets no `rue_target` compiles for
that native target; setting `rue_target` explicitly is intentional
cross-compilation. `runs_natively` is computed — resolved target equals the
configured platform's target — never asserted. The acceptance criterion that the
target architecture keys the action is met either way, because the resolved
target appears on the compile command line.

For `test_tiers.bxl` discovery to keep working the rule must satisfy two
conditions explicitly, not incidentally: its name must keep the `_test` suffix so
it matches the `^(.*_test|test_suite)$` kind regex, and it must expose a `labels`
attribute that carries exactly one tier (`test_tiers.bxl:11-22`).

### Generating the manifest: a scan action, not a glob

`rue_program` runs three ordinary actions. **Not a `dynamic_output`** — an
earlier draft reached for one, and that was a category error worth naming, since
it is the kind of mistake that hardens into a structure nothing else can be built
on.

1. **Scan.** `rue --emit deps <root>` with no manifest, declaring `srcs` + `std`
   as its Buck inputs. Because reads are unrestricted, resolution reports every
   arm it probes — present and absent alike. The scan's command line carries no
   `-O` and no `--target`, so **one scan serves every flag set of a root**: the
   read closure is target-invariant by construction (see open question 1), which
   is what makes this action worth separating rather than fusing into the compile.
2. **Derive.** A script over the envelope plus the static `srcs` list, emitting
   the manifest — `{requested_path of every accepted read} ∪ {requested_path of
   every absent observation} ∪ the declared std` — and failing when any accepted
   read falls outside `srcs ∪ std`.
3. **Compile.** `rue <root> --source-manifest <generated> -o <out>`, declaring
   `srcs` + `std` + compiler + the generated manifest as inputs.

`dynamic_output` exists for when the *shape of the action graph* — which actions
run, over which inputs — depends on an artifact's content. Nothing here does.
Content that depends on an upstream action's output is an ordinary action edge:
every command line and every input set above is known at analysis time, and the
manifest is consumed by path rather than inspected to decide anything. The one
design that would genuinely need `dynamic_output` is pruning the compile's
declared inputs down to the observed read closure instead of declaring all of
`srcs` — and that is precisely the design this ADR did not choose (the key table
below reads "Transitive imports | declared `srcs`", and over-declaration is
handled by the `ValidationInfo` audit). Steps 1 and 2 may be fused into a single
wrapper action if the extra edge is judged not to earn its keep; that is a
packaging choice and changes nothing above.

Verified end to end on the reproducer above, and on a program importing the real
standard library — the scan-derived manifest picks up all 30 std modules without
the rule naming them:

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
the two agree by construction.

**Manifest entries must be machine-stable, and writing them verbatim would not
be.** An earlier draft wrote entries absolute, on the theory that this kept the
manifest-relative resolution rule from misfiring. That had it backwards: the
manifest-relative rule is the fix, and absolute entries are a live correctness
hole under this design's own remote-cache posture. The composition that breaks:

- The envelope is machine-*un*stable. `requested_path` and `canonical_path` are
  both `normalize_absolute` (`import_discovery.rs:334`), and the envelope also
  embeds device/inode identity and mtimes — the same fact the audit section
  already relies on.
- The scan's action key is machine-*stable*: content digests of `srcs` + std +
  compiler over a project-relative command line. That is exactly what makes it
  uploadable and shareable, which `allow_cache_upload = True` asks for.
- So environment A uploads a scan result; environment B, at a different checkout
  root, takes the cache hit and receives *A's* absolute paths. Derivation then
  emits a manifest naming paths that do not exist in B. Any compile that misses
  while the scan hits — a different `-O`, a different `--target`, or simply a
  combination never built in A — runs against a foreign manifest and fails: either
  at the derive step's boundary check (A's canonical paths against B's `srcs`) or
  at the compiler's lexical membership check (`source_loader.rs:406-431`), which
  compares before any filesystem probe and so never matches B's requests. **A hard
  failure on a correct build, triggered by a cache hit.**

Even with upload disabled it costs the design its headline property: an absolute
entry makes the *manifest's* digest, and therefore the compile's action key, vary
with checkout root — so "compile once, consume many" would hold only among
machines whose filesystem layouts agree, and Phase 1's sharing between the
`pull_request` and `merge_group` lanes would narrow to lanes whose runners share a
path.

**Derivation therefore re-anchors before writing.** The envelope records the
project root it was produced under (`dependency_envelope.rs:36`, populated at
`:166`), so the script strips that prefix and writes each entry relative to the
manifest's own directory. This needs no compiler change: `SourceManifest::load`
already resolves a relative entry against the manifest file's parent
(`source_loader.rs:51-83`) and normalizes it into the same absolute form the
lexical check compares against, so relative entries and absolute requests agree
in whatever environment the compile runs. A generated manifest lives under
buck-out, which lives under the project root, so the relative offset is itself
stable. This is the discipline the audit section already states — compare paths
and fingerprints, never envelope bytes — applied one level earlier, to the
pipeline rather than to the assertion over it.

**The declared standard library is unioned in unconditionally, and it must be.**
A scan reports only what resolution probed, and `--emit deps` does no semantic
work — so trusted-std acquisition for fallible intrinsics, which happens only on
semantic runs, is invisible to it. A four-line program using one fallible
intrinsic and importing nothing shows the consequence:

```text
$ cat main.rue
fn main() -> i32 { let p = @parse_i32("41"); 0 }

$ rue main.rue -o prog                                    # no manifest → exit 0

$ rue --emit deps main.rue | derive-manifest              # reports main.rue alone
$ rue main.rue --source-manifest derived.manifest -o prog
Hermetic build configuration error: the trusted standard-library module
'\0rue-std/option.rue' at '.../std/option.rue' is not permitted by the hermetic
build configuration: the source manifest does not declare this path.   # exit 1

$ rue main.rue --source-manifest with-std.manifest -o prog             # exit 0
```

Trusted-module acquisition re-checks the manifest before any probe
(`source_loader.rs:1574-1667`), so a scan-derived manifest without std rejects a
program that compiles fine without a manifest. Unioning the declared std in is
not over-declaration of the action key: manifest membership means "available to
import", not "read" (ADR-0047), and the std filegroup keys the action either way.

### Where hermeticity actually lives, stated precisely

A glob-derived manifest would have encoded the *declared* boundary, so an
out-of-`srcs` read failed E1400 in-band. A scan-derived manifest encodes what the
scan *observed* — so on a local build an out-of-`srcs` read would otherwise pass
scan, land in the manifest, and compile cleanly. Worse, it would be laundered into
the cache: the scan's key (`srcs` + std + compiler) never mentions the stray file,
so a later change to it leaves a stale manifest and a stale binary to be
cache-served. That is exactly `corpus.bzl`'s false-pass hazard, reproduced one
level down.

**The derivation step therefore enforces the boundary, not a downstream audit.**
It already reads the envelope; it fails when any accepted read's canonical path
lies outside `srcs ∪ std`. Under-declaration is then an in-band build failure on
every build that re-runs the scan, local and remote — the property a glob-derived
manifest would have had, recovered on a mechanism that actually works, at the cost
of a set comparison in a script the rule runs anyway. Remote execution remains a
second, independent check, because it materializes only declared inputs.

What is left for the standalone audit is over-declaration only, which is a
cache-precision concern rather than a correctness one. That distinction matters:
correctness is enforced by construction, and only precision is advisory.

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
| CLI cases naming a checked-in root | 64 cases → 9 roots, **8 new programs** | `rue_program` | 3–17 TOML scenarios each consume one artifact; `examples/meridian/main.rue` is the ninth root and is already declared by the row above |
| The one-scenario wordfreq root | 1 case | harness | See below — one cheap scenario; shard/monolith is a scheduling alternative, not a second consumer |
| Auto-discovered `examples/**` roots not already covered | 34 | harness | See below — every one fails the rule on both prongs |
| Inline-source CLI / spec / UI cases | ~4,050 | harness | Sources live inside TOML; compiles are milliseconds |
| Cross-target fixture cases (`abi_conformance.toml`, `linker.toml`) | 6 cases | harness | See below — they fail the rule on both prongs |
| The repo-relative `source_path` fixture case | 1 case | harness | See below — its subject *is* the TOML mechanism |
| The one `differential_opt` calculator case | 1 case | harness | Four compiles by design; a compile-*time* differential, same family as reproducibility |
| Reproducibility fixture | — | harness | See below — modelling these as actions is self-defeating |

**The rule is applied to itself, including where that costs a row.** Successive
drafts of this table said 17 programs, then 11, then 10. All three were wrong in
the same way — the rule was stated and then quietly not applied — so every count
above has now been re-derived from the tree rather than adjusted.

The three `cli-test-fixtures/` roots all fail the rule on both prongs.
`abi_conformance_smoke.rue` is 75 lines and `cross_runtime_smoke.rue` is 22; both
compile in milliseconds, and each (root, target) tuple is consumed by exactly one
case. `repo_relative_source_path.rue` is one file with one case — and it could not
migrate even if it were expensive, because that case exists to test the harness's
own repo-root-relative `source_path` resolution (`source_path.toml:6-10`,
RUE-495). Rewritten as a `rue_program_test` it would no longer test anything: its
subject *is* the TOML mechanism it would be leaving.

**wordfreq fell to the same rule on this round's re-check.** An earlier draft
kept `examples/wordfreq/main.rue` (one case) as a program on the grounds that
each premerge case appears in both its CI shard and the unsharded `//:cli-tests`.
Those two targets are scheduling *alternatives* — CI's platform-corpus matrix
runs the shards, a local `./test.sh` runs the monolith and excludes
`rue_cli_shard` (`test.sh:108-139`) — so they are one scenario scheduled two
ways, not two consumers, and counting them as two would silently rewrite the
rule from "more than one scenario" to "more than one consuming action
definition". One cheap scenario, one consumer: wordfreq compiles inside the
corpus action that runs its case, like any other single-scenario root. The
remaining eight CLI roots all carry 3–17 TOML scenarios against one artifact.

So 9 of the 73 cases stay compile-in-harness (6 cross-target, 1 repo-relative, 1
differential, 1 wordfreq), 64 migrate, and they name 9 roots. Only **8** are new
programs: `examples/meridian/main.rue` is also a large-example root, so one
artifact serves both the six CLI scenarios and the slow-tier scenarios. That
overlap is the design working rather than an accounting nuisance — it is exactly
the "many scenarios, one compile" property, reaching across two suites.

**The 34 auto-discovered roots fail the rule the same way, and an earlier draft
counted them as programs anyway.** `collect_example_files` yields 45 roots; 10
are CLI-named and one is caldera's, leaving 34 whose only compilation scenario is
the RUE-48 automatic smoke — one compile, one run, per corpus execution. The
claimed second consumer was the frontend differential, and that claim was wrong:
`rue-frontend-diff` compiles exactly one root, `examples/ruelex/main.rue`, and
merely lexes and parses the rest of the corpus in-process as comparison input
(`crates/rue-frontend-diff/src/main.rs:1281-1303`). Sized against the expensive
prong they are 3–246 lines each, transitively — milliseconds to compile. Single
consumer, cheap: they stay where they are, inside the automatic pass, which also
means no phase has to rewire `run_example` to consume artifacts it has no reason
to consume. A root that later grows expensive or acquires a second scenario
graduates by being named — the same path the 10 CLI-named roots took.

A rule that its own table quietly exempts things from is not a rule; a reader
applying it strictly must get the same table.

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
| Cache upload | `allow_cache_upload = True` | on **both** actions; the compile-once-consume-many property depends on the scan and the compile being uploadable, not merely cacheable locally |
| Transitive imports | action inputs | declared `srcs`; audited below |
| Source manifest | generated input | scan-derived; changes when any probed path changes |
| Compiler build | toolchain input | `RueToolchainInfo.compiler`, internally defaulting to `$(exe_target //crates/rue:rue)`; release vs debug already distinct via `//platforms:*` |
| Standard library | toolchain input | `RueToolchainInfo.std`, internally defaulting to `//:std` |
| Compiler flags | command line | `-O`, `--preview` |
| Target architecture | command line | `--target`, always passed explicitly with the *resolved* target — platform-derived default or deliberate `rue_target` override — so the key never depends on host detection |
| Linked archives | **action inputs** | `--link-archive` bytes are read at link time, so the flag must carry a `$(location ...)` input, not a bare path string |
| Linker | pinned | `rue_program` pins `--linker internal`; see below |
| Runtime inputs | test-target inputs | `files`, `data`, `stdin`, `program_env` on `rue_program_test` |
| Expected outputs | test-target attrs | expectations key the test, not the compile — which is the point of the split |

`--linker clang|gcc` executes an undeclared `$PATH` binary and writes through
`TMPDIR`, neither of which is a declared input. The default internal linker is
genuinely hermetic — in-process, with the runtime embedded via `include_bytes!` —
so `rue_program` pins it and external linking stays out of scope. Cases that
exist to test external linkers remain harness cases.

### The over-declaration audit

Correctness is handled above, in the derivation step. What remains is precision:
a `rue_program` whose `srcs` glob is wider than its real read closure takes cache
misses on files it never reads, which turns a precise action back into a corpus
stamp. Left unchecked, a broad glob undoes the whole point of the rule.

**Over-declaration must be advisory, not a required check, and the reason is
structural rather than a matter of nerve.** An earlier draft made
`srcs − accepted_reads = ∅` a required validation, and that check fails on this
ADR's own example target: `examples/meridian/main.rue` does not import
`canary.rue`, yet `glob(["examples/meridian/**/*.rue"])` contains it — and the
canary target's glob symmetrically contains all of `main`'s tree. Caldera has the
same shape, and any directory holding sibling roots reproduces it. The only ways
to satisfy a required check are all worse than the imprecision: exact per-root
`srcs` cannot be derived from the scan while remaining a static action input
without reintroducing the dynamic dependency shaping this design explicitly
rejected; a hand-maintained exact list duplicates import discovery and rots; and
per-root directory layouts would let the build graph dictate source organization.
RUE-1164 requires every real input to key the action — it does not require the
first implementation to carry a mathematically minimal key. A directory-bounded
glob delivers the property that matters (one correct, cacheable artifact shared
by scenarios) at the cost of some spurious invalidation inside the directory,
and tightening that later should be driven by measured invalidation cost, not
asserted up front.

Three constraints on how the comparison is done, all of which matter:

- Compare **path and fingerprint sets, never envelope bytes**: the envelope
  embeds device/inode identity and mtimes (`dependency_envelope.rs:320-331`) and
  is not machine-stable.
- Compare against `srcs`, **not** the generated manifest — the manifest
  legitimately contains absent-arm entries and the whole std tree, none of which
  need be read.
- `accepted_reads` is the observed read set *of that scan*, not a complete
  compile-time read set — trusted-std acquisition is invisible to it, as above.
  Treat std as always-declared rather than inferring it from the envelope.
- Because sibling roots share a glob, `srcs − accepted_reads` is legitimately
  non-empty per target. The signal worth reporting is directory-scoped: a file
  that no program whose `srcs` contain it ever reads is dead weight in every
  key it touches; a file read by one sibling and carried by another is the
  glob doing its job. **A per-target validation cannot compute that signal** —
  it sees one program's envelope and would report `canary.rue` against `main`
  as a known false positive — so the directory signal lives on an aggregate.

**The aggregate is explicit, not discovered.** Sibling `rue_program`s that share
a directory glob are declared by one macro call (`rue_program_family`, or simply
the same BUCK package block), and that call also emits one
`<dir>-srcs-report` aggregate whose deps are exactly those siblings. The
aggregate reads each sibling's `_RueProgramInternalInfo.deps_envelope`, unions
the accepted-read sets, and reports files of the shared glob that **no** sibling
reads. Ownership is the macro's argument list — a rule cannot discover its
siblings, and this design does not ask it to. A `rue_program` declared outside
any family still carries a strictly target-local report, explicitly labelled as
listing *extras* rather than classifying them as dead weight, since one
target's view cannot make that call.

**Attach the reports with `ValidationInfo`, marked `optional`, rather than as
separate test targets.** The pinned 2026-07-15 buck2 ships it and the bundled prelude
already uses it (java, kotlin, apple); its `ValidationSpec` carries an `optional`
field, and optional validations do not run by default — they are selected with
`--enable-optional-validations`. That is exactly the advisory shape: the report
travels with the target it describes, costs nothing on ordinary builds, and a CI
job or a curious maintainer can demand it by name. The derivation step's
under-declaration check stays where it is, required and in-band, because it is
correctness; this validation carries only the precision report. It still
dissolves the question an earlier draft had to ask — which tier do ~50 audit
targets carry, given every compiler change invalidates all of them — rather than
answering it. There are no audit targets to schedule.

### Negative controls

RUE-1164 asks for clean-root and undeclared-input controls. An earlier draft
claimed all three could be Buck targets; only the first can be, and the honest
shape matters because the repository has strong precedent for the other two
living outside the graph.

1. **Under-declared program must fail at the derivation boundary** — a Buck
   target, wrapped so that the expected build failure is *contained*: an
   uncontained failing target breaks `buck2 build //...`. The naive fixture — a
   `rue_program` whose `srcs` simply omits one imported module — is **not one
   stable test**, because its failure stage depends on the executor. On a
   non-sandboxed local run the omitted file is still on disk, the scan reads it,
   and derivation rejects the out-of-`srcs` read; in a clean root or under RE the
   file was never materialized, the scan records an absent probe, and the compile
   fails as an ordinary unresolved import. Both are failures, but only the first
   exercises the boundary check this control exists to pin. The fixture therefore
   *materializes* the omitted module deliberately — passed to the scan action as
   a test-only hidden input so it is present in every execution environment —
   while excluding it from the `srcs` set the derivation script compares against,
   and asserts the derivation-specific error, not just any failure.
2. **Clean-root / remote materialization** — a CI job, not a target. The RUE-320
   merge-group canary is a workflow job that builds only `//crates/rue:rue`
   (`ci.yml`), so covering `rue_program` targets is a workflow change rather than
   a reuse of an existing target. This job stays the independent proof that
   undeclared Buck inputs are simply *unavailable*, which control 1 deliberately
   no longer demonstrates.
3. **Digest sensitivity** — a CI script in the `scripts/check-reproducible-compiler.sh`
   mould. Mutating a source and asserting the action re-executes requires driving
   buck2 and reading its event log; every Buck-visible test in this repository
   that touches buck2 stubs it, and the closest precedents deliberately live
   outside the graph.

**And one positive control, because RUE-1164's acceptance criteria are not all
failure-shaped.** "Warm runs show Rue compilation actions served by BuildBuddy
rather than rerun inside a harness" is an explicit criterion, and the three
controls above cover failure modes only. Phase 1 therefore owns a success check:
two builds — across relocated checkout roots, or across the `pull_request` and
`merge_group` lanes, which is the pair RUE-1118 measured — must show the scan and
compile actions cache-served while more than one scenario consumes the same
executable. Phase 2 repeats it for a CLI root.

Remote test-result caching stays off until the three negative controls pass on a
real branch. RUE-1164 makes that ordering an acceptance criterion and this ADR
keeps it strictly.

### Forward positioning: external build systems

Rue's roadmap includes first-class integration with state-of-the-art build tools
— a published `rules_rue`, toolchain distribution, a plausible Bazel port. This
ADR is not that work and does not attempt it. But most of what such an
integration needs is the hard part of *this* design: compile-as-artifact keyed on
a declared read closure, the program/scenario split, the granularity rule, and
`--emit deps` acquiring a real consumer are all build-system-agnostic. The point
of this section is to keep it that way, by recording the seams where an internal
convenience would otherwise become an external constraint. Each is cheap now and
expensive after publication.

**Machine-stable manifest paths are the same fix twice.** The relative-entry
derivation above is required for Buck2 correctness on its own; it is also the
only portable form, because absolute paths inside a consumed action output are
unrepresentable under sandboxed execution. Relocation-invariance of the
derivation script belongs in the unit coverage the Consequences section already
demands — the repository holds exactly this bar for the compiler binary, via the
reproducibility suite's relocated-source-root perturbations
(`scripts/check-reproducible-compiler.sh`, `scripts/test-reproducible-output.sh:164`).

**A toolchain indirection, not per-invocation wiring.** The compiler and std
reach the rule through a single `attrs.toolchain_dep` carrying compiler, std and
default flags as one resolved unit, rather than each call site naming
`//crates/rue:rue` and `//:std`. External consumers bring a *released* compiler,
not a target in this repo, and both Buck2 toolchain rules and Bazel toolchain
resolution assume that shape. This is the repository's own established pattern
rather than a new one: `crates/rue-runtime/runtime.bzl:137` already takes
`_rust_toolchain = attrs.toolchain_dep(default = "toolchains//:rust")` and reads
`RustToolchainInfo`. It also buys something internal — the ability to run these
rules against a prebuilt release compiler, which is what a toolchain-bootstrap
test needs anyway.

**The provider is an API surface; keep mechanism out of it.** `RueProgramInfo`
above carries durable artifact facts only. `manifest` and `deps_envelope` live in
an internal provider, because the envelope is machine-unstable bytes in a format
this ADR treats as private — publishing it in the consumer-facing provider would
bake that format into a compatibility contract. On naming: `rue_program` /
`rue_program_test` depart from the ecosystem's `rue_binary` / `rue_test`
convention. Renames are free today and not later; the choice should be made
deliberately in Phase 0 — either adopt the conventional names, or keep the
internal names distinct on purpose so published rules can differ. (The absence of
a `rue_library` is not a gap. Whole-program compilation is the language's model,
and a declared read closure is exactly what a future library-granularity unit
would compose from.)

**The scan apparatus compensates for a manifest semantic, and that is worth
recording rather than only working around.** Mature language integrations
converge on handing the compiler an authoritative input map it consults to the
exclusion of everything else — rustc's `--extern` and dep-info, Go's importcfg,
javac's classpath, Swift's module maps. `--source-manifest` is instead an
*allowlist with fatal probes*: an undeclared probe is `DeniedLexical`-fatal even
when a later declared arm resolves, so a valid manifest must enumerate absent
arms, so a glob cannot produce one, so this design needs a scan action, a
derivation script, and the hermeticity trap the Consequences section warns is
easy to lose. That machinery is not intrinsic to Rue's build problem, and every
future integration surface would re-inherit it.

The seam is a compiler mode in which **the manifest is the filesystem**: a probe
of a non-member path resolves as `Absent` rather than fatally. Under it a
glob-derived manifest is valid by construction, `rue_program` collapses to one
action, the derivation script and the path-stability problem above cease to
exist, and under-declaration surfaces as an ordinary unresolved-import error. The
Non-Goals section dismisses this as weakening the read policy, which is half
right, and the honest form of the trade is:

- **Cost.** On a *non-sandboxed* build where the disk is a superset of the
  manifest, divergence between the two degrades from a loud E1400 into a silent
  arm-shift: a file present on disk but absent from the manifest resolves via a
  later arm, producing a different binary than the manifestless compile. That is
  a real hazard and it is the true content of the Non-Goals sentence.
- **Gain.** The residual window this ADR documents under Consequences — an
  absent-arm path outside `srcs` is a declared-*allowed* read, so a file
  materializing there is read undeclared until something re-runs derivation —
  does not exist under manifest-as-filesystem semantics. Not in the manifest,
  never readable. Negative control 3 exists to police a window these semantics
  would not have.
- **And the hazard is confined to the environments official rules do not run
  in.** Under sandboxed or remote execution the disk *is* the declared inputs, so
  the two semantics are observationally identical.

No change is proposed here — "no compiler changes" is the right constraint for
this milestone and the mechanism above is correct without one. This is recorded
so that ADR-0051's fatal-probe semantics are not mistaken for a permanent
constraint merely because this document worked around them successfully.

**Portability of everything else**, for the record: `ValidationInfo` maps to
Bazel's validation output group and is the right choice on both; `allow_cache_upload`
is implicit in Bazel's remote cache model; `rue_program_test`'s inline `files`
and writable working directory are expressible in both; open question 2's
scheduling-out-of-the-key-domain principle is build-system-independent. BXL
(`test_tiers.bxl`, the `labels`/kind-regex contract) is Buck2-only, but it is
internal CI scheduling and would not ship in a ruleset.

## Implementation Phases

- [ ] **Phase 0: Rules, scan-derived manifest, audit, controls** — `rue_rules.bzl`
      with `rue_program`, `rue_program_test`, `RueProgramInfo`, the three-action
      scan/derive/compile shape, a `RueToolchainInfo` indirection, the declaration
      audit, and the three controls. This phase also settles the rule names while
      renaming is still free (see "Forward positioning"). Also the
      stale-`--prefer-remote` documentation fix noted in the References (RUE-320
      is Done, so `AGENTS.md`'s condition no longer holds). No existing target
      changes.
- [ ] **Phase 1: Large examples** — convert the six `large-example-*` `sh_test`s
      over their four roots; each scenario in `scripts/run-large-example.sh`
      becomes its own `rue_program_test`, retiring the script. **The justification
      is not critical-path seconds** — the required path pays only 2.3s + 3.1s of
      canary compiles. It is: the canary compiles stop re-executing on every
      invocation and become shareable between `pull_request` and `merge_group`;
      the scheduled release lane stops paying 243.0s and 80.7s twice when both the
      slow and stress tiers run; and these are the clearest targets on which to
      establish the mechanism. This phase owns the positive warm-cache check: a
      relocated or cross-lane second build must show the canary scan and compile
      actions cache-served under multiple consuming scenarios.
- [ ] **Phase 2: break the weld for CLI cases naming a checked-in root** — the
      64 cases keep their TOML form and their existing corpus actions; what
      changes is that each CLI corpus action declares the 9 program artifacts as
      inputs and the harness runs the prebuilt executable a case names instead of
      compiling it. The wiring already exists: `cached_corpus_suite`'s `env` is
      `attrs.arg()`, so the artifacts arrive through `$(location ...)` exactly
      like every other declared corpus input, and an artifact edit invalidates
      the consuming corpus actions correctly. Meridian's six scenarios come
      first, which is where the disabled coverage is restored. The 6 cross-target
      fixture cases, the repo-relative fixture case, the one `differential_opt`
      case, and the one-scenario wordfreq case stay compile-in-harness
      regardless. **This phase
      requires no sharding change, no TOML migration, and no decision on open
      question 2** — Buck builds each program once, every scenario shares it,
      and warm runs serve the compile from cache, which is RUE-1164's goal met on
      the existing corpus topology.
- [ ] **Phase 3: Test-result caching decision** — only after the negative controls
      pass on a real branch. See open question 4. The default — do not enable —
      stands unless evidence overturns it.

Linear issues are filed per phase under RUE-1164 once this ADR is accepted.

## Consequences

### Positive

- Large programs compile once per compiler, flag set and architecture, and the
  result is a real artifact many scenarios consume.
- Coverage disabled by RUE-1083 becomes affordable again, which is a correctness
  gain rather than a speed one.
- Undeclared inputs become build failures on every build that re-runs the scan,
  rather than only where RE runs — but only because the derivation step enforces
  `srcs` itself. One window remains open by ordinary action-cache semantics: an
  absent-arm path outside the `srcs` tree is a declared-*allowed* read, so if a
  file later materializes there, local rebuilds read it undeclared until some
  change re-runs derivation. Negative control 3 is precisely the check that
  covers that window, which is part of why it is a control rather than a nicety.
  That
  property belonged to the glob-derived manifest of an earlier draft; on the
  scan-derived mechanism it has to be put back deliberately, and it is easy to
  lose again if derivation is ever simplified to "just write what the scan saw".
- Large-example scenarios become individually selectable and schedulable
  targets. For corpus cases the "split" remedy in RUE-1267's alarm stays an
  authoring act (split the TOML file) unless the per-file corpus ADR lands, at
  which point it becomes target-granular.
- ADR-0047 Phases 3 and 4 acquire their first consumer, and the design needs both
  of them, not just one.

### Negative

- `rue_program` is three actions, not one. The scan is 36x cheaper than the
  compile, but it is not free, and it runs on every cache miss. It is at least
  ordinary static actions rather than a `dynamic_output`, so the cost is
  scheduling overhead and not a structure that resists porting.
- Correctness now depends on a derivation script, not only on rule wiring. A bug
  there is a hermeticity bug, so it needs its own unit coverage — the negative
  controls exercise it end to end but will not pin its set arithmetic. That
  coverage must include **relocation invariance**: derivation reads absolute
  paths out of the envelope and must emit manifest entries that do not depend on
  the checkout root, or a shared scan-cache hit becomes a build failure.
- The CLI harness grows a second execution mode: run a staged prebuilt binary
  rather than compile the case's root. It is a mode of the existing harness, not
  a parallel mechanism — cases stay in TOML, the corpus targets keep their names,
  labels and tiers, and RUE-924's corpus-omission audit is untouched — but the
  harness's compile path and its staged path can now drift and need shared
  plumbing kept honest.
- **If open question 3's fallback is ever taken** — migrating scenarios to
  `rue_program_test` targets — two costs return: the authoring surface splits
  between TOML and BUCK during the transition, and migrated targets **escape**
  RUE-924's audit rather than break it (the audit discovers `rue_heavy_suite`
  labels, so a `rue_program_test` outside the discovery set is silently
  unaudited; the migrating change must extend discovery in the same change).
  Under the recommended prebuilt-artifact form, neither cost exists.

### Neutral

- The compiler is a universal dependency, so a compiler change still invalidates
  every `rue_program` and every audit. This ADR does not change that regime;
  ADR-0069 measures it as roughly three quarters of changes.
- `runs_natively = False` programs (cross-target compiles) have no execution
  story here. They are build-only targets with a structural-validation scenario
  at most, and they inherit whatever platform scope ADR-0069 Phase 4 lands.

## Open Questions

All four carry a recommendation now, and three of them changed after review. They
remain maintainer decisions: RUE-1164 is labelled `needs-decision`, and questions
2 and 3 in particular reach beyond what this ADR can settle on its own.

**Questions 2 and 3 are related but no longer coupled.** An earlier draft
decided them together, on the theory that per-file corpus actions were what made
prebuilt-artifact consumption clean. Review showed the dependency runs the other
way: question 3's recommendation works on the existing corpus actions as they
stand, and question 2 is a separate, larger decision that can be taken — or
declined — afterward without reopening it.

1. **Target architecture.** Recommendation, revised once more after review:
   **hybrid** — platform-derived by default, attribute only for intentional
   cross-compilation. An earlier draft recommended a bare attribute, and its own
   example (`rue_target = "x86-64-linux"`) refuted it: the same target labels
   must build and run natively on all three CI platforms, exactly as today's
   `sh_test`s do by never passing `--target` at all. A fixed attribute cannot; a
   per-architecture label triples the program count; leaving the flag off and
   trusting host detection would leave an acceptance-criterion input implicit.
   So the platform supplies the default — `RueToolchainInfo` carries the
   configured platform's native Rue target, resolved like `toolchains//:rust`
   already is — and the compile always passes the *resolved* target explicitly.

   What survives from the earlier argument, and still matters: **Rue's read
   closure is target-invariant by construction.** `ImportDiscoveryContext::new`
   takes epoch, project root, std root and policy revision — and no target
   (`source_loader.rs:1004-1013`) — and `@import` has no conditional form, so
   `--target` cannot change which sources a program reads. That is why no
   configuration *transition* is needed: there is no target-variant dependency
   graph to request, and it is why one scan action serves every target of a
   root. The configuration's only job here is supplying the native default,
   which toolchain resolution already does.

   **The expiry clause: native resolution applies today; two conditions remain
   for transitions.** Revisit the no-transition stance if a `rue_program` ever
   links an archive that another rule **builds**, since an attribute cannot
   request a dep in the matching configuration and that is precisely what
   transitions are for; or on **publication**, since `platform()`-driven
   cross-compilation is the idiom external users expect (rules_go spent years
   migrating off goos/goarch attributes for exactly this). The first is one
   refactor away, not hypothetical: the `c_ffi` cases' FFI archive is already a
   target-variant *generated* input, synthesized per case inside the harness to
   match the case's `executable_target`
   (`crates/rue-cli-tests/src/main.rs:1949-1971`); the day it becomes a
   Buck-built artifact is the day the condition fires. The hybrid forecloses
   nothing either way: the override attribute can become a transition's output.

2. **Corpus input precision.** The insight that survived four rounds: a
   *scheduling* concern — which shard runs a case, a function of
   `shard-weights.json` — currently keys the corpus actions, and no amount of
   input-tightening removes that coupling while the shard is the action unit.
   The concrete proposal (per-file corpus actions; shard assignment moved up
   into ADR-0069's lane planner) is recorded under **Future Work** rather than
   here, because it is a corpus-topology redesign — it dissolves CLI sharding,
   reaches into the spec and UI corpora, and modifies the harness's whole-corpus
   validation. Recommendation: file it as its own ADR and issue once this one is
   accepted. Nothing in Phases 0–2 waits on it or forecloses it — per-file
   actions would consume the same `RueProgramInfo` artifacts Phase 2 wires in,
   just from finer-grained consumers.

3. **Do the CLI cases migrate out of TOML, or does the harness consume prebuilt
   artifacts?** Recommendation **reversed** after review: **let the harness
   consume prebuilt artifacts** — and, after a further round, this stands on the
   existing corpus actions alone rather than conditionally on question 2.

   The defect Phase 2 fixes is the weld between compile and scenario, not the
   TOML. Handing the 65 scenarios a prebuilt executable fixes exactly that;
   migrating them additionally relocates the authoring surface — by this
   document's own assessment the largest-blast-radius change in it — to buy
   per-scenario CI selection whose value stays speculative until RUE-1267's packer
   names a single scenario as a binding constraint. Once the weld is broken,
   scenario executions are cheap binary runs, and that day may not come.

   The "least TOML-shaped cases" argument that justified migration also expires
   once the weld is broken: such a case is then a program reference,
   `program_args`, inline `files`, and expectations — precisely
   `rue_program_test`'s attribute set. The shapes converge, so the only remaining
   difference is where they are written, and the TOML side keeps the working
   inline-fixture and writable-cwd machinery, keeps RUE-924's audit unchanged
   rather than extended, and avoids a two-mechanism transition entirely.

   The mechanism needs nothing question 2 would build. Each existing CLI corpus
   action declares all ten program artifacts through its `attrs.arg()` env — the
   same `$(location ...)` contract every other corpus input already uses — and
   the harness runs the artifact a case names. The simplest correct form declares
   all ten on every CLI corpus action; that is a mild over-declaration of each
   action's key (an edit to any of the ten roots re-runs all the CLI corpus
   actions — which is also true today, since the roots live inside the declared
   `:examples` filegroup), and it fails closed, since a case whose program is
   missing from the staging environment cannot run. If question 2's per-file
   redesign later lands, the mapping merely gets finer (~14 files naming 1–2
   roots each); if it never lands, nothing here is waiting for it.

   Migration to `rue_program_test` stays the fallback if the staged-binary mode
   proves ugly, and remains available later if individual scenario scheduling
   ever demonstrates real value — RUE-1267's packer naming a single scenario as
   a binding constraint is what that evidence would look like. Nothing in Phase
   0 or 1 is wasted either way, since the large-example scenarios use
   `rue_program_test` regardless.

4. **Test-result caching.** Recommendation unchanged — **don't** — with two
   reinforcements, and RUE-1164's own wording supports the posture: it says
   caching may be *enabled only after* hermeticity validation, not that it must
   be enabled. The first reinforcement: the corpus's scenario work is already
   served by the action cache today, because RUE-1118's corpus suites are cached
   actions — and Phase 2 moves the compiles into cached actions too. The
   population of uncached test executions shrinks to `rue_program_test` runs and
   ordinary unit tests, so the toolchain investment buys even less than stated
   above. And the trust asymmetry deserves saying
   outright: cache writes are authorized by holding the credential, and enabling
   test-result caching widens what a poisoned entry can fake from an *artifact*,
   which still has to run and pass, to a *verdict*, which does not. That is a
   strictly sharper target, and it is the deeper reason the posture is "off until
   the negative controls pass, then a deliberate trust decision" rather than
   ordinary caution.

   One thing to record for any future port: this argument is
   build-system-independent, but the **default is inverted elsewhere**. Bazel
   caches passing test results by default, so on a Bazel surface the posture has
   to be re-asserted (`--nocache_test_results`) rather than merely maintained.
   Deciding it here does not decide it there, and a port that assumes otherwise
   flips the answer silently.

## Future Work

**Per-file corpus actions (from open question 2 — a separate ADR, not part of
this one).** Make the corpus action's unit the case TOML file, comprehended from
`glob(["cases/*.toml"])` at load time, each file-target declaring its own TOML
plus the coarse rare-change inputs; move shard assignment out of the action-key
domain entirely by letting ADR-0069's lane planner pack the file-targets per run
with the existing weights. A case edit then invalidates one action; a stale
weight can unbalance a lane but never affect correctness; `CliShardPlan`,
`CLI_TEST_SHARD_COUNT`, `validate-cli-shard-coverage.py` and the vacuous skew
guard are deleted; RUE-1222's weight refresh stops invalidating anything. Two
priced costs carry over with it: ~323 file-actions repo-wide (214 CLI, 71 spec,
38 UI; directory granularity is the dial if too fine), and contract-graph
validation — which deliberately runs against the complete unfiltered inventory
(`crates/rue-cli-tests/src/main.rs:2876-2878`) — needs a relaxed per-file mode
plus one whole-corpus parse-only validation target. Accepting ADR-0070 neither
approves nor forecloses this; it consumes the same artifacts Phase 2 wires in.

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
  design does not ask for it. It is, however, the seam a future external-build-tool
  integration would most want, and "Forward positioning" states the trade in both
  directions rather than leaving it as a one-line dismissal.
- **External linkers.** `rue_program` pins `--linker internal`. Cases covering
  `clang`/`gcc` linking stay in the harness.
- **No change to what is tested.** Every scenario that runs today runs after, and
  Phase 2 restores scenarios that currently run nowhere. The RUE-924 audit and
  the tier-labelling contracts must survive each phase; under the recommended
  Phase 2 both are untouched, and Consequences records the audit-escape hazard
  that returns if the migration fallback is ever taken.
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
