# Boundary evidence and the size of performance-data-v1

Supporting evidence for ADR-0067 Amendment 1 and ADR-0071 Amendment 1
(RUE-1543). Those amendments carry the recommendations; this note carries the
measurements they rest on and the reasoning that did not fit in them.

`performance-data-v1` grows by roughly 300 MB a day. Protocol v2 (suite
revision 3, `boundary = "fresh_source_to_native_v1"`, ADR-0071 Decision 2)
began retaining per-sample build-boundary evidence on 2026-08-13, and that
evidence is now 98.6–99.3% of a collected record.

Two findings shape everything below.

**The evidence is mostly duplicate.** Within one workload of one run, every
part of every evidence entry is bit-identical across all N processes except
`critical_path`, and `critical_path` is read by nothing that consumes the
branch. Hoisting the invariant part to the level it is actually invariant at
and committing to each process with a pair of digests — one for
`{runner, compiler}`, one for `compiler_work` — gives a record 4.0% of today's
size while preserving every guarantee a reader can re-derive from the branch,
under any worker setting.

**The cost is a checkout cost, not a storage cost.** The whole branch — 402
commits, 1,188 records — fetches in 53.69 MiB. It expands to 1,482.9 MiB on
disk. Rewriting history can reclaim at most the former; changing the tip tree
reclaims the latter, and needs no rewrite.

Everything labelled *measured* was computed from
`upstream/performance-data-v1` at `53f23a9aa` (2026-08-16), from the Actions
API, or from the repository source at `dd4e3ce30`. Projections are labelled.

## Re-measured at rebase (2026-08-23)

The 2026-08-16 corpus above remains the analyzed corpus; this section only
records that its trajectory held for the following week, measured at
`44b5d5164` (552 commits):

| Path | Records | Bytes |
| --- | ---: | ---: |
| `runs/` | 1,619 | 3,470.7 MiB |
| `runtime/` | 437 | 4.6 MiB |
| `index.json` | 1 | 0.5 MiB |

That is +1,989.5 MiB across +431 records in seven days — **284 MiB/day**,
matching the measured 288.6 MiB/day within 2%. Nothing on trunk changed what
is collected (RUE-1590 made the stall gate ignore *empty* records; it does
not alter evidence retention), and all nine declared baselines still resolve
(`rue-bench check-baselines`: 9/9, exit 0, against the live `index.json`).
The compacted-size projections below scale roughly linearly with record
count; the *shares* (evidence at 98.6–99.3% of a record, the per-entry
breakdown, the cross-process identity finding) are properties of the
encoding, not the corpus size.

## What is on the branch

Measured, from `git ls-tree -r -l`:

| Path | Records | Bytes |
| --- | ---: | ---: |
| `runs/` | 1,188 | 1,481.2 MiB |
| `runtime/` | 142 | 1.4 MiB |
| `index.json` | 1 | 0.4 MiB |

57,387 samples across 1,910 workload observations, and 105,489
boundary-evidence entries across the 1,860 observations that carry any.

| Platform / epoch | Records | Median record | Total |
| --- | ---: | ---: | ---: |
| `aarch64-linux` e2 | 291 | 2.0 KiB | 0.6 MiB |
| `aarch64-linux` e5 | 83 | 444.0 KiB | 34.1 MiB |
| `aarch64-linux` e6 | 23 | 1,062.4 KiB | 23.9 MiB |
| `aarch64-macos` e2 | 287 | 38.9 KiB | 10.9 MiB |
| `aarch64-macos` e5 | 82 | 12,393.5 KiB | 945.7 MiB |
| `aarch64-macos` e6 | 23 | 15,933.7 KiB | 357.9 MiB |
| `x86_64-linux` e2 | 290 | 4.4 KiB | 1.2 MiB |
| `x86_64-linux` e5 | 83 | 914.3 KiB | 70.0 MiB |
| `x86_64-linux` e6 | 23 | 1,639.5 KiB | 36.8 MiB |

`aarch64-macos` is 88.7% of the branch. The cause is its calibrated sampling
policy — epoch 6 declares `samples = 99, batch_size = 8` for `startup` against
12 × 6 on `x86_64-linux` — so a macOS record carries 951 evidence entries
against 99. Record size is linear in Σ(samples × batch_size). The 99-sample
policy answers that platform's ~6.4% relative MAD and is not the thing to
change; it is the multiplier that turns a 14 KiB per-entry cost into a 16 MB
record.

By epoch: epoch 2 is 12.7 MiB (0.9%), epoch 4 is 0.1 MiB, epoch 5 is
1,049.8 MiB (70.9%), epoch 6 is 418.6 MiB (28.3%). Epoch 5 is retired
(`collection = false`, superseded by epoch 6) and is more than two thirds of
the store.

## The git-level reality

Measured by fetching the branch alone into an empty bare repository:

