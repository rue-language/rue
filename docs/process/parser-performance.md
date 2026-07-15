# Parser performance and AST allocation decision

RUE-906 rebenchmarked the handwritten parser introduced by RUE-905. The
canonical full-pipeline measurement remains `scripts/rue perf --release`; the
parser-only allocation survey is:

```bash
scripts/parser-profile.py --release
```

The survey builds a small measurement-only binary and runs every sample in a
fresh process with an enforced 10-second default timeout. It lexes before
starting its clock and resetting its counting allocator, then measures exactly
`Parser::new(...).parse_preserving_interner()`.
Consequently its allocation calls and requested bytes include parser state,
AST construction, parser diagnostics, directive validation, and interner work
initiated by the parser, but exclude source I/O, lexing, session queries,
lowering, and code generation. Allocation density is a controlled proxy for
heap scattering/locality pressure; it is not a hardware cache-miss counter.
The full-pipeline spans establish whether even eliminating that parser cost
could materially change compilation time.

The corpus covers a tiny valid file, two modules parsed through one shared
interner, two checked-in stress programs, an 8/32/128/512-function malformed
recovery series, and a 12/24/48/96/192-level balanced-block series containing
64 functions at each depth. The checked-in `deep_nesting` workload (150
functions, nesting up to 40 levels, 12,198 lines) is included in the ordinary
performance harness. RUE-787 separately owns stronger baseline-shape and
regression-timeout guarantees for that checked-in corpus.

## 2026-07-14 result

Environment: Apple M5 (10 cores), 24 GiB RAM, macOS 26.5.1, arm64; commit
`46a8c1e4`; release compiler/driver; seven fresh parser-profile processes after
one warmup and five fresh full-compiler processes after one warmup; compiler
jobs fixed at one and Rue programs compiled at `-O0`.

| workload | outcome | tokens | parser ms (median ± MAD) | allocations | bytes/token |
|---|---:|---:|---:|---:|---:|
| hello | success | 10 | 0.004 ± 0.000 | 4 | 144.8 |
| multi-file | success | 39 | 0.006 ± 0.000 | 15 | 125.1 |
| control-flow stress | success | 47,108 | 0.934 ± 0.020 | 23,660 | 113.7 |
| deep-nesting stress | success | 41,208 | 0.858 ± 0.004 | 22,383 | 109.6 |

The large valid workloads allocate about 0.50--0.54 times per token. The
controlled scaling families were:

| family | scale | token growth | time growth | allocation growth | time/token growth |
|---|---:|---:|---:|---:|---:|
| malformed recovery | 8 → 512 functions | 63.48x | 22.49x | 50.71x | 0.35x |
| balanced nesting | 12 → 192 levels | 11.90x | 15.20x | 14.66x | 1.28x |

These endpoints are descriptive, not an asymptotic proof. Across the measured
range, malformed recovery grows below token count, while the nesting family's
time and allocations remain within 1.28x and 1.23x of token growth,
respectively. That is consistent with near-linear work and provides no evidence
of the former exponential branch search. Every subprocess is independently
bounded by the configured timeout.

The production full-compiler harness now preserves each inclusive `parser`
aggregate and computes its percentage per sample before taking the median. In
the matched release corpus those parser medians total 5.48 ms across 417.04 ms
of summed compile medians: **1.31%**. The largest measured median share is
`register_pressure` at 0.85/45.58 ms (1.86%). Aggregate spans overlap their
children and are therefore reported separately from the leaf-pass ranking.
This result is comparable within the RUE-906 run, but not as an absolute ratio
against the old DEFAULT-profile RUE-892 numbers.
RUE-904's approximately 14.5x candidate comparison and RUE-905's replacement
smoke tests remain the appropriate matched-regime historical evidence.

## Decision

Do not replace the recursive AST with an indexed/arena AST now. The production
largest measured inclusive parser share is 1.86%, while an arena
would spread representation changes across the parser and every AST consumer.
Even eliminating that span entirely would fall below the decision threshold.
The current language grammar is amenable to fast
deterministic recursive descent; malformed recovery and deep nesting do not
reveal a residual complexity problem.

Reconsider an arena only when a matched release profile shows parser work at
least 10% of representative compile time (or at least 5 ms of an interactive
compile), and a prototype demonstrates at least a 20% parser-time reduction or
a 2% end-to-end reduction. Require cache-miss evidence before attributing a win
to locality rather than allocation count. This keeps an AST representation
migration separate from parser implementation work and gives it a measurable
success threshold.
