# Compiler scaling reports

The weekly compiler-scaling workflow measures how the compiler behaves as
maintained Rue programs grow from Ruelex through Mosaic and Harbor to Lattice.
It complements the fast compiler-performance headline; it does not contribute
to that index.

## Measurement regime

Each sample launches the canonical compiler binary in a new process and asks
it for `--benchmark-json`. There is no retained compiler session or persistent
query cache. Filesystem and operating-system page-cache state are not reset, so
the reports call this a **fresh-process compile**, not a cold compile.

The compiler and measurement runner are built with the release target platform
(`-Copt-level=3 -Clto=thin`). The compiler reports that profile in its benchmark
metadata, and the scaling runner rejects a binary that does not match the
manifest's declared profile.

The runner executes three timing samples sequentially for each workload. JSON
keeps every raw observation in integer nanoseconds. The Markdown view reports
the median and median absolute deviation (MAD), along with the machine and
hosted runner fingerprint. Two additional fresh-process probes run with one
compiler worker; their timings are discarded, and their deterministic
compiler-work counters must agree exactly. Fixing the structural probes to one
worker prevents legitimate parallel scheduling order from perturbing exact
counters while leaving the user-relevant timing samples parallel. A counter
disagreement rejects the workload instead of averaging structurally different
compilations. The report records:

- files, modules, source bytes, lines, lexer tokens, and functions considered;
- externally observed fresh-process wall time and peak resident memory;
- the compiler's additive, mutually exclusive phase accounting;
- semantic-reachability scans, scheduled keys, and fixed frontier-width
  buckets;
- request-local CFG-materialization index builds, declaration/anonymous/type
  scans, and exact body-local fact selections;
- query validation, endorsement, lease, demand, and retention-scan work;
- display-only query identities materialized for memo nodes, structured batch
  cycle rendering, and abort fallbacks, with exact counts and formatted key
  bytes;
- output binary size.

The Markdown work table includes validation nodes per token. That ratio is a
clock-independent amplification signal: source growth should not silently turn
into many repeated validation visits merely because elapsed time still looks
plausible on one host.

The semantic-reachability table reports average scheduled keys per non-empty
dependency-ready logical frontier, fixed width buckets, and body transactions
consumed from bounded ready-frontier prefetch windows versus demanded on the
fallback coordinator path. Multi-permit runtimes execute each window as a
structured batch; a single-permit runtime executes it inline so query-proof
state stays in the coordinator task. These counters
distinguish dependency graphs that expose little parallel work from wide
frontiers whose work remains slow because of allocation, hashing, query-runtime,
or synchronization overhead. They describe deterministic graph shape and
scheduler submissions; they are not elapsed-time or worker-utilization
estimates.

The CFG-materialization table separates immutable lookup preparation from the
exact fact-closure selections that remain body-local. Its selections-per-build
ratio makes accidental per-body rebuilding of request-wide declaration,
anonymous-nominal, destructor, and slice-source indexes directly visible.

The display-identity table similarly reports formatted key bytes per token.
Memo-node identities and abort fallbacks label diagnostics. Structured-wait
identities are materialized only when a detected wait cycle must be rendered;
registering an acyclic edge formats no key text. Typed query keys, not display
strings, remain authoritative for memo lookup. Family names are shared and
therefore excluded from the byte totals.

The compiled examples are never executed. Runtime tests and heavyweight
example scenarios remain in their existing suites, so a slow example runtime
cannot be mistaken for a compiler regression.

## Running locally

Build the compiler and the existing ADR-0067 runner, then run its scaling mode:

```bash
./buck2 build //crates/rue:rue //crates/rue-bench:rue-bench --target-platforms //platforms:release
RUE="$(scripts/rue-bin --target-platforms //platforms:release)"
BENCH="$(./buck2 build //crates/rue-bench:rue-bench --target-platforms //platforms:release --show-simple-output 2>/dev/null | tail -1)"
"$BENCH" scaling \
  --manifest performance/scaling.toml \
  --compiler "$RUE" \
  --commit <40-character-revision> \
  --repo-root . \
  --out /tmp/rue-scaling.json
```

This writes `/tmp/rue-scaling.json` and `/tmp/rue-scaling.md`. Supplying a
`--workdir` keeps the generated executables for inspection; without it the
runner uses a temporary directory.

## Comparing history

The scheduled workflow stores both report forms as a 90-day workflow artifact.
Compare reports only within the same target and manifest revision, and treat a
runner fingerprint change as advisory because GitHub-hosted hardware is not a
controlled machine.

Every workload row includes a shape id derived from its root path and the
compiler-produced file/module/function and source-size counts. A changed shape
id marks the timing comparison as advisory even if someone forgot to advance
the fixture revision. Deliberate changes to workload membership or intent must
increment `revision` in `performance/scaling.toml`; the old workflow artifact
remains the boundary record. Raw JSON is the authority, while the Markdown
table is a derived view that can be regenerated from those samples.

The suite is intentionally lower frequency. Do not add these workloads to
`performance/manifest.toml` or give them the startup probe's calibrated
sampling policy: doing so would make every trunk collection expensive and
silently change the headline's meaning.