| Quantity | Measured |
| --- | ---: |
| pack for the full branch history (402 commits, 1,188 records) | **53.69 MiB** |
| time to fetch it (local, warm) | 16.1s |
| share of that pack that is `runs/` blobs | 99.0% |
| `git repack -adq --window=250 --depth=100` on it | no change |
| checkout of the tip tree | **1,482.9 MiB** |
| whole `rue-language/rue` repository, as GitHub reports it | 115.8 MiB |

The branch compresses about 28× because a 16 MB macOS record is 792 identical
copies of a 5.4 KiB structure plus several thousand mostly-zero 64-element
arrays. This is why a full fetch took 0.6s in CI and why the repository is not
large by any hosting measure.

The consequence is the central fact of the compaction question. Fetching is
already cheap and history rewriting can only make fetching cheaper. What is
expensive is materializing and parsing the **tip tree**, and the tip tree is
determined by the newest commit alone — which an ordinary append changes.

Measured consistently, each candidate state built as a single commit and
repacked with identical aggressive settings. Every re-encoded row carries one
digest per process, as serialized; the split digest adds 6.7 MiB to the
checkout column (S4: 54.3 → 61.0 MiB) and at most the same, uncompressed, to
the pack column:

| Branch state | Checkout | Pack |
| --- | ---: | ---: |
| tip as published today | 1,482.9 MiB | 40.2 MiB |
| every record re-encoded, S4 | 54.3 MiB | 5.0 MiB |
| every record re-encoded, S1 | 46.5 MiB | 4.2 MiB |
| retired epochs re-encoded, epoch 6 as published | 456.0 MiB | 16.7 MiB |
| epoch 6 only, as published | 420.0 MiB | 11.7 MiB |

Per epoch, checkout bytes:

| Epoch | Published | S4 | S1 |
| --- | ---: | ---: | ---: |
| 2 | 12.7 MiB | 12.7 MiB | 12.7 MiB |
| 4 | 0.1 MiB | 0.1 MiB | 0.1 MiB |
| 5 | 1,049.8 MiB | 22.9 MiB | 19.9 MiB |
| 6 | 418.6 MiB | 16.9 MiB | 12.2 MiB |
| **all** | **1,481.2 MiB** | **52.6 MiB** | **44.9 MiB** |

Epochs 2 and 4 are protocol v1 and carry no evidence, so re-encoding leaves
them byte-identical.

## What it costs today

Measured from the Actions API on 2026-08-16.

`performance staleness (linux-x64)` ran 360–400s across four sampled trunk
runs, against 103–136s for `premerge (linux-x64)` and 177–194s for
`compiler reproducibility (linux-x64)`. Its growth tracked the branch: 209–244s
on 08-14, 302–357s on 08-15, 360–383s on 08-16. The step breakdown of run
31951278581 (job 95175000497): `rue-bench` build 4.6s, both fetches 0.9s,
materialize plus `rue-bench derive` **349.8s**, stall check 1.5s.

RUE-1542 (`dd4e3ce30`) has since narrowed that job to the live epoch — 1.5 GB
to 419 MB, local derive 197s to 57s. It fixes the *history*, not the *trend*:
its own commit message says so, and the live epoch grows at the same ~300 MB a
day. Four days of collection restores the job to where it was.

Two consumers were not narrowed and still read the whole branch:

- `website/build.sh` is unchanged by RUE-1542. It runs
  `git --work-tree="$PERF_DATA_ROOT" checkout origin/performance-data-v1 -- .`
  and one `rue-bench derive` over everything. Its `Build website` step measured
  425s on deploy run 31937499410, and the workflow triggers on every completed
  `Performance collection` run. It cannot be narrowed the way the gate was: the
  dashboard renders every epoch, so every record it plots must be read.
- The publish job in `performance-collect.yml` does
  `git worktree add ../data origin/performance-data-v1` then `git add -A` over
  the whole tip tree, on every collection.

Parse cost is linear in bytes, measured over the real corpora:

| Corpus | Bytes | Read + parse | Read + parse + canonicalize |
| --- | ---: | ---: | ---: |
| as published | 1,481.2 MiB | 18.48s | 35.74s |
| S4 re-encoded | 52.6 MiB | 0.34s | 0.79s |

`StoredRun::read` is more expensive than either column: it parses to a
`serde_json::Value`, canonicalizes and SHA-256s that value, then deserializes
the same value again into the typed struct.

## What is in `boundary_evidence`

`Sample::boundary_evidence` is a `Vec<BuildBoundaryEvidence>` with exactly
`batch_size` entries, one per fresh compiler process (`run.rs:238`). Each entry
has four members (`boundary.rs:602`). Median sizes over the 99 entries of the
median `x86_64-linux` epoch-6 record; the macOS record's 951 entries agree to
within 0.1%:

