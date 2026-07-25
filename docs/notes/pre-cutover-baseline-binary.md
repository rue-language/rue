# The pre-cutover baseline binary, and the first real historical comparison

Status: first value-audit runs with a genuine historical baseline. The `rill`,
`mosaic`, `harbor`, and `lattice` rows are protocol evidence (one warmup, seven
alternating paired samples, medians and MADs). The `meridian` row is a
single-sample scouting compile and is **not** protocol evidence; it is recorded
to size the remaining work, and no verdict may cite it.

## The binary

The `[historical_reference]` figures in
[`benchmarks/value-audit/manifest.toml`](../../benchmarks/value-audit/manifest.toml)
came from a prose table with no reproducible artifact behind it. There is one
now, and it is cheap to rebuild:

```sh
# 586f50c is "RUE-1027: make bodies revisioned query terminals" — the body-query
# cutover. Its parent is the last revision before the regression.
git worktree add /path/to/pre-cutover 586f50c~1
cd /path/to/pre-cutover
./buck2 build --target-platforms //platforms:release //crates/rue:rue
```

That parent is `7f61ba2b` ("RUE-1084: extract one-body semantic transactions").
It builds unmodified — no backporting, no toolchain pinning, no patched
dependencies.

It works as `--historical-baseline` for every `regressed_example` workload,
because those are cold-only black-box compiles: the runner's cold driver needs no
session benchmark and no modern incrementality harness. The run below classified
as `three_role_binary_comparison` with `historical_comparison_valid: true` — the
first time the audit has produced a real historical comparison rather than a
same-binary protocol smoke.

## Protocol results

Seven paired samples per role, release binaries, this host.

| workload | historical median | MAD | current median | MAD | ratio |
|---|---:|---:|---:|---:|---:|
| rill | 1054.4 ms | 30.1 | 8129.0 ms | 236.7 | **7.7x** |
| mosaic | 1241.1 ms | 21.1 | 18521.3 ms | 747.8 | **14.9x** |
| harbor | 2082.2 ms | 40.7 | 57119.5 ms | 752.3 | **27.4x** |
| lattice | 3324.7 ms | 75.1 | 57641.8 ms | 953.0 | **17.3x** |

Every MAD is between 0.4% and 4% of its median, so none of these ratios is
noise. All four fail their cold budgets.

## Scouting compile

One compile, same host, same method, not the protocol:

| program | pre-cutover | current | ratio |
|---|---:|---:|---:|
| meridian | 13.41 s | 474.19 s | 35.4x |

The regression is alive on all five programs, between roughly 8x and 35x their
own pre-cutover cost on the same machine with the same corpus. This is not a
stale prose figure and it is not host variance — the control is the same host.

Single scouting compiles ran slower than the protocol medians for the same
programs (harbor 71.6 s vs 57.1 s; lattice 75.6 s vs 57.6 s). The protocol
performs an unrecorded warmup and the scouting compiles did not, so the scouting
figures carry first-compile cost. Where the two disagree, the protocol median is
the number to use.

Meridian's current cost (474 s) lands close to the recorded post-cutover figure
(438 s) even though its pre-cutover cost here is 2.4x the recorded one. One
sample cannot explain that, but it is consistent with the regressed term being
less sensitive to host speed than ordinary compilation is.

### Correction

An earlier revision of this note reported rill at 1.1x and called it repaired.
That was wrong: the "current" measurement had run the pre-cutover binary,
because the shell's working directory was still the pre-cutover worktree and the
binary was addressed by relative path. Re-measured with absolute paths, the
pre-cutover binary compiles rill in 0.81 s and the current binary in 7.8–8.5 s.
Rill is regressed like the rest.

## A separate finding: some programs do not compile reproducibly

The audit failed the correctness gate for two of the four programs before either
reached its timing comparison, with `output artifact changed between paired
samples`. That is not a measurement artifact. Three identical compiles of
`examples/rill/main.rue` produce three different executables:

