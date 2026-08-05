# RUE-1223 contention-free instrumentation acceptance ledger

This note records the local performance and correctness evidence for replacing
the timing collector's per-transition shared lock with deterministic
thread-local accumulation. It is a host-specific acceptance record, not a
portable latency guarantee.

## Measurement context

Measured 2026-08-04 and 2026-08-05 on an Apple M5 MacBook Pro (`Mac17,2`, 10
logical CPUs), macOS 26.5.2 / Darwin 25.5.0 ARM64, targeting
`aarch64-macos`. The baseline is upstream `trunk` at `22b94122`; both compilers
were built with Buck's `//platforms:release` configuration. Each invocation
used the same compiler binary, source closure, standard library, output path
shape, and default query-worker count. Instrumentation-off and
`--benchmark-json` invocations alternated within each pair and were timed with
`/usr/bin/time -p`.

The paired overhead ratio `(on - off) / off` is the primary comparison. Pairing
keeps each instrumented observation adjacent to its control on this otherwise
noisy interactive host. Marginal medians are included so the raw variance is
visible.

## Instrumentation overhead

| Workload | Pairs | Off median | On median | Ratio of marginal medians | Median paired overhead |
| --- | ---: | ---: | ---: | ---: | ---: |
| Meridian | 5 | 6.80 s | 6.87 s | +1.03% | **+1.65%** |
| Caldera | 11 | 19.34 s | 19.85 s | +2.64% | **+0.87%** |

Meridian off/on samples in pair order were `6.84/6.87`, `6.85/8.17`,
`6.80/7.07`, `6.67/6.78`, and `6.75/6.80` seconds. Caldera off/on samples were
`21.43/20.21`, `19.24/18.84`, `20.60/20.78`, `19.34/19.85`, `19.44/19.27`,
`19.83/20.05`, `18.79/18.70`, `18.42/19.09`, `20.00/20.02`, `19.09/19.49`,
and `18.97/21.38` seconds. Caldera's ratio of marginal medians is above 2%
because the arms drift and include opposite-direction outliers; the median of
the 11 adjacent pair effects is 0.87%.

The startup probe was measured for 21 alternating pairs. Both arms had a 0.01
second median at `time -p`'s centisecond resolution. One off sample was 0.05
seconds and one on sample was 0.02 seconds, confirming that this host and timer
cannot resolve a useful startup effect without batching.

The final reviewed source was rebuilt in release mode and smoke-measured after
the repeated run. The review follow-up changed only final report reduction and
tests, not event collection or worker publication. Final Meridian measured
7.10 seconds off and 6.78 seconds on; final Caldera measured 19.53 seconds off
and 22.34 seconds on. These single observations are correctness smokes, not an
additional overhead estimate.

## Exact accounting and output parity

Final instrumented release compiles satisfied the integer-nanosecond invariant:

| Workload | Compiler root | Attributed phases | Mixed parallel | Unattributed | Exact sum | Unattributed before / after |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Meridian | 6,644,317,168 | 4,974,499,944 | 871,217,154 | 798,600,070 | 6,644,317,168 | 39.0% / 12.0% |
| Caldera | 21,788,942,458 | 15,366,671,898 | 1,561,343,968 | 4,860,926,592 | 21,788,942,458 | 53.1% / 22.3% |

Instrumentation-off and instrumentation-on output hashes were identical:

- Meridian: `3ec7580afe63c8a98201ace15870ce6bb59fa79bf57ab4475f74aac5f6c52ef3`
- Caldera: `3d42a42c6ad17105c600cdf8f0cdeec6990c077bcefaf4c207a62720cff5fca7`

The executable regression suite additionally covers deterministic merge under
equal timestamps, exact root/phase cross-check corruption detection, real
registered-batch publication from worker and inline caller threads, and
cooperative cancellation across every worker completion boundary. The query
runtime's existing unwind tests cover the same completion path for panic.