| Member | Median | Share | What it is |
| --- | ---: | ---: | --- |
| `critical_path` | 8,607 B | 61% | 10 scalar counters and 38 `DurationDistribution`s |
| `compiler_work` | 3,733 B | 27% | deterministic work counters, 8 sub-structs |
| `compiler` | 1,142 B | 8% | the compiler's own account of the boundary |
| `runner` | 464 B | 3% | the runner's independent process facts |
| **entry total** | **14,003 B** | | |

Inside `critical_path`: 10 scalars are 220 B, 38 distributions are 8,338 B, and
5,510 B of that — 64% of `critical_path`, 39% of the entry — is `log2_buckets`.
Every distribution carries a fixed 64-element array; 37 of 38 are populated,
and the median populated one has 7 non-zero buckets on Linux, 9 on macOS.

Inside `compiler`, `accepted_inputs` is one `{class, logical_identity, sha256}`
per source module, so its size is the workload's module count: 1.12 KiB for
`startup`, 6.08 KiB for `scale_instantiations_*`, 24.44 KiB for `lattice`,
34.87 KiB for `scale_modules_256`. A typical entry is 14.0 KiB rather than the
37.9 KiB a `lattice` entry costs, because `startup` is 72 of the 99 entries in
the median `x86_64-linux` epoch-6 record and 792 of the 951 in the macOS one.

### The redundancy

For every workload of every record examined — `x86_64-linux` e5 and e6,
`aarch64-macos` e5 and e6, `aarch64-linux` e6 — the number of *distinct* values
across all evidence entries of a workload is:

| Member | Distinct values per workload |
| --- | ---: |
| `runner` | 1 |
| `compiler`, including `accepted_inputs` | 1 |
| `compiler_work` | 1 |
| `critical_path` | N, one per process |

For macOS `startup` that is 792 byte-identical copies of `runner`, `compiler`,
and `compiler_work`. Run-wide, `runner` minus its output fields and `compiler`
minus `accepted_inputs` and its output fields are also single-valued across
every workload of the record.

This is by construction. The output digest and, at `worker_setting = "one"`,
`compiler_work` are *required* equal by `check_boundary_evidence`; the
configuration and source closure are fixed by the epoch.

### Who reads it

The complete inventory, from `grep` over `crates/`, `scripts/`, `website/`, and
`.github/` at `dd4e3ce30`:

1. **`check_boundary_evidence`** (`validate.rs:524`), reached from
   `validate_run`, which `derive` calls for every stored record. The only
   consumer of stored evidence.
2. **`measure.rs:373`**, calling `validate_current_producer_against` — the
   26-row `REQUIRED_SEMANTIC_EVIDENCE` table, the RUE-1510 deletion proof, and
   the body-lowering, provider, precompute, expression and CFG attribution
   partitions. Runs **in the producing process, before the sample is assembled
   into a run object**. Never applied to a stored record.
3. **`measure.rs:520` `verify_input_provenance`**, re-hashing every
   `accepted_inputs` entry against the real file on disk. Producer-side only.
4. **`crates/rue-bench/src/scaling.rs`**, reading `boundary_evidence.first()`
   and `evidence.critical_path`. `rue-bench scaling` measures fresh processes
   and writes a `ScalingReport` to a workflow artifact. **It never reads
   `performance-data-v1`.** `docs/notes/adr-0071-phase-1-reference-baseline.md`
   is written from that report, not from the branch.

Nothing in `derive`, `SiteData`, `website/static/performance.js`, or the
templates touches boundary evidence. `log2_buckets` has no reader outside
`DurationDistribution::validate`, which checks the buckets sum to the `count`
beside them in the same object.

## What the evidence proves

ADR-0071 Decision 2 requires runner, manifest, and compiler to agree before a
sample is admissible. ADR-0067 §8 describes what a run object holds and does not
mention boundary evidence; it entered through ADR-0071.

**Re-derivable from the branch by any reader** (`check_boundary_evidence`):

- G1 Each process ran with a fresh state and output directory, no daemon, no
  retained-session handle, the ADR's clock boundary, a successful exit, and a
  verified native output. `runner`, 464 B.
- G2 The compiler's boundary, pipeline, session and root-request counts,
  configuration, completed stages and artifact hits agree with the epoch's
  policy and target, and no preview feature or external link archive entered.
- G3 Runner and compiler name the same output bytes and size, agreeing with the
  sample's `output_binary_bytes`.
- G4 Every accepted input is in an allowed class, well-formed, canonically
  sorted and deduplicated, and at least one is workload source.
- G5 Embedded assets match the policy's classes, count and target.
- G6 Every duration distribution is internally consistent, and five specific
  counters are non-zero.
- G7 All processes of a workload produced the same output digest.
- G8 At one worker, all processes of a workload reported identical
  `compiler_work`.

**Producer-side only, never re-checked against a stored record:** all of
`validate_current_producer_against` and `verify_input_provenance`. These are the
checks that need the full `critical_path` and the per-file input digests, and
they have already run in the process that produced the numbers.

