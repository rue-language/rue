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

The runner executes three samples sequentially for each workload. JSON keeps
every raw observation in integer nanoseconds. The Markdown view reports the
median and median absolute deviation (MAD), along with the machine and hosted
runner fingerprint. It records:

- files, modules, source bytes, lines, lexer tokens, and functions considered;
- externally observed fresh-process wall time and peak resident memory;
- the compiler's additive, mutually exclusive phase accounting;
- output binary size.

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
