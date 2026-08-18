# Compiler worker scaling

Status: measurement and design note, 2026-08-18. The ADR-0071 cold-build
campaign measured everything at one worker by design, so nothing had
characterized what `-j` actually buys. Executables are byte-identical across
worker counts, so this is purely a throughput question. Current source and the
measurements below are authoritative.

## Result

Additional workers buy nothing, and on three of four shapes they cost.

- **Lattice at `-j4` is 0.99x of `-j1`.** Eighteen balanced adjacent pairs,
  median paired speedup 0.986x, eight of eighteen pairs won by `-j4`. Mosaic,
  a generated 512-module wide shape, and a generated 256-module chain are all
  *slower* at `-j2` and `-j4` than at `-j1` on both minimum and median.
- **The cause is not the dependency graph.** A shape with 512 mutually
  independent modules regresses exactly like a shape whose semantic frontier is
  one body wide at every step. Turning concurrency on is what costs, not
  running things concurrently.
- **The mechanism is proof re-acquisition.** At concurrency 1 a registered
  batch executes inline on the requesting task, so the endorsement identities
  it proves accumulate and later items hit them. At concurrency above 1 each
  item gets a child task whose task-local endorsement set starts empty, so
  certificates the parent already proved are rejected per child and the cone is
  re-validated. Lattice performs **8.6x** the retained-terminal validation
  traversals at `-j2` that it performs at `-j1`, on an *identical* set of query
  claims.
- **Phase boundaries are hard barriers.** `mixed_parallel_ns` is exactly zero
  in all 318 samples at every worker count. No two published phases are ever
  concurrently active, so whatever a phase fails to parallelize is lost outright.
- **What does scale**: the backend (2.52x paired at `-j4` on Lattice) and CFG
  construction and optimization (1.85x). **What does not**: semantic analysis
  (0.46x paired — that is 2.2x *slower*), source discovery and parsing,
  object generation, and linking.
- Nothing here is byte-identity-unsafe today and nothing here is fixed by a
  bounded change. The three candidate designs below are one ADR, one
  counter-rebaseline, and one policy decision. **No implementation is proposed
  in this note.**

## The host, and what it can honestly support

Four cores. `performance/scaling.toml` declares a five-row worker matrix up to
eight, and this machine cannot honestly produce the eight-worker row, so every
eight-worker observation below is labelled advisory oversubscription and no
conclusion rests on one.

The host was characterized before measuring rather than assumed:

| probe | result |
| --- | --- |
| `nproc` | 4 |
| CPU | Intel Xeon @ 2.80GHz, 15 GiB |
| 5 s CPU breakdown while idle | user 0.5%, system 0.4%, idle 99.1%, **steal 0** |
| single-core fixed loop, 25 repeats | min 0.326 s, median 0.334 s, MAD **1.2%**, max/min 1.19x |
| same loop on k independent processes | k=2 → 1.88x throughput, k=4 → **3.54x**, k=8 → 3.70x |

So the machine really does deliver 3.54x on four cores, and its short-window
timing noise is 1.2%. A compiler that fails to scale here is failing on its own
account.

That does **not** make the host quiet at the ten-minute scale, and Lattice in
particular is unmeasurable on it by wall clock. On 46 one-worker samples with
bit-identical `compiler_work`, Lattice ranges from 2,736 ms to 23,392 ms, and a
later control run saw 43,913 ms. The spread is not localized: splitting those
46 into the fastest and slowest eight, semantic analysis contributes 41% of the
gap, CFG 42%, and unattributed 10% — every band inflates together, peak
resident memory stays flat at 285-290 MB, and the worst sample spends 4,306 ms
in parsing against a 139 ms norm.

To find out whether that is the host or the compiler, fourteen Lattice `-j1`
compiles were interleaved one-for-one with the same fixed register-bound
arithmetic loop used above, so both see the same host epoch:

| | max/min over the sequence | Pearson r against the other |
| --- | ---: | ---: |
| control loop (pure CPU, no allocation) | 1.55x | 0.714 |
| Lattice `-j1` (identical `compiler_work` every run) | **10.44x** | 0.714 |