**Read by nothing:** the shape of every `log2_buckets`; every `critical_path`
beyond G6; processes 2..N of `runner`, `compiler` and `compiler_work`.

G6 compares a histogram's bucket sum against the `count` beside it in the same
object. It is a check of a number against itself: it can catch a corrupt
serializer and nothing about the compiler. Its only content about the measured
process is "these five counters are non-zero".

G7 and G8 are real, and are why all N entries exist. But the N copies are not N
independent witnesses — they are one runner process reporting what it observed
N times, and that runner already refuses to emit a sample whose processes
disagree (`measure.rs:145`). Storing the copies lets a third party re-derive the
agreement; it does not add an authority.

## Encodings, measured

Each encoding was applied to all 1,188 records and serialized as canonical
JSON. Sizes come from exact byte arithmetic on canonical JSON — key order does
not change an object's length — cross-checked against full re-serialization.

| Encoding | Branch total | Of baseline | 08-16 growth |
| --- | ---: | ---: | ---: |
| baseline (as stored) | 1,481.2 MiB | 100.0% | 288.6 MiB |
| A — whole array → one digest per sample | 24.9 MiB | 1.7% | 2.5 MiB |
| B — keep process 0 of each sample | 584.3 MiB | 39.4% | 123.5 MiB |
| C — one full entry per workload | 46.3 MiB | 3.1% | 10.6 MiB |
| E — drop `critical_path` | 775.1 MiB | 52.3% | 142.8 MiB |
| F — drop `log2_buckets` only | 1,033.7 MiB | 69.8% | 195.5 MiB |
| G — sparse `log2_buckets` | 1,129.2 MiB | 76.2% | 215.8 MiB |
| H — `accepted_inputs` → digest | 1,215.9 MiB | 82.1% | 238.6 MiB |
| **S1 — witness + per-process digests** | **44.9 MiB** | **3.0%** | **8.2 MiB** |
| S2 — S1, `accepted_inputs` → digest | 34.2 MiB | 2.3% | 4.8 MiB |
| S3 — S1, process count instead of digests | 38.2 MiB | 2.6% | 7.1 MiB |
| **S4 — S1 + one `critical_path` per workload** | **52.6 MiB** | **3.6%** | **11.4 MiB** |
| S5 — no evidence at all (the floor) | 27.6 MiB | 1.9% | 3.7 MiB |

Median record, KiB:

| Platform / epoch | baseline | A | B | C | E | S1 | S2 | S4 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `aarch64-linux` e5 | 444.0 | 4.8 | 165.5 | 50.4 | 221.7 | 37.5 | 14.1 | 51.3 |
| `aarch64-linux` e6 | 1,062.4 | 16.9 | 720.6 | 203.3 | 574.0 | 128.5 | 52.8 | 198.2 |
| `aarch64-macos` e5 | 12,393.5 | 95.2 | 4,673.0 | 124.8 | 6,379.2 | 172.2 | 148.8 | 186.1 |
| `aarch64-macos` e6 | 15,933.7 | 124.2 | 6,459.5 | 291.7 | 7,852.8 | 280.1 | 204.4 | 350.2 |
| `x86_64-linux` e5 | 914.3 | 8.0 | 246.3 | 53.1 | 411.3 | 43.0 | 19.7 | 56.9 |
| `x86_64-linux` e6 | 1,639.5 | 20.2 | 819.2 | 206.0 | 797.4 | 134.1 | 58.4 | 203.8 |

**F and G are not worth doing.** Deleting or sparsifying `log2_buckets` — the
most obviously wasteful thing in the record — saves 30% and 24%. Sparse
encoding is worse than deletion because `[[9,41],[10,87]]` costs more than the
zeros it replaces once most buckets are dropped anyway. Neither touches the
duplication.

**H is not worth doing.** `accepted_inputs` looks large because a `lattice`
entry is 24 KiB of it; weighted across the branch it is 18% of the bytes.

**B is the wrong axis.** One process per sample halves the branch, and the half
it keeps is still 60× larger than it needs to be.

**A is the cheapest and the weakest.** A digest of discarded data proves the
producer had bytes with that hash and nothing else; it converts G1–G8 from
re-derivable facts into producer assertions. It is worth something only if the
bytes are retained somewhere — which is a real option, and a different one.

**S1 and S4 exploit the measured redundancy rather than discarding
information.** The shape:

