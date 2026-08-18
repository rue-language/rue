# Caldera compiler scaling snapshot

These measurements use the same compiler revision (`bcfccc3aa94`), host, and
regime: a release-built compiler (`--target-platforms //platforms:release`),
the default `-O0` pipeline, a warm compiler binary, the internal linker, and
`--benchmark-json`. Each number is the median of three runs on one Linux
x86-64 host. They are a checked-in snapshot, not a statistically stable
performance gate; RUE-1038 tracks retained multi-run scaling history.

| Program | Application Rue lines | Compiler graph lines | Files | Tokens | Functions | Compile | Peak memory | Executable |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Lattice | 13,030 | 20,462 | 161 | 160,221 | 1,221 | 3.54s | 321.7MB | 1.66MB |
| Meridian | 35,885 | 43,295 | 295 | 405,440 | 3,074 | 8.75s | 933.7MB | 4.78MB |
| Caldera | 104,945 | 112,258 | 828 | 1,200,390 | 8,569 | 20.81s | 2.05GB | 11.46MB |

Scaling is now essentially linear. Caldera's full source graph is 2.59 times
Meridian's and compiles in 2.38 times the wall clock; it is 5.49 times
Lattice's graph and compiles in 5.88 times the wall clock. Compile time per
function is flat across the range (2.90ms, 2.85ms, and 2.43ms), and the
deterministic per-function compiler-work counters stay within roughly 0.8 to
1.3 times Lattice's rates across the semantic-provider, CFG, and
query-runtime families. The distinctly nonlinear semantic scaling reported by
the previous snapshot (80.29s for Caldera at revision `54a2eac5`) is no
longer present.

The remaining cost is an absolute constant factor rather than a scaling
curve. On Caldera the query runtime performs roughly 33.5 million validation
probe, visit, lease, and demand operations to support 213 thousand computed
queries in a fresh single-revision process, and body-local symbol interners
retain 12.5MB across 336 thousand entries for a program whose distinct
identifier text is about 61KB. Peak memory remains above 2GB, and the
compiled selftest runs in well under a second, so compilation — not Caldera
runtime — remains the operative bottleneck.