Both readings matter. The host really does drift — a loop that touches nothing
but registers still varies 1.55x, and the compiler's wall time tracks it at
r=0.71 — so the machine is not quiet at this timescale. But the compiler
amplifies that drift by roughly sevenfold, which a CPU-throughput story alone
does not explain; the plausible reading is that a 290 MB, allocation-heavy
process is exposed to memory-subsystem contention that a register-bound loop is
not. This note does not diagnose that further, because it is **not
worker-count-dependent**: the entire effect is measured inside `-j1` samples
doing bit-identical work.

The consequence is a protocol, not an excuse. Every sample is interleaved
round-robin across `(workload, worker)` cells so drift spreads evenly instead
of pooling in whichever cell ran last; the Lattice headline is a **balanced
adjacent-pair** comparison, because only adjacent samples share a host epoch;
and every load-bearing claim below is either a paired ratio or a deterministic
counter. Absolute Lattice milliseconds are context, not evidence. This is the
same conclusion the
[per-body identity note](per-body-identity-closure-materialization.md) reached
when it measured a 28.69% paired MAD on this machine and refused to quote wall
clock at all.

## What was measured

Release build (`-Copt-level=3 -Clto=thin`) of the canonical compiler at trunk
`32c0e83b`, `RUE_STD_PATH` at the repository `std`, `-O3`, one fresh compiler
process per sample asked for `--benchmark-json`, output written and discarded.

| shape | root | files | modules | lines | tokens | functions |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Lattice | `performance/workloads/lattice/main.rue` | 161 | 161 | 20,462 | 160,221 | 1,221 |
| Mosaic | `examples/mosaic/main.rue` | 83 | 83 | 13,540 | 84,152 | 588 |
| wide512 | generated, scratch-only | 514 | 514 | 20,508 | 147,565 | 515 |
| chain256 | `performance/workloads/scale_modules/n256/main.rue` | 257 | 257 | 1,801 | 8,213 | 257 |

Counts are the compiler's own, so they include the standard library the root
program reaches.

The two generated shapes exist to separate width from depth, which the
maintained programs conflate.

- **wide512** is 512 leaf modules that each import one shared base and nothing
  else, three real function bodies apiece. Depth is fixed at two regardless of
  N; only the number of mutually independent modules grows. It is close to the
  most favourable shape the scheduler could be handed.
- **chain256** is the committed module-count probe: 256 modules in an import
  chain, one floor body each. Its semantic frontier is **one body wide in all
  257 of its recorded frontier batches** — there is no parallel semantic work
  to find. It is the control.

wide512 is deliberately not committed. It is a diagnostic that answered its
question; committing it would add a workload to a suite whose membership is a
revision event under ADR-0067, for a shape whose conclusion this note records.

Byte identity was verified rather than assumed. Across all 318 samples — every
shape at every worker count, including the advisory eight-worker rows — the
compiler-reported `emitted_output_sha256` has exactly **one** distinct value
per shape. The additive phase partition also holds exactly, in integer
nanoseconds, in all 318.

## The measured curve

Thirteen interleaved repetitions per cell, four shapes, three worker counts;
Lattice additionally measured as eighteen balanced `-j1`/`-j4` adjacent pairs.
Minimum and median are both reported because on this host they disagree about
Lattice and agree about everything else.

### Root wall clock