```
run object
  boundary:                          # run-level, one per record
    runner:   { the 8 process facts, minus output identity }
    compiler: { boundary, pipeline, session_count, root_request_count,
                configuration, completed_stages, artifact_hits,
                embedded_assets }
  workloads[i]
    boundary:                        # workload-level, one per observation
      accepted_inputs: [...]
      runner.output_sha256, runner.output_size_bytes,
      compiler.emitted_output_sha256, compiler.emitted_output_size_bytes
      compiler_work: {...}           # witness at one worker; sample otherwise
      critical_path: {...}           # S4 only
      critical_path_source: { sample_index: 0, process_index: 0 }
    samples[j]
      boundary_processes:      [ "<sha256>", ... ]   # {runner, compiler}
      boundary_work_processes: [ "<sha256>", ... ]   # one worker only
```

Each process contributes **two** digests: one over its `{runner, compiler}` and
one over its `compiler_work`. A reader re-derives G7 and G8 exactly as today —
all N digests of a kind must be equal and must equal the digest of the
corresponding stored witness.

The pair is not decoration. `check_boundary_evidence` enforces output identity
unconditionally for protocol-2 records (`validate.rs:569-580`) but enforces
work identity only when `policy.worker_setting == WorkerSetting::One`
(`validate.rs:588-600`), because parallel rows deliberately carry
schedule-dependent joins and reuses. Every boundary epoch in the manifest is
`worker_setting = "one"` today, which is precisely why this note measures one
distinct `compiler_work` per workload — but ADR-0071 Decision 7 requires the
report across `WorkerSetting::REFERENCE_MATRIX`, and on such an epoch a single
combined digest would differ for every process. The workload witness could not
hold it, and output identity — the guarantee that is *not* gated — would stop
being re-derivable, because a mismatch could no longer be attributed to the
binary rather than the schedule. Under the split, only
`boundary_work_processes` is permitted to vary, and it stays comparable.

#### The parallel case carries no work digest

The paragraph above originally said a parallel boundary epoch keeps the
per-process work digests without a workload-level `compiler_work` witness.
That is not a guarantee, and the correction is Steve's on the pull request: with
no stored preimage and no requirement that the values agree, a reader holding
those digests can observe only that they differ, which is what
`check_boundary_evidence` already expects there. A digest of discarded bytes
that is *supposed* to differ certifies nothing — the same objection this note
makes against encoding A, reached from the other side.

So the encoding is stated per worker setting rather than uniformly:

| | `worker_setting = "one"` | parallel (`two`/`four`/`eight`/`automatic`) |
| --- | --- | --- |
| workload-level `compiler_work` | witness; every process must equal it | one representative sample, from a named process |
| `boundary_processes` (`{runner, compiler}`) | present; all equal the witness | present; all equal the witness |
| `boundary_work_processes` | present; all equal the witness | **absent** |
| full per-process `compiler_work` | workflow artifact | workflow artifact |

`boundary_processes` is unconditional, which is the point of splitting the
digest: output identity is enforced for every protocol-2 record, so it stays
re-derivable under any worker setting. `boundary_work_processes` exists only
where there is a witness to check it against.

A parallel epoch still keeps one `compiler_work` per workload observation, by
the same selection rule S4 uses for `critical_path`, and labelled the same way
— a sample from a named process, not a witness. That keeps a per-commit work
signal without claiming a cross-process guarantee the checker does not make.

This does not move the size projections. Every boundary epoch in
`performance/manifest.toml` is `worker_setting = "one"`, so every record in the
store today carries both digests and the 6.7 MiB figure stands as measured. A
future parallel epoch is strictly cheaper than these projections, not dearer.

#### A worked digest vector

The digest is `SHA-256(tag || canonical_json(value))`. Taking one
`RunnerBoundaryEvidence` value, canonical JSON is 464 bytes:

```
{"clock_boundary":"monotonic_pre_spawn_through_exit_and_output_verification","compiler_binary_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","daemon_endpoint_supplied":false,"fresh_output_directory":true,"fresh_state_directory":true,"native_output_verified":true,"output_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","output_size_bytes":16384,"retained_session_handle_supplied":false,"successful_exit":true}
```

With the identity tag `rue.boundary.identity.1\n`:

```
2a095434f674a8c7d4096f6c69d45273c1f811a8ac05bd590f82649170a8501e
```

Two contrasts make the domain separation checkable rather than asserted. The
same bytes with no tag digest to
`354de1ad26a990020fb8548f8e29a8b3e5618562fa91e9b256224beb68433675`, and under
the work tag `rue.boundary.work.1\n` to
`b8b977f34591df2d3d8700988c6f0dd0a0a7da1a65e9ac5818bcce75af821155` — three
different names for one value, which is the property the tag buys.

Reproduce any of them with:

```bash
printf 'rue.boundary.identity.1\n{"clock_boundary":...}' | sha256sum
```

The real preimage is the two-key object `{"compiler": …, "runner": …}` over the
reassembled per-process pair; this vector fixes the mechanism — tag,
canonicalization, hex — which is the part two implementations could otherwise
disagree about.
G1–G5 are checked once per record or workload instead of once per process, over
bytes that were identical anyway. G6 survives for one process per workload
under S4 and is dropped under S1.

