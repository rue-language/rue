# Daemon client performance regime

This note records the RUE-2130 measurement boundary. It is separate from
`fresh_source_to_native_v1`: a fresh record must continue to describe one
compiler process with no retained daemon state. The daemon regime measures the
client operation through the opt-in local service and keeps the default daemon
mode unchanged.

The checked-in record kind is
`daemon_client_to_publication_v1`. The Rust types and validator in
[`rue-perf-schema/src/daemon.rs`](../../crates/rue-perf-schema/src/daemon.rs)
are the only validity authority. Validate a report or one endpoint sidecar
with:

```text
rue-bench daemon-performance validate --input report.json
```

The external runner starts its clock before each compiler client and stops it
after that client exits. Executable rows use the bytes of the actually
published output; analysis rows use the caller-visible presentation. Test rows
carry the full client invocation time and separately record compiler test-image
preparation, real test execution, and a digest of the emitted test events.
Image preparation is not execution evidence. The daemon server reports the
linked image and request-owned executor measurement; publication, signing,
and test execution remain client operations.

Test preparation starts at the same post-argument-parsing invocation boundary
in both modes and ends when the shared test runner has the published image.
It includes observation, compilation, transfer, and signing where applicable.
Transfer time starts with the first response byte and ends after frame decoding
and image chunks; waiting for admission, queueing, and compilation is excluded.
These phase clocks are nested observations, not additive components of the
external client duration.

Each endpoint records the actual compiler image hash, version, protocol,
daemon process identity, successful session generation, accepted-input hash,
target, requested and resolved compiler workers, optimization, preview features, and test-job
policy. The service reply carries the ticket-correlated session/reuse,
query-counter, source, retained-charge, dependency-pin, and executor facts.
Missing phase or transport measurements remain unavailable rather than being
filled with synthetic zeroes. Report validation requires matching direct and
daemon output, diagnostics, and exit status; false parity is invalid.

The external runner uses private read-only copies of the compiler and validator,
hashes them before and after the run, and fills the compiler image SHA in the
paired endpoints. Raw sidecars leave that external field, the external clock,
and captured output/diagnostic fields unavailable. This avoids charging a full
compiler-image hash to every client. Test image bytes are hashed once at the
shared runner boundary, only when performance capture is requested. Test rows
compare both image hashes and the Rust-owned event projection, while preserving
the raw event stream as execution proof.

The runner pairs `--daemon=off` fresh calls with `--daemon=required` calls and
can exercise first-client, prepared, analysis, test, build, edit, error,
repair, revert, eviction, restart, and contention scenarios. A partial report
must declare its coverage and cannot claim complete qualification. A complete
qualification requires all fourteen scenarios and real test execution.

The validator checks the transitions as well as their names: prepared requests
retain the same accepted inputs/session and show query reuse; both mixed
analysis/test/build orders reuse shared computations; repeated eviction recreates
a session for identical inputs; restart changes the daemon generation; and
contention carries overlapping client intervals plus an observed queued request.
Unexpected compiler failures cannot qualify merely because both paths failed.

Run a complete release qualification from a clean source checkout:

```text
python3 scripts/bench-daemon-performance.py --compiler /absolute/rue \
  --validator /absolute/rue-bench --repository /absolute/rue-checkout \
  --revision <source-commit> --output /private/tmp/rue-dp-run
```

The output directory must be empty. Use a short path for the private Unix
socket. The runner preserves raw sidecars, stdout/stderr, process intervals,
fixture inventories, global resource observations, and sampled RSS alongside
the validated paired report and a descriptive summary. RSS includes allocator
high-water memory and is separate from retained query charge; sampling begins
after the first response identifies the daemon PID, so it does not claim a
cold-start RSS peak. The first daemon client starts its service inside the
measured interval, after its fresh reference may have warmed the OS file cache.

The sidecar option requires a new file and never replaces an existing source,
symlink, report, or executable. This regime currently excludes watch, explicit
module manifests, test candidates, link archives, system linkers, test listings,
other emit stages, and simultaneous fresh-benchmark/timing output. Their own
compiler behavior remains available without the measurement option.

The explicit `--daemon=off` pins in Rue's rules, corpus, oracle, frontend-diff,
benchmark, and shared test-runner callers keep hermetic and fresh-process
measurements independent of ambient developer services. The ordinary CLI
default remains direct until a separately reviewed release calibration decides
whether automatic startup is justified. This note does not make that rollout
decision and does not claim a latency or RSS result.