| shape | -j | n | min ms | median ms | MAD | speedup (min) | speedup (median) | efficiency (min) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Lattice | 1 | 13 | 4,125 | 8,373 | 37% | 1.00x | 1.00x | 100% |
| Lattice | 2 | 13 | 4,073 | 5,799 | 18% | 1.01x | 1.44x | 51% |
| Lattice | 4 | 13 | 3,022 | 4,676 | 30% | 1.36x | 1.79x | 34% |
| Mosaic | 1 | 13 | 979 | 1,066 | 4% | 1.00x | 1.00x | 100% |
| Mosaic | 2 | 13 | 1,366 | 1,826 | 15% | 0.72x | 0.58x | 36% |
| Mosaic | 4 | 13 | 1,268 | 1,953 | 30% | 0.77x | 0.55x | 19% |
| wide512 | 1 | 13 | 829 | 931 | 5% | 1.00x | 1.00x | 100% |
| wide512 | 2 | 13 | 1,208 | 1,574 | 23% | 0.69x | 0.59x | 34% |
| wide512 | 4 | 13 | 997 | 1,392 | 17% | 0.83x | 0.67x | 21% |
| chain256 | 1 | 13 | 249 | 277 | 8% | 1.00x | 1.00x | 100% |
| chain256 | 2 | 13 | 328 | 695 | 40% | 0.76x | 0.40x | 38% |
| chain256 | 4 | 13 | 383 | 510 | 22% | 0.65x | 0.54x | 16% |

Mosaic, wide512, and chain256 are unambiguous and agree with each other: both
statistics say more workers is slower, and the `-j1` cells are the stable ones
(4%, 5%, 8% MAD against 15-40% once concurrency is on). Turning concurrency on
does not only cost time, it costs predictability.

Lattice's row is the one the host makes unreadable — its `-j1` MAD is 37%, so
neither its min nor its median can carry a conclusion. The paired protocol is
what settles it:

| Lattice, 18 balanced adjacent `-j1`/`-j4` pairs | value |
| --- | ---: |
| median paired speedup of `-j4` over `-j1` | **0.986x** |
| mean paired speedup | 1.055x |
| range | 0.62x - 1.92x |
| paired MAD | 24.2% |
| pairs won by `-j4` | 8 / 18 |

Four workers on the largest maintained program is a coin flip against one.

An advisory eight-worker row was collected on this four-core host in a separate
eleven-repetition campaign. It is oversubscription, not scaling, and it is
recorded only so nobody re-measures it: Lattice min 3,341 ms (1.06x),
Mosaic min 1,583 ms (0.63x). Neither is evidence of anything except that
oversubscription is not the missing ingredient.

### Per phase

Bands are taken from the fastest sample of each cell so they sum exactly to
that run's `compiler_root_ns`. `program_construction` and `mixed_parallel` are
zero in every sample and are omitted.

| shape | band | -j1 ms | -j2 ms | -j4 ms | -j2 | -j4 |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Lattice | source discovery and parsing | 138 | 140 | 131 | 0.99x | 1.06x |
| Lattice | **semantic analysis** | 2,027 | 2,221 | 1,545 | **0.91x** | **1.31x** |
| Lattice | cfg and optimization | 1,252 | 1,143 | 815 | 1.10x | 1.54x |
| Lattice | backend | 332 | 236 | 130 | 1.41x | 2.55x |
| Lattice | object generation | 31 | 29 | 40 | 1.07x | 0.78x |
| Lattice | linking | 50 | 22 | 26 | 2.30x | 1.92x |
| Lattice | unattributed | 295 | 283 | 335 | 1.04x | 0.88x |
| Mosaic | source discovery and parsing | 69 | 84 | 69 | 0.83x | 1.00x |
| Mosaic | **semantic analysis** | 496 | 853 | 761 | **0.58x** | **0.65x** |
| Mosaic | cfg and optimization | 202 | 174 | 174 | 1.16x | 1.16x |
| Mosaic | backend | 126 | 95 | 114 | 1.33x | 1.11x |
| Mosaic | unattributed | 69 | 141 | 126 | 0.49x | 0.54x |
| wide512 | source discovery and parsing | 166 | 166 | 167 | 1.00x | 0.99x |
| wide512 | **semantic analysis** | 423 | 696 | 565 | **0.61x** | **0.75x** |
| wide512 | cfg and optimization | 94 | 104 | 58 | 0.90x | 1.62x |
| wide512 | backend | 56 | 75 | 34 | 0.74x | 1.66x |
| wide512 | unattributed | 79 | 153 | 141 | 0.52x | 0.56x |
| chain256 | source discovery and parsing | 56 | 50 | 53 | 1.13x | 1.06x |
| chain256 | **semantic analysis** | 129 | 219 | 276 | **0.59x** | **0.47x** |
| chain256 | cfg and optimization | 24 | 21 | 17 | 1.13x | 1.41x |
| chain256 | backend | 12 | 11 | 9 | 1.18x | 1.34x |