The smallest representation preserving the load-bearing guarantees is **S1, at
3.0%**. S4 costs 0.6 points more and buys back G6 plus one per-commit critical
path per workload — the same projection `scaling.rs` takes when it wants one.

Both figures are for one digest per process. The second digest the split
requires costs the same as the first, and this table measures that directly:
S1 minus S3 — one digest per process, versus a process count in its place — is
**6.7 MiB** over 105,489 process entries, or 66.6 bytes each, a 64-hex string
with its quotes and separator. So the split takes S1 to 51.6 MiB (3.5%) and S4
to 59.3 MiB (4.0%), and daily growth to 9.3 and 12.5 MiB. Those four numbers
are arithmetic on a measured per-digest cost, not a fresh serialization; the
implementing change owes the re-measurement.

### What is lost

A reader can no longer re-derive per-process `critical_path` self-consistency
for processes 2..N, nor inspect per-process timing histograms for a stored
commit. Nothing today does either. The producer-side checks that consume
`critical_path` in full are unaffected: they run before storage.

## Re-encoding published records

Measured by generating the re-encoded corpus and comparing it record by record.

**Nothing a chart is drawn from changes.** Stripping the evidence keys
(`boundary_evidence`, `boundary`, `boundary_processes`) from both sides,
**0 of 1,188 records differ**. Identity, pins, environment, phase accounting,
`process_elapsed_ns`, `peak_memory_bytes`, `output_binary_bytes` and failure
records are byte-identical, so every median, dispersion, ratio, index, ratchet
and flag is unchanged by construction.

