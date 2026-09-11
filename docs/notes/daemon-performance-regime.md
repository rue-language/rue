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
default remains direct. The release calibration below does not support automatic
startup; explicit opt-in remains available.

## Release calibration and default recommendation

The 2026-09-11 qualification used clean source
`cdf1059285b63bf6712fa639a2e5a1c164b5e4ce` and a release ThinLTO compiler,
SHA-256 `76049650e3a7288a1d4a58e9906c6f002352886c96fc6da0779a5bec03b7bb29`.
The host was an Apple M5 with 10 logical CPUs and 24 GiB RAM, running
macOS 26.6.2 on AArch64. Optimization was O0, with the internal linker and
no preview features. Build and AIR requests used four compiler workers.
Tests used one test process and automatic compiler workers: the fresh process
resolved to 10 workers and the service's resource policy resolved to four.
This compares the configured product policies; it is not an equal-worker
compiler scaling experiment.

The [validated report](daemon-performance-qualification.json) contains all 34
pairs across the fourteen required scenarios. The
[evidence companion](daemon-performance-evidence.json) preserves compiler and
validator identities, fixture and standard-library hashes, every subprocess's
arguments, intervals and stream digests, resource snapshots, sampled RSS,
cleanup results, and the descriptive summary. The runner keeps raw streams,
sidecars and produced programs in its output directory. The checked-in report
can be validated with the command above; the companion is supporting evidence,
not a second performance-contract schema.

Every paired output, diagnostic and exit status matched. Produced executables
were actually run and their behavior compared. Each of the five test pairs
executed and reaped one test on each path, including prepared requests; image
reuse did not replace execution. Both AIR/test/build orders reused shared
queries in the same session. The second test request after an executable build
also passed; the query-runtime regression discovered during qualification is
covered by the permanent alternating-root and mixed-validation tests.
Body, API and import edits, an error, repair and revert all passed. Repeated
large-program requests evicted the retained host and created different session
generations for identical inputs. Restart changed daemon identity, concurrent
clients overlapped with an observed queued request, and both private service
lifetimes were stopped and reaped.

The following are external spawn-to-client-exit durations in milliseconds.
Rows with multiple observations show medians; all individual timings and ranges
remain in the report and companion.

| Scenario | Pairs | Fresh direct | Daemon |
| --- | ---: | ---: | ---: |
| First startup request, empty private endpoint | 1 | 12.4 | 185.9 |
| Prepared startup executable | 3 | 16.5 | 175.3 |
| Prepared Lattice executable | 3 | 719.7 | 1,044.2 |
| Lattice AIR in mixed workflow | 3 | 707.3 | 1,871.2 |
| Test after another artifact request | 2 | 27.1 | 173.3 |
| Prepared test request | 3 | 26.4 | 168.1 |
| Body edit in the two-module fixture | 1 | 17.6 | 172.4 |
| 4,096-function requests with host eviction | 2 | 3,355.3 | 11,751.0 |
| Concurrent Lattice clients | 2 | 855.4 | 2,111.4 |

The daemon was slower in every observed pair. Reuse was real: a prepared
Lattice request reported over 61,000 query reuses, and the prepared startup
executor took under one millisecond. The latter still took roughly 175 ms
at the external client boundary. A representative prepared Lattice reply
spent about 765 ms transferring and decoding its response. These clocks
locate costs outside reused compiler work but do not establish which socket,
serialization or scheduling mechanism caused them. Phase clocks are nested,
so subtracting or adding them does not produce a complete latency breakdown.

Peak observed retained query charge was 323,961,666 bytes (309.0 MiB), including
the protected request before the 256 MiB policy retired its host. The peak
response charge was 2,205,896 bytes (2.1 MiB). Sampled daemon RSS peaks were
750,496 KiB (732.9 MiB) and 363,040 KiB (354.5 MiB) in the two lifetimes.
These are daemon RSS observations, not a paired total-process memory comparison;
the runner did not sample fresh compiler RSS. Query charge, source bytes,
response charge and RSS overlap and must not be summed. Allocator high-water
memory may remain after logical eviction, and sampling does not cover the
first reply's cold-start peak.

**Keep automatic daemon use off.** Correctness, retained reuse and bounded
lifecycle behavior passed, but this calibration shows no client-latency benefit
and a persistent process with a substantial memory footprint. It therefore
provides no evidence for imposing automatic startup on ordinary users.
Future rollout work should profile transport and end-to-end overhead, then
repeat the complete qualification after any repair and on additional supported
hosts before changing the default. No persistence or incremental linking is
introduced by this measurement work.

These are descriptive observations from one host and one ordered run, with
three repeats for the prepared cases and fewer for transitions. Normal desktop
applications remained active; task-owned builds were stopped for the run.
The fixed fresh-then-daemon order and OS cache effects are not randomized,
and no confidence interval or cross-platform speedup is claimed. Those limits
reinforce retaining the current default; they do not turn the measured slowdown
into a prediction for every workload or machine.