Bands below roughly 100 ms carry the host's noise rather than a signal;
Lattice's 2.30x "linking" speedup and 0.78x "object generation" are 20-50 ms
cells and mean nothing. The load-bearing rows are the bolded ones and the two
that scale.

Lattice's per-phase story is confirmed by its paired protocol, which is
immune to the drift that makes the single-run column above look better than it
is:

| Lattice band | paired `-j4` speedup (median of 18) | efficiency |
| --- | ---: | ---: |
| source discovery and parsing | 0.949x | 24% |
| **semantic analysis** | **0.462x** | **12%** |
| cfg and optimization | 1.848x | 46% |
| backend | 2.523x | 63% |
| **root** | **0.986x** | **25%** |

Semantic analysis at four workers takes **2.16x as long** as at one. It is
also, at one worker, either the largest or second-largest band on every shape
measured (49.3% of Mosaic, 49.5% of wide512, 51.5% of chain256, 31.2% of
Lattice). The compiler's biggest phase is its anti-scaling one.

### Worker utilization

Derived from summed permit-holding time over compiler-root elapsed, which is
what the scaling suite already publishes.

| shape | -j | peak workers | worker-active ms | root ms | mean busy workers | utilization |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Lattice | 1 | 1 | 7,036 | 8,373 | 0.84 | 84% |
| Lattice | 2 | 2 | 6,404 | 5,799 | 1.10 | 55% |
| Lattice | 4 | 4 | 8,164 | 4,676 | 1.75 | 44% |
| Mosaic | 1 | 1 | 885 | 1,066 | 0.83 | 83% |
| Mosaic | 2 | 2 | 1,652 | 1,826 | 0.90 | 45% |
| Mosaic | 4 | 4 | 3,107 | 1,953 | 1.59 | 40% |
| wide512 | 1 | 1 | 714 | 931 | 0.77 | 77% |
| wide512 | 2 | 2 | 1,439 | 1,574 | 0.91 | 46% |
| wide512 | 4 | 4 | 1,203 | 1,392 | 0.86 | **22%** |
| chain256 | 1 | 1 | 184 | 277 | 0.66 | 66% |
| chain256 | 2 | 2 | 363 | 695 | 0.52 | 26% |
| chain256 | 4 | 4 | 303 | 510 | 0.59 | 15% |

wide512 is the number to sit with. Five hundred and twelve modules with no
dependency between them, and the mean number of permit-holding tasks at four
workers is **0.86** — below one. The scheduler is not failing to find work
because the program has none.

### The serial fraction, and what it would be worth

Median one-worker band profiles, pooling every one-worker sample collected
(Lattice n=46, Mosaic n=28, generated shapes n=13):

| shape | structurally serial | nominally parallel | Amdahl ceiling at `-j4` | ceiling at infinite workers |
| --- | ---: | ---: | ---: | ---: |
| Lattice | 817 ms, **13.7%** | 5,141 ms, 86.3% | 2.83x | 7.29x |
| Mosaic | 162 ms, **15.9%** | 855 ms, 84.1% | 2.71x | 6.28x |
| wide512 | 287 ms, **31.1%** | 637 ms, 68.9% | 2.07x | 3.22x |
| chain256 | 90 ms, **34.0%** | 174 ms, 66.0% | 1.98x | 2.94x |

"Structurally serial" is source discovery and parsing, program construction,
object generation, linking, and unattributed — the bands that are not expressed
as registered query batches and therefore have no path to a second worker.
Parsing measures 1.00x at every worker count on every shape; unattributed is
flat on Lattice and roughly doubles on Mosaic and wide512, so if anything the
serial fraction is understated above. "Nominally parallel" is semantic
analysis, CFG, and backend, the three that are submitted as registered
batches.

