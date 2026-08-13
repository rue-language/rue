# ADR-0071 Phase 1 reference baseline

This report closes the measurement phase of ADR-0071. It records the first
complete `fresh_source_to_native_v1` observations from the release ThinLTO
compiler, compiling frozen Lattice with Rue `-O3` through successful native
output publication. The boundary proof rejects retained sessions, daemons,
precompiled artifacts, hidden cache hits, alternate pipelines, and incomplete
stage or input provenance.

## Reference result

Hosted collection [run 31659335914](https://github.com/rue-language/rue/actions/runs/31659335914)
measured commit `72d2155a57046f6a2ca05b357e8f8d4d5c32b7a4`. All three platform lanes
completed and their canonical run objects entered epoch 5:

| platform | run object | Lattice process median | peak RSS median |
| --- | --- | ---: | ---: |
| x86_64 Linux, reference | `9b7d4bdb4bf398301fa5bd90b6a57e75f4764a982aacf2723c485b59b5398394` | 2,887.26 ms | 310.9 MiB |
| AArch64 Linux, directional | `08deff0a5bd6beb187063db4ed31d70634067cf9aa9d5c453a78be828fa953e0` | 3,096.26 ms | 308.9 MiB |
| AArch64 macOS, directional | `1404128c40051b386dd7b545c9eb555333004f0774ec99689df1fce296042f18` | 1,940.71 ms | 378.9 MiB |

Only x86_64 Linux adjudicates the absolute target. Its three Lattice process
samples were 2,914.57, 2,887.26, and 2,878.98 ms, giving an 8.28 ms MAD. The
manifest keeps the 250 ms product goal separate from a fixed initial
non-regression gate of 3,022.38 ms. The gate is the baseline median plus six
times the hosted calibration's 0.78% relative MAD; the baseline run's own three
samples had an 8.28 ms MAD. It never widens from a later noisy observation. A
regression run remains comparable and is published as evidence, but fails
collection after upload.

The reference build is therefore 11.55 times slower than the 250 ms goal and
uses 2.43 times the provisional 128 MiB envelope. It is also above the first
500 ms / 256 MiB milestone, so both latency and retained memory are active work.

## Worker scaling

Hosted scaling [run 31659468677](https://github.com/rue-language/rue/actions/runs/31659468677)
measured the same commit and boundary across one, two, four, eight, and
automatic workers on a four-core x86_64 Linux runner. Its raw report has content
address `af2c1f5262fd03447d825c294075a6d4dbd53456d02bc04d1333474099b59189`.
Each cell is the median of three independent fresh compiler processes; output
and one-worker deterministic work were exact.

| Lattice workers | process | compiler root | peak RSS | utilization |
| --- | ---: | ---: | ---: | ---: |
| 1 | 3,069.85 ms | 2,848.06 ms | 311.3 MiB | 78.8% |
| 2 | 2,551.04 ms | 2,337.79 ms | 310.3 MiB | 57.4% |
| 4 | 2,181.95 ms | 1,952.47 ms | 313.1 MiB | 43.8% |
| 8 | 2,279.01 ms | 2,049.66 ms | 318.1 MiB | 34.0% |
| automatic (4) | 2,246.81 ms | 2,012.30 ms | 314.6 MiB | 43.1% |

Automatic workers improve Lattice process latency by 1.37 times and compiler
root latency by 1.42 times. The smaller programs show the same bounded shape:
automatic-worker process speedups are 1.31 times for Ruelex, 1.28 times for
Mosaic, and 1.39 times for Harbor. Four workers are near the best point; asking
eight workers to contend on four cores is slower for every maintained program.

## What limits scaling

Parallelism is useful but not the dominant route to the 250 ms goal:

- Lattice CFG/optimization falls from 838.33 to 415.18 ms and backend work from
  369.50 to 148.44 ms at automatic workers. Those parts scale materially.
- The semantic phase falls only from 1,253.49 to 1,106.75 ms. Summed semantic
  body time grows from 722.62 to 1,454.58 ms while wall time barely moves,
  showing coordination and repeated/shared work rather than an absence of body
  parallelism.
- Query-worker utilization falls from 78.8% on one worker to 43.1% across four
  automatic workers. Ready-wait maximum improves from 695.45 to 269.86 ms, but
  the longest producer chain remains ten nodes and total active work rises.
- Toolchain acquisition remains 1,280.70 ms at one worker and 1,136.40 ms at
  automatic workers. It sits inside the semantic ownership boundary and is the
  leading current-source audit target, not linking.
- Linking is 24.33 ms at one worker and 25.60 ms at automatic workers. It is
  measurable, but cannot explain the multi-second gap.
- External process time outside compiler root is roughly 220–235 ms for
  Lattice, so process/orchestration cost alone already approaches the final
  250 ms whole-build goal and must be decomposed later.

The next step is therefore the semantic-to-CFG ownership and repetition audit
in RUE-1474, using the exact work counts and these critical-path observations.
It should prioritize toolchain/semantic ownership and repeated immutable fact
observation while preserving body parallelism, fine invalidation, one canonical
pipeline, and exact output.