```
a868033dc18996ee…  /tmp/rill_r1.bin
8adbfd462df9baaf…  /tmp/rill_r2.bin
380c74897c374cd0…  /tmp/rill_r3.bin
```

| program | output reproducible |
|---|---|
| rill | **no** |
| lattice | **no** |
| mosaic | yes |
| harbor | yes |

Both failures reproduce on the pre-cutover binary as well, so **this predates
the query cutover** and is not caused by the incremental work. Whatever the
cause, it is not universal — half the measured corpus is byte-stable — which
should make it tractable to localize by diffing a stable program against an
unstable one.

Existing coverage does not catch it. The `compiler reproducibility` CI job is
RUE-617: it rebuilds the *compiler* from a clean tree and compares that binary.
Output reproducibility is checked by `scripts/test-reproducible-output.sh`
against `reproducibility/fixture`, a small purpose-built corpus. No real
multi-module program is checked for byte-stable output, and the one now measured
is not stable.

This deserves its own issue. It is unrelated to incremental compilation except
that the audit's fail-closed correctness gate is what surfaced it — on the first
run that had real inputs to compare.

## Recalibrating the absolute cold gates

The measured pre-cutover column runs about 2.4x the recorded maintainer-host
figures, on a host with the same nominal core count and memory. That gap is
exactly the hazard an absolute cross-host gate carries, and the protocol medians
show the original 4x multiplier did not absorb it.

Measured against this host's own pre-cutover cost, the 4x gates delivered:

| program | 4x gate | this host's pre-cutover | effective allowance |
|---|---:|---:|---:|
| rill | 1.28 s | 1.05 s | **1.22x** |
| mosaic | 1.96 s | 1.24 s | 1.58x |
| harbor | 3.56 s | 2.08 s | 1.71x |
| lattice | 4.60 s | 3.32 s | 1.38x |
| meridian | 22.80 s | 13.41 s | 1.70x |

Rill's gate sat 22% above what this host achieves *pre-cutover*. A host a
quarter slower than this one would have failed a fully repaired compiler on
that row — a false failure, and precisely the measurement artifact the
multiplier existed to prevent.

The multiplier is therefore raised from 4x to 6x. That restores 1.8x to 2.6x
headroom on this host while leaving today's failures unambiguous:

| program | 6x gate | this host's pre-cutover | allowance | current | over gate |
|---|---:|---:|---:|---:|---:|
| rill | 1.92 s | 1.05 s | 1.83x | 8.13 s | 4.2x |
| mosaic | 2.94 s | 1.24 s | 2.37x | 18.52 s | 6.3x |
| harbor | 5.34 s | 2.08 s | 2.57x | 57.12 s | 10.7x |
| lattice | 6.90 s | 3.32 s | 2.08x | 57.64 s | 8.4x |
| meridian | 34.20 s | 13.41 s | 2.55x | 474.19 s | 13.9x |

Nothing about the verdict changes: every program still fails, by 4x to 14x. The
change buys robustness on slower hosts, which is what an absolute gate is for.

That said, the absolute gate was always the fallback, and it is no longer the
only option. With a buildable baseline binary the role-vs-role pair comparison is
available, and it is host-independent by construction because both roles run on
the same machine in the same run. A run that supplies the baseline should be read
pair-first; the absolute gate is what reports when no baseline is supplied.

## Reproducing

```sh
scripts/rue-value-audit.py \
  --historical-baseline /path/to/pre-cutover/rue \
  --current <current rue> --candidate <current rue> \
  --source-dir historical_baseline=/path/to/pre-cutover \
  --source-dir current_production=. --source-dir candidate=. \
  --workload rill --workload mosaic --workload harbor --workload lattice \
  --output results/value-audit-cold.json
```

Rill and mosaic together take a few minutes; harbor and lattice add roughly
20 minutes each at seven paired samples per role. Meridian is the expensive one
and should be run deliberately rather than inside an iteration loop.