This is the shape of the opportunity. **The serial fraction is not the
problem.** On Lattice it is 13.7%, which permits 2.83x at four workers and
7.29x in the limit; the compiler delivers 0.99x. The gap between 2.83x and
0.99x is not Amdahl's law, and no amount of shortening parsing or linking
closes it. It is entirely (a) semantic analysis scaling at 0.46x instead of
4x, and (b) CFG and backend scaling at 1.85x and 2.52x instead of 4x.

The generated shapes carry a second reading: their serial fraction is twice
Lattice's, and almost all of the difference is parsing (19.0% of wide512,
22.5% of chain256, against 2.5% of Lattice). Source discovery and parsing is
strictly serial — it measures 1.00x at every worker count on every shape — and
on a module-heavy program it is already a fifth of the build. That is a real
ceiling for wide programs, but it is second in line behind a phase that is
currently negative.

## Why more workers is slower

The wall clock says what happens; the counters say why, and they are
host-independent.

### The compiler computes exactly the same things

| shape | `claims` at -j1 | -j2 | -j4 |
| --- | ---: | ---: | ---: |
| Lattice | 32,880 | 32,880 | 32,880 |
| Mosaic | 15,809 | 15,809 | 15,809 |
| wide512 | 27,248 | 27,248 | 27,248 |
| chain256 | 8,759 | 8,759 | 8,759 |

Not one extra query body is evaluated. Whatever the extra time is, it is not
recomputation of results.

### It re-proves them many times over

Retained-terminal validation work, median, relative to the same shape's
one-worker run:

| shape | -j | traversals | node visits | memo misses | **proof reacquisition misses** | endorsement probes | registry probes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Lattice | 1 | 38,587 | 242,296 | 23,872 | 23,862 | 266,257 | 280,873 |
| Lattice | 2 | **8.40x** | 5.23x | 8.62x | **8.62x** | 5.96x | 5.66x |
| Lattice | 4 | 8.47x | 5.24x | 8.72x | 8.72x | 5.98x | 5.69x |
| Mosaic | 1 | 19,502 | 99,328 | 12,538 | 12,533 | 111,907 | 118,824 |
| Mosaic | 2 | **5.66x** | 3.70x | 5.45x | **5.45x** | 4.24x | 4.02x |
| Mosaic | 4 | 5.75x | 3.72x | 5.58x | 5.58x | 4.27x | 4.05x |
| wide512 | 1 | 28,757 | 69,794 | 14,898 | 14,897 | 83,681 | 98,039 |
| wide512 | 2 | **2.05x** | 1.58x | 1.74x | **1.74x** | 1.86x | 1.72x |
| wide512 | 4 | 2.05x | 1.59x | 1.75x | 1.75x | 1.86x | 1.73x |
| chain256 | 1 | 8,486 | 18,477 | 4,630 | 4,629 | 24,390 | 26,962 |
| chain256 | 2 | **2.09x** | 1.56x | 1.83x | **1.83x** | 1.81x | 1.72x |
| chain256 | 4 | 2.09x | 1.56x | 1.83x | 1.83x | 1.81x | 1.72x |

Three things in that table matter.

**The jump is entirely at one worker to two.** Every column is flat from `-j2`
to `-j4`. This is a mode switch, not contention: the cost is paid for having
concurrency configured, not for using it.

**`proof_reacquisition_misses` tracks `memo_misses` almost exactly** — 23,862
against 23,872 on Lattice at one worker, and the same 8.6x multiplier on both.
The counter's own documentation says what it counts: *"memo misses caused only
by a registered proof scope lacking the exact lease."* These are nodes with a
live, valid, revision-compatible certificate that is refused because the asking
task cannot show it holds the lease, and so the cone below is walked again.

**chain256 is the control that closes the argument.** Its semantic frontier is
one body wide in every batch, at every worker count — the reachability counters
are identical across `-j1`, `-j2`, and `-j4` (258 scans, 257 batches, 257
keys, 257 width-one buckets, 0 serial transactions). There is no
parallelism to exploit and none is exploited, and it still pays 2.09x the
traversals and runs 1.7x slower in semantic analysis at `-j2`. Nothing about
concurrent execution is required to produce the regression.

