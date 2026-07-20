# Caldera compiler scaling snapshot

These single-run measurements use the same compiler revision
(`54a2eac5`), host, default `-O0` pipeline, a warm compiler binary, the
internal linker, and `--benchmark-json`. They are a checked-in snapshot,
not a statistically stable performance gate; RUE-1038 tracks retained
multi-run scaling history.

| Program | Application Rue lines | Compiler graph lines | Files | Tokens | Compile | Peak memory | Executable |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Lattice | 13,030 | 18,839 | 160 | 155,687 | 2.91s | 239.5MB | 1.74MB |
| Meridian | 35,863 | 41,672 | 294 | 403,784 | 13.12s | 1.12GB | 4.54MB |
| Caldera | 104,926 | 110,633 | 827 | 1,209,040 | 80.29s | 2.10GB | 10.27MB |

Caldera is 2.65 times Meridian's full source graph but takes 6.12 times
as long to compile. Semantic analysis grows from 7.52s to 56.12s (7.46
times), while code generation grows from 1.29s to 4.25s (3.30 times).
Against Lattice, Caldera's graph is 5.87 times larger but total compile
time is 27.57 times larger and semantic analysis is 40.32 times larger.

The current 100K result therefore succeeds as a capacity experiment but
exposes distinctly nonlinear semantic scaling. Peak memory remains below
2.2GB on this host, and the compiled selftest runs in roughly 0.06s, so
compilation—not Caldera runtime—is the operative bottleneck.