**Every address moves: 1,188 of 1,188.** `schema_version` is an ordinary field
of `RunObject` (`run.rs:457`) with no `skip_serializing_if`, so it is part of
the canonical form `content_address` digests. A record whose only change is
`1` → `2` therefore gets a new address, and Question 1a requires every
re-encoded record to declare 2. Under a retired-epochs-only re-encode, 1,119
move (epoch 2's 868, epoch 4's 3, epoch 5's 248).

The evidence-free records remain byte-identical *below the version field* —
epoch 2 (868) and epoch 4 (3) carry no evidence at all, as do six epoch-5
records with no evidence in any sample, so 877 records change in that one field
and nothing else. That distinction is why the equivalence result above holds,
but it does not reduce the breakage inventory, which turns only on whether an
address moved.

An earlier revision of this note reported 311 moved addresses and 242 under
retired-only. Those came from a prototype that left `schema_version` at 1,
which Question 1a does not permit; they are superseded.

### Exhaustive breakage inventory

Found by reading the source, not by reasoning about it.

| Site | What breaks | How it fails |
| --- | --- | --- |
| `performance/manifest.toml` — 9 `[epoch.baseline] run` pins | All 9 move under a full re-encode; 6 under retired-only (epochs 2 and 5). Six of the nine belong to retired epochs. | silent, see below |
| `performance/manifest.toml:845` `reference_run` | Must equal its epoch's baseline. | `manifest.rs:695` rejects the manifest at parse — **loud** |
| `derive.rs:1316` | Resolves the baseline by `stored.address() == baseline.run`. A miss yields `baseline_medians = None`. | **silent**: the epoch keeps plotting per-workload series and loses its headline index and every workload ratio |
| `scripts/validate-performance-stall.py` `unindexed()` | Catches exactly that failure — but iterates `newest_epochs()` only, over data `staleness-inputs` restricted to the live epoch. | catches a live epoch, **misses a retired one**; not fixable in the rule, see below |
| `rue-bench check-baselines` (added on this branch) | Every declared baseline must name a record of its own epoch and platform in `index.json`. | **loud**, exit 3, covers retired epochs |
| `index.json` | Must be rewritten to the new addresses. | rewritten on every commit anyway |
| `publish-performance-runs.py` | Refuses differing bytes under an existing name. | new addresses never collide; an *in-place* rewrite would trip it — **loud** |
| `stored.rs` `Stored::read` | The naming property: a record is named by the bytes it was published as. Re-encoding creates a second published record, it does not rename the first. | n/a; the regression test `a_schema_change_does_not_rename_an_existing_record` still holds |
| `website/static/performance-data.json` | Carries record addresses (`RejectedRun.run`, per-point `run`, `reference_run`). | **not tracked in git**; regenerated by `website/build.sh` on every build — self-healing |
| `docs/notes/adr-0071-phase-1-reference-baseline.md` (3 addresses), `adr-0071-phase-2-…md` (1) | Prose citations of epoch-5 run objects. | inert prose; needs a manual edit |
| everything else | — | nothing found: no committed derived data, no test pinning a record address, no external consumer in-tree |

The dangerous one is the combination of rows 1, 3 and 4: re-pinning a *retired*
epoch's baseline incorrectly loses that epoch's headline index from the
dashboard with **no gate firing**, and the remediation text in the gate that
would have caught it says "A record is named by the bytes it was published as;
nothing may rename it afterwards."

Declaring `schema_version = 2` makes that combination load-bearing rather than
hypothetical. Six of the nine baseline pins that must be re-pinned belong to
epochs 2 and 5, both `collection = false`, and `unindexed()` iterates
`newest_epochs()` — so every one of those six sits outside the only gate that
would report the mistake.

Extending `unindexed()` does not fix it, because the gate never holds the
records. `rue-bench staleness-inputs` selects the live epoch alone before
`derive` (RUE-1542), and selecting every epoch with a baseline would mean
reading 1,437 of the store's 1,440 records instead of 321 — the cost RUE-1542
removed. The resolution question needs no derived data: `index.json` carries
every record's platform, epoch and address, and the gate has already checked it
out to decide what to read. `rue-bench check-baselines` asks it there, for
every epoch, and is the prerequisite this note means. Measured against the
store on 2026-08-18: nine declared baselines, nine resolving.

### What immutability and content addressing actually buy

ADR-0067 §8 argues a stored record can be verified against its own name without
trusting whoever wrote it. Reading the write path for the actual threat model:

- The only writer is the `publish` job in `performance-collect.yml`, running
  `scripts/publish-performance-runs.py` with `permissions: contents: write` on
  the repository's own token. There is no external service, no separately
  managed credential, and no second writer.
- `performance-data-v1` is **not branch-protected** (`gh api …/protection`
  returns 404), so a force-push needs no settings change.
- The repository is public with 42 forks, so the claim that nobody has fetched
  the branch is not verifiable from here. Anyone who has cloned
  `rue-language/rue` has it, because a default clone fetches every branch.

So there is no untrusted writer. Anyone who can make the publisher write a
record can equally make it write a record with a correct address. What content
addressing does buy, concretely and in use today:

1. **Idempotent republication.** A re-run of a collection workflow produces
   byte-identical records; the publisher recognises them by name and skips
   them. Without it, workflow re-runs would double-count points.
2. **Accident refusal.** Differing bytes under an existing name are refused
   rather than clobbered.
3. **Protection against a silent schema rename** — the real one.
   `stored.rs` exists because record fields are additive: a new
   `#[serde(default)]` field parses into older records, and re-serializing one
   yields bytes, and therefore a name, the record never had. That would unname
   whichever record a baseline pins, and the failure is invisible. This is a
   guarantee against *our own future carelessness*, not against an attacker.
4. **A governance property.** A published measurement cannot be quietly edited
   later to make a chart look better.

A single reviewed re-encode spends (4) once, deliberately and in the open. It
does not touch (1), (2), or (3) — a re-encoded record is a *new* record with
its own correct address, and the original keeps its name and its bytes.

### The option space

Reclaim figures are measured; "loses" is what becomes unavailable to a reader
who has only the branch tip.

| # | Option | Checkout after | Pack after | Loses |
| --- | --- | ---: | ---: | --- |
| 0 | do nothing | 1,482.9 MiB, +289/day | 53.7 MiB | nothing; the trend continues |
| 1 | smaller encoding for new records only | 1,482.9 MiB, +11/day | +5 MiB/yr | nothing |
| 2 | re-encode every record at the tip, no history rewrite | **54.3 MiB** | 53.7 + ~5 MiB | per-process `critical_path` from the tip; still in branch history |
| 3 | re-encode retired epochs only | 456.0 MiB | 53.7 + ~17 MiB | same, for epochs 2–5 |
| 4 | delete retired-epoch records from the tip | 420.0 MiB | 53.7 MiB | epochs 2/4/5 disappear from the dashboard entirely |
| 5 | summarize a retired epoch into one record | ~420 MiB | 53.7 MiB | per-commit resolution; needs a new record kind and a dashboard path |
| 6 | archive the pre-compaction tip as a tag or branch | unchanged | +0 | nothing; the objects already exist |
| 7 | force-push a fresh orphan with the same records | 1,482.9 MiB | ~40 MiB *eventually* | history; reclaims almost nothing |
| 7b | force-push a fresh orphan with re-encoded records | 54.3 MiB | ~5 MiB *eventually* | all history, including the full evidence |
| 8 | git-level repacking, no content change | unchanged | **no change** | nothing |

Option 8 is measured and empty: the fetched pack is already 53.69 MiB and
`--window=250 --depth=100` does not improve it.

Options 7 and 7b carry a caveat that makes their pack column misleading. A
force-push does not delete anything on GitHub's side. Unreachable objects stay
in the repository's object store, remain fetchable by SHA, and the reported
repository size does not drop until GitHub runs its own maintenance, which is
not on request. In practice a force-push buys nothing measurable on the server
and destroys the history locally — the worst combination available.

Option 2 is the one that matters, and its shape deserves stating plainly: it is
an **ordinary append**. One commit adds 1,188 re-encoded records under their own
new addresses, removes the originals from the tip, and rewrites `index.json`.
No history is rewritten, no address is reused for different bytes, and every
original record stays reachable at `<pre-compaction-commit>:runs/<address>.json`
in a 53.69 MiB history. Combined with option 6 — tag that commit — the full
evidence has a name a reader can quote.

The pack column above measures each candidate as a standalone single commit, so
it understates the append: landing the 5.0 MiB S4 commit on top of the existing
402 takes the branch's fetch from 53.69 MiB to roughly 59 MiB, permanently —
under 66 MiB once the split digest's 6.7 MiB is counted uncompressed. The
checkout falls 24× and the fetch rises 10–23%; that trade is the whole
recommendation, and the rising half should be read off this paragraph rather
than reconstructed from the table.

### Re-deriving a chart in six months

Under options 1, 2, 3 and 6: unchanged. Every value a chart is drawn from is
byte-identical (0 of 1,188 records differ), so `rue-bench derive` over the tip
produces the same page. Recovering the full per-process evidence for a specific
run means checking out the archived tag and reading the original record.

Under option 4: impossible for epochs 2/4/5 without checking out an older
commit of the data branch and deriving from there — and the dashboard will not
show those stretches at all in the meantime.

Under option 7b: impossible, permanently, once GitHub's maintenance runs.

## Are the two decisions independent?

Only under one reading of ADR-0067 §3, and this is the question the store
amendment asks for a ruling on.

§3 assigns "runner protocol semantics (what a sample is, how batching is
defined, what a run object contains)" to the **suite revision**. Read literally,
changing the evidence representation is a suite revision — revision 5, and
therefore new epochs on all three platforms, and therefore a headline gap on
each until its baseline is pinned. It also makes compaction impossible: a
re-encoded epoch-5 record claims suite revision 3, which declares
`protocol_version = 2`, so it would be refused. Under that reading the two
decisions are **coupled**, and the legacy bytes cannot move at all without
redefining what epochs 5 and 6 mean.