The magnitude tracks how much shared structure a program has: Lattice, with a
dense shared nominal closure, pays 8.6x; the two generated shapes, whose
modules share only a trivial base, pay 1.8x. That is consistent with the cost
being re-traversal of shared dependency cones.

### Confirmed against retired instructions

Wall clock on this host cannot carry a 15% claim, so the extra work was also
counted deterministically with `valgrind --tool=callgrind` on the release
binary, which serializes threads but counts every instruction either way.

| shape | -j1 | -j2 | -j4 | -j2 vs -j1 | -j4 vs -j1 |
| --- | ---: | ---: | ---: | ---: | ---: |
| Lattice | 7,039,280,944 | 8,554,995,478 | 8,556,158,917 | **+21.53%** | +21.55% |
| Mosaic | 2,883,421,815 | 3,366,677,207 | 3,382,332,944 | **+16.76%** | +17.30% |
| wide512 | 2,266,960,882 | 2,448,531,043 | 2,465,986,535 | **+8.01%** | +8.78% |
| chain256 | 512,469,556 | 577,899,127 | 578,893,622 | **+12.77%** | +12.96% |

Lattice retires **1.52 billion extra instructions** — 21.5% of the entire
build — purely for having been asked for more than one worker, and the number
is the same at two workers as at four, which is the mode switch again in a
metric with no host in it. The one-worker Lattice total also cross-checks
against the independently measured 7,249,286,766 in the
[per-body identity note](per-body-identity-closure-materialization.md); the
small difference is trunk movement between the two measurements.

The instruction increase is real but smaller than the wall-clock regression,
which locates the remainder: parking, permit donation, and cross-task
synchronization, which retire few instructions and consume wall time. The
runtime reports 1,117 donated permits on Mosaic at `-j2` and 2,430 on Lattice,
against essentially no joins in the same runs — Lattice records **2** joins at
`-j2` and 9 at `-j4`, against 32,880 claims. Tasks park and donate permits
constantly and almost never find an in-flight computation worth joining.

### Where the mechanism lives in the source

Two sites in `crates/rue-query/src/lib.rs`, and the comment at the second one
states the behaviour plainly. Line numbers are as of trunk `32c0e83b`.

`QueryContext::query_registered_adaptive_batch` (line 8425) branches on
concurrency before it does anything else:

```rust
if self.max_concurrency() == 1 {
    return keys
        .into_iter()
        .map(|key| self.query_registered_ref(family, &key))
        .collect();
}
```

`query_registered_ref` evaluates on `self.task` — the *requesting* task. Every
item in the batch therefore shares one `ValidationEndorsementScope`, and its
`identities` set accumulates across the whole batch, so item 400 finds what
items 1 through 399 proved.

Above one worker, each item is spawned as a child task, and `Task::batch_child`
(line 10370) builds that child's scope:

```rust
// A batch child is a structured descendant of the lexical proof
// scope. It starts with no task-local endorsements, but keeps the
// same borrowed published authority live while it validates its
// own selected cone.
validation_endorsements: Mutex::new(
    inherited_validation_fallbacks
        .map(|fallbacks| ValidationEndorsementScope {
            identities: AHashSet::new(),
            fallbacks,
        })
```

`identities: AHashSet::new()`. The child inherits coarse retained-pin
`fallbacks` and a published `batch_validation_authority`, but not the exact
`(incarnation, stamp, revision)` identities the parent has already proved.
`Task::validation_endorsement_authority_at_raw`
(`crates/rue-query/src/lib.rs:10610`) consults `scope.identities` first and
returns `TaskLocal` on a hit; with an empty set it falls through to the coarser
checks, and where those miss it returns `Missing`, which is precisely the
`proof_reacquisition_miss` branch at `crates/rue-query/src/lib.rs:5925`.

