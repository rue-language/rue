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
recommendation and its evidence, but questions 2 and 3 are coupled and question 2
proposes work large enough to deserve its own ADR.

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
failing the build when the compiler read anything outside `srcs`. A granularity
rule keeps the action count proportionate: a compile becomes its own action when
it is expensive relative to action overhead, or when more than one scenario
consumes its output. That yields 47 programs; the ~4,050 inline-source corpus
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
| Distinct `rue_program` targets proposed | 47 (4 large-example + 9 CLI + 34 auto-discovered) |

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
| CLI cases naming a checked-in root | 65 cases → 10 roots, **9 new programs** | `rue_program` | 1–17 TOML scenarios each, *plus* the automatic-examples and frontend-diff passes; `examples/meridian/main.rue` is the tenth root and is already declared by the row above |
| Auto-discovered `examples/**` roots not already covered | 34 | `rue_program` | Derived exactly: `collect_example_files` yields 45 roots, less the 10 named above and caldera's |
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

So 8 of the 73 cases stay in the harness (6 cross-target, 1 repo-relative, 1
differential), 65 migrate, and they name 10 roots. Only **9** are new programs:
`examples/meridian/main.rue` is also a large-example root, so one artifact serves
both the six CLI scenarios and the slow-tier scenarios. That overlap is the design
working rather than an accounting nuisance — it is exactly the "many scenarios,
one compile" property, reaching across two suites.

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

### The over-declaration audit

Correctness is handled above, in the derivation step. What remains is precision:
a `rue_program` whose `srcs` glob is wider than its real read closure takes cache
misses on files it never reads, which turns a precise action back into a corpus
stamp. Left unchecked, a broad glob undoes the whole point of the rule.

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

**Attach it with `ValidationInfo` rather than as a separate test target.** The
pinned 2026-07-15 buck2 ships it and the bundled prelude already uses it (java,
kotlin, apple). A validation attached to a target runs whenever that target is
transitively reachable from any requested build or test, in parallel with the
build, and a failed required validation fails the build. That is precisely the
shape of "an assertion over an artifact the rule produces anyway", and it
dissolves the question an earlier draft had to ask — which tier do ~50 audit
targets carry, given every compiler change invalidates all of them — rather than
answering it. There are no audit targets to schedule.

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
      establish the mechanism.
- [ ] **Phase 2: break the weld for CLI cases naming a checked-in root** — 65 of
      the 73 cases against 10 roots (9 new programs; meridian's is already declared
      by Phase 1), meridian's six scenarios first, which is where the disabled
      coverage is restored. The 6 cross-target fixture cases, the repo-relative
      fixture case, and the one `differential_opt` case stay in the harness
      regardless. **Whether these scenarios stay in TOML consuming a prebuilt
      executable, or migrate to `rue_program_test` targets, is open question 3 —
      and it is decided together with question 2.** The current recommendation is
      that they stay in TOML, which makes this phase considerably smaller than
      earlier drafts assumed.
- [ ] **Phase 3: Corpus input precision** — see open question 2. A concrete
      proposal now exists (per-file corpus actions, shard assignment moved into
      the lane planner), but it is large enough to warrant its own ADR and issue
      rather than a phase here, and it gates the shape of Phase 2.
- [ ] **Phase 4: Test-result caching decision** — only after the negative controls
      pass on a real branch. See open question 4.

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
- Scenarios become individually selectable and schedulable targets, which is the
  "split" remedy in RUE-1267's alarm.
- ADR-0047 Phases 3 and 4 acquire their first consumer, and the design needs both
  of them, not just one.

### Negative

- `rue_program` is two actions and a `dynamic_output`, not one action. The scan is
  36x cheaper than the compile, but it is not free, and it runs on every cache
  miss.
- Correctness now depends on a derivation script, not only on rule wiring. A bug
  there is a hermeticity bug, so it needs its own unit coverage — the negative
  controls exercise it end to end but will not pin its set arithmetic.
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

All four carry a recommendation now, and three of them changed after review. They
remain maintainer decisions: RUE-1164 is labelled `needs-decision`, and questions
2 and 3 in particular reach beyond what this ADR can settle on its own.

**Decide 2 and 3 together.** They are one decision wearing two numbers — whether
the corpus stays a scheduling unit or becomes a graph of per-file actions
determines whether migrating scenarios out of TOML buys anything.

1. **Target architecture: attribute or configuration?** Recommendation: **a plain
   attribute**, on a stronger argument than the one an earlier draft gave. That
   draft cited the `abi_conformance`/`linker` fixtures as wanting all three
   targets side by side in one package; the granularity repair moved those to the
   harness, so no proposed `rue_program` needs side-by-side targets today, and the
   example is now about keeping the door open rather than serving a consumer.

   The principled argument is that **Rue's read closure is target-invariant by
   construction.** `ImportDiscoveryContext::new` takes epoch, project root, std
   root and policy revision — and no target (`source_loader.rs:1004-1013`) — and
   `@import` has no conditional form, so `--target` cannot change which sources a
   program reads. Buck configurations exist to vary the dependency graph per
   platform; for `rue_program` there is provably nothing for a configuration to
   vary, and the whole difference between two targets' compiles is one
   command-line flag that keys the action as an attribute at zero cost. Revisit
   only if Rue grows target-dependent imports, which ADR-0047's model forbids.