But `RUN_SCHEMA_VERSION` is a second versioning axis, carried in the record and
checked by `validate_run` before anything else is trusted. It is the natural
axis for *how a run object is written* as opposed to *what was measured*. The
evidence that the distinction is real here is the equivalence result: 0 of 1,188
records differ outside the evidence keys, so the series' meaning demonstrably
does not change. Under that reading the encoding is a `schema_version` 1 → 2
change, needs no epoch turn and no headline gap, and legacy records can be
re-encoded under their existing epochs.

**An earlier version of this section concluded that the two decisions were
therefore independent, and either could be taken first or alone. That was
wrong**, and the correction came from review (Steve, on PR #2444).
`RUN_SCHEMA_VERSION` is not a decoding axis today. `validate_run`
(`validate.rs:401`) compares the record's version against the single current
constant and returns `UnsupportedSchemaVersion` without evaluating anything
else, and `lib.rs:139` states the intent: "there is no compatibility path, by
design." Bumping the constant does not add a version, it replaces the only one
readers accept.

Measured consequence of getting this wrong: with v2 records written while the
1,188 v1 records remain, `derive` routes all 1,188 to `rejected`, derives no
platform, and the dashboard empties — and `validate-performance-stall.py` reads
the empty `platforms` list as "no plotted points yet; nothing to stall" and
**exits 0**. The gate that exists to catch a stopped series cannot see a totally
rejected corpus.

So the encoding change requires dual v1/v2 decoding and validation, and an
amendment to the stated no-compatibility invariant, before either decision can
be taken. With that in place the two are independent in outcome and ordered in
execution; without it they are coupled and must be simultaneous, which is not
achievable across a repository merge and a data-branch push. ADR-0067
Amendment 1, Question 1a states the reader contract this implies.

The honest cost of the `schema_version` reading, separate from the above: within
one epoch, records written before and after the cutover carry different amounts
of re-checkable evidence, so a reader auditing an epoch's admissibility
retroactively gets different depth at different points. ADR-0067 Amendment 1
puts this to the maintainers.

## Reproducing

```bash
git fetch upstream performance-data-v1
mkdir -p /tmp/perf-branch
git archive upstream/performance-data-v1 | tar -x -C /tmp/perf-branch
```

`git archive` avoids the index entirely, so it does not disturb a checkout the
way `git --work-tree=… checkout … -- .` plus `git reset` does. Pack figures come
from fetching the branch alone into an empty bare repository; each candidate
state was built as a single commit and repacked with identical settings, so the
columns compare. The encoding prototypes assert their byte arithmetic against
full re-serialization on a real record before reporting any number.

CI figures come from `gh api repos/rue-language/rue/actions/runs/<id>/jobs` and
`gh run view --job <id> --log`; the decisive step breakdown is job
`95175000497` of run `31951278581`.