That is the whole regression: one worker gets a warm per-task proof cache
covering the entire batch, N workers get N cold ones.

### A secondary, smaller effect

The coordinator that drives semantic reachability
(`crates/rue-compiler/src/revisioned_query_database.rs:14987`) also branches on
concurrency, and above one worker it submits its ready window through
`query_registered_batch`, which spawns a child task unconditionally — including
for a one-key window. On chain256 that is 514 single-item child-task spawns
(257 toolchain-demand batches plus 257 transaction batches) where one worker
performs plain inline calls. This is a real cost on chain-shaped programs and
close to irrelevant on Lattice and Mosaic, whose 1,263 and 630 bodies arrive in
eleven batches averaging over a hundred keys each.

## Candidate designs

Each is stated with the saving reachable from the numbers above, its effect on
deterministic artifacts, and what authority it needs. Determinism across worker
counts is a hard falsifier in this codebase: any change must keep `-j1` output
byte-identical to `-jN`. All three below do, because none of them changes what
is computed — `claims` is already identical across worker counts.

### 1. Give a batch child the parent's proved endorsement identities

The direct fix for the measured cause. A batch child would start from a
snapshot of (or shared read access to) the parent scope's `identities` set
rather than an empty one, so a certificate the parent already proved is
honoured by every sibling.

- **Reachable saving.** Removes the 8.6x / 5.5x / 1.8x validation multiplier,
  which is the whole of semantic analysis's 0.46x paired scaling on Lattice.
  Substituting a different semantic speedup into Lattice's pooled one-worker
  band profile (5,958 ms of banded time) while holding every other band at its
  measured paired ratio:

  | semantic paired `-j4` speedup | predicted root | predicted speedup |
  | --- | ---: | ---: |
  | 0.462x — measured today | 6,538 ms | 0.91x |
  | 1.848x — what CFG already achieves | 3,523 ms | **1.69x** |
  | 2.523x — what the backend already achieves | 3,255 ms | 1.83x |
  | 4.0x — perfect, CFG and backend unchanged | 2,983 ms | 2.00x |
  | 4.0x, with CFG and backend also perfect | 2,110 ms | 2.82x |

  The model reproduces the measured cell as 0.91x against a measured 0.986x, an
  8% error, so treat these as one significant figure. The reading is that
  merely bringing semantic analysis up to the scaling CFG *already has* is
  worth about **1.7x** on Lattice, and that the 2.83x Amdahl ceiling needs this
  *and* the sub-linear CFG and backend fixed.
- **Deterministic artifacts.** Executables unaffected — nothing changes what is
  computed. Published validation counters at `-jN` change substantially and
  need a rebaseline; `-j1` counters are untouched, because `-j1` never takes
  this path. Note that parallel-row counters are already permitted to vary run
  to run, and the one-worker row is the structural probe.
- **Risk.** High, and it is correctness risk rather than performance risk. The
  empty set is not an oversight — the comment at the site is deliberate, and
  the endorsement scope is what makes a registered proof sound about which
  leases the *asking task* holds. Sharing identities across tasks means
  reasoning about a lease one task holds being used to justify another task
  skipping validation, which is exactly the property the current split
  guarantees away. There is a spectrum here (snapshot at spawn, copy-on-read,
  publish-on-terminal into the existing `batch_validation_authority`) and
  choosing a point on it is a soundness argument, not an optimization.
- **Authority: needs an ADR.** This is the query runtime's proof contract.

### 2. Keep the structured-batch path off a one-item window

Have the reachability coordinator submit its ready window through
`query_registered_adaptive_batch` rather than `query_registered_batch`. The
adaptive entry point already collapses to an inline `query_registered` at one
key and at a zero worker claim, and its comment on the latter says that path is
chosen "to preserve the same ordered dependency observations in this task" —
so the collapse is intended to be observationally equivalent.

- **Reachable saving.** Bounded and shape-dependent. It removes 514 spurious
  child-task spawns on chain256, whose semantic band is 129 ms at one worker
  and 219 ms at two; it removes essentially nothing on Lattice and Mosaic,
  where windows are wide. This does not move the headline.