2. **Corpus input precision — a proposal, replacing the three options an earlier
   draft could not choose between.** The root problem is that the current design
   couples a *scheduling* concern (which shard runs a case, a function of
   `shard-weights.json`) into a *correctness* concern (what keys an action). All
   three earlier options managed that coupling; none removed it.

   Recommendation: **make the corpus action's unit the case file, and move shard
   assignment up into the lane planner.**

   - One corpus action per case TOML, comprehended from `glob(["cases/*.toml"])`
     at load time — derived from the tree, so there is no hand-maintained mapping
     and nothing to gate, which meets ADR-0069's ledger standard by construction.
     Each file-target declares its own TOML plus the coarse rare-change inputs
     (compiler, harness, std, fixture and example trees). A case edit then
     invalidates exactly one action, while a compiler or fixture change still
     invalidates everything, which is correct.
   - **`shard-weights.json` leaves the key domain entirely.** ADR-0069 Phase 2's
     planner (implemented and gated) generates the lane split by packing the
     file-targets with the existing weights, regenerated per run and never
     persisted — so stale weights can only unbalance a lane, never affect
     correctness. Coverage stays ADR-0069's union gate. RUE-1267's floor-aware
     packer replaces the interim packing when it lands, and the ordering is
     consistent with ADR-0069 having deliberately scheduled that packer after the
     distribution stops changing: this conversion *is* the distribution change.
   - **What it deletes:** `CliShardPlan` and the LPT code in
     `crates/rue-cli-tests/src/sharding.rs`, `CLI_TEST_SHARD_COUNT`,
     `scripts/validate-cli-shard-coverage.py`, and the skew guard ADR-0069 §6
     proved vacuous. RUE-1222's scheduled weight refresh becomes free: today it
     would invalidate every shard action; afterward it touches no action key.
   - A heavyweight file that dominates a lane is RUE-1267's indivisible-item alarm
     firing with a mechanical remedy — split the TOML, an ordinary authoring act.

   Two costs, both verified, both belonging in the decision rather than in a
   footnote. **Scale:** ~323 file-actions repo-wide — 214 CLI, 71 spec, 38 UI,
   since spec and UI declare whole `cases/**` filegroups today too
   (`crates/rue-spec/BUCK`, `crates/rue-ui-tests/BUCK`) — at roughly 8–13 cases
   each. That sits above action overhead, and if it is judged too fine the same
   mechanism works at directory granularity (spec already has 10 subdirectories);
   the dial is grouping, not design. **One real harness change:** contract-graph
   validation deliberately runs against the complete unfiltered inventory before
   any filter is applied (`crates/rue-cli-tests/src/main.rs:2876-2878`), so a
   per-file action staged with only its own TOML needs a mode that relaxes
   cross-file checks, with contract completeness, duplicate-name detection and
   RUE-924-style counting moving to one whole-corpus parse-only validation target
   whose coarse key is fine because it costs seconds.

   **Scope caveat, and the reason this stays open.** This is a larger change than
   "a phase of RUE-1164": it dissolves CLI sharding, reaches into the spec and UI
   corpora, and modifies the harness. It is a good answer to the question, but it
   probably deserves its own ADR and issue rather than living as Phase 3 here.

3. **Do the CLI cases migrate out of TOML, or does the harness consume prebuilt
   artifacts?** Recommendation **reversed** after review: **let the harness consume
   prebuilt artifacts**, conditional on question 2.

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

   The condition: this is clean *because of* question 2. Under per-file actions,
   `examples_meridian.toml`'s target declares `:meridian` as an input and runs six
   scenarios against it — one compile action, one scenario action, selectable at
   file granularity. The per-file targets naming checked-in roots declare deps on
   the programs their cases consume; that mapping is small (~14 files, 1–2 roots
   each) and fails closed, since a case whose program is absent from the staging
   environment cannot run. If question 2 is decided the other way and the shards
   stay monolithic, the calculus shifts back toward migration, because per-scenario
   targets become the only route to individual scheduling.

   Migration stays the fallback if the prebuilt-consuming mode proves ugly — the
   staging-environment wiring is the risk — and nothing in Phase 0 or 1 is wasted
   either way, since the large-example scenarios use `rue_program_test` regardless.

4. **Test-result caching.** Recommendation unchanged — **don't** — with two
   reinforcements. Under question 2's proposal the corpus's scenario work is
   already served by the action cache, because per-file corpus actions are cached
   actions; the population of uncached test executions shrinks to
   `rue_program_test` runs and ordinary unit tests, so the toolchain investment
   buys even less than stated above. And the trust asymmetry deserves saying
   outright: cache writes are authorized by holding the credential, and enabling
   test-result caching widens what a poisoned entry can fake from an *artifact*,
   which still has to run and pass, to a *verdict*, which does not. That is a
   strictly sharper target, and it is the deeper reason the posture is "off until
   the negative controls pass, then a deliberate trust decision" rather than
   ordinary caution.

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