- **Deterministic artifacts.** Executables unaffected. `-j1` unaffected —
  the coordinator's one-worker branch is a separate early `continue`. `-jN`
  counters change for chain-shaped programs.
- **Risk.** Low but not nil: the two entry points differ in nested-request
  accounting and the substitutability claim is a comment, not a test.
- **Authority: needs a counter rebaseline**, and a test that pins the
  equivalence the comment asserts.

### 3. Reconsider the shipped default of `-j0`

`configure_thread_pool` (`crates/rue-compiler/src/lib.rs:239`) maps the default
`jobs = 0` to `available_parallelism()`. On every shape measured here that
default is the slowest or joint-slowest configuration available, and it is also
by far the least predictable: one-worker MAD is 4-8% and every parallel row is
15-40%.

- **Reachable saving.** The auto default resolves to four workers here, so this
  is the `-j4` row read backwards: Mosaic 1.83x on medians and 1.30x on minima,
  chain256 1.84x / 1.54x, wide512 1.50x / 1.20x, Lattice 1.01x paired. It is
  the only entry here that improves anything today, and it does so by not
  opting users into the regression rather than by fixing it.
- **Deterministic artifacts.** None. Executables are already byte-identical
  across worker counts; the published one-worker structural probe becomes the
  default path rather than a special one.
- **Risk.** It is a user-visible behaviour change and it forfeits the real
  1.85x and 2.52x that CFG and backend do deliver, so on a host with more cores
  and a program whose profile is backend-heavy the sign could differ. It should
  not be decided from one four-core host.
- **Authority: a maintainer policy decision.** It is also the item most worth
  re-measuring on the eight- and sixteen-core CI hardware before deciding,
  since this note cannot honestly speak above four workers.

### Not proposed

**Overlapping phases.** The phase taxonomy's own rule is that distinct
published phases must not be active at once — "if two different markers can be
active at once, redraw the boundary rather than accepting the mixed band" — and
the measurement confirms none ever were: `mixed_parallel_ns` is exactly zero in
all 318 samples. Parsing therefore never overlaps semantic analysis and
semantic never overlaps CFG. That is a genuine structural ceiling and it is
worth roughly the serial fraction — 13.7% on Lattice, 31.1% on wide512. It is
not proposed because the phase partition is a published contract that the
additive accounting model and the scaling suite both depend on, and because it
is second in line behind a phase that currently scales negatively. Fixing
semantic analysis first is strictly better ordered.

**Parallel parsing.** Source discovery and parsing measures 1.00x at every
worker count on every shape and is 19-22% of the two module-heavy generated
shapes. It is the largest remaining serial band after the above. It is not
proposed here because it is a separate investigation with its own ordering
question (RUE-1571 has recently been moving parse staging), and because on the
maintained programs it is 2.5% of the build.

## Recommendation

**Stop at this note; implement nothing yet.** The measured cause is a soundness
property of the query runtime's proof scope, and the only change that reaches
it (design 1) is an ADR-level decision about whether one task's lease may
justify another task's skipped validation. The two bounded changes available
(designs 2 and 3) are, respectively, worth nothing on the maintained programs
and a policy call that should not be made from a single four-core host.

What this note establishes for whoever picks that up:

1. The serial fraction is 13.7% on Lattice and is **not** the bottleneck.
   Semantic analysis scaling at 0.46x is. It is the single lever that matters:
   holding semantic where it is and making CFG scale perfectly moves Lattice
   from 0.99x only to about 1.04x, because at four workers the regressed
   semantic band is already 61% of the predicted root.
2. The cause is identified, located in the source at two named sites, and
   measured host-independently — 8.6x validation traversals on identical query
   claims, with a control shape proving the cost is paid for configuring
   concurrency rather than for using it.
3. Byte identity across worker counts holds and is not at risk from any
   candidate here, since none of them changes what is computed.
4. Re-measure on hardware that can honestly run eight and sixteen workers
   before deciding design 3. Every number above is bounded by four cores.
