# Clone-from-template probe: derivation is 83% of the per-body prefix, and removing it does not make the prefix flat

Status: executed feasibility measurement (RUE-1133 — RUE-1091 ordered probe #1).
Measurement only. **Nothing in this probe is a production boundary**, and none
of it may be promoted to one: a cheap copy narrows no dependency, so it is
performance evidence and nothing else. It cannot complete RUE-1091.

Raw evidence:
[`../benchmarks/rue-1133-clone-from-template-probe.jsonl`](../benchmarks/rue-1133-clone-from-template-probe.jsonl)
(one JSON object per curve point, every sample retained).

## The question

Every reached body rebuilds the whole declaration epoch before it analyzes one
body — `prepare_query_declaration_shells`,
`project_durable_declaration_semantics`, the durable install, and the stable
endpoint installs. That prefix is O(declarations) per body and dominates the
cold compile.

RUE-1091 lists clone-from-template as ordered probe #1 precisely because
nobody knows how that prefix decomposes. Two very different repairs follow from
two possible answers:

- if the prefix is mostly **derivation**, a scheme that stops re-deriving it —
  a shared base, or a copy — recovers nearly all of it; and
- if a large part of the prefix is **structure that any per-body epoch must
  still materialize**, then no sharing scheme reaches parity and only the full
  provider path (RUE-1092) can.

The probe answers this by replacing derivation with copying and changing
nothing else: the bound epoch is derived **once per revision** and deep-copied
per body. That is still O(declarations × bodies), deliberately. The difference
between the arms is the derivation cost; what survives in the clone arm is the
structural floor of a per-body epoch.

## How it was run

`crates/rue-compiler/src/clone_probe.rs` retains one bound epoch per semantic
request and serves each reached body from a copy of it. It is off unless a
harness calls `unstable::enable_clone_from_template_probe`, which only the
measurement binaries do; an ordinary compile never constructs a probe and every
probe counter reads zero.

Two arms:

- **rebuild arm** — production today, unchanged.
- **clone arm** — one derivation per revision, one deep copy per body.

A third **differential** arm analyzes every body twice, once each way, and
compares the published `BodyTransaction`s. It is never timed.

```bash
# Both arms at one curve point, plus the parity run (raw JSON on stdout):
./buck2 run --target-platforms //platforms:release \
    //crates/rue-scaling-bench:rue-scaling-bench -- \
    --mode probe --bodies 200 --decls 400 --iterations 3 --json

# Allocation counts under either arm (counting global allocator):
./buck2 run --target-platforms //platforms:release \
    //crates/rue-scaling-bench:rue-scaling-bench-allocations -- \
    --mode alloc --bodies 200 --decls 400 --clone-probe clone

# Peak resident memory, isolated child process:
./buck2 run --target-platforms //platforms:release \
    //crates/rue-scaling-bench:rue-scaling-bench -- \
    --mode memory --bodies 200 --decls 400 --clone-probe clone

# Multi-module parity and copied units (counters only):
./buck2 run --target-platforms //platforms:release \
    //crates/rue-compiler-session-bench:rue-compiler-session-bench -- \
    --module-axis --bodies 64 --module-counts 1,8
```

Reference host: `nproc=4`, 15.7 GiB, linux/x86_64, release platform, on top of
`ec49f3f`. The corpus is the `rue-scaling-bench` synthetic generator, which
holds the call graph fixed and varies reached bodies (`B`) and unrelated
declarations (`D`) independently. `N = B + D + 1` is the declaration universe
each epoch carries.

## Parity first

The timing result would be meaningless if the clone published a different
program. It does not, across every configuration measured:

| corpus | bodies compared | mismatches |
|---|---:|---:|
| scaling corpus, all 10 curve points | 1 785 | 0 |
| module axis, 64 bodies / 1 module, staged discovery | 64 | 0 |
| module axis, 64 bodies / 1 module, rooted demand | 64 | 0 |
| module axis, 64 bodies / 8 modules, rooted demand | 64 | 0 |
| producer-nominal corpus (unit test) | 7 | 0 |

`transaction_equal` compares the canonical body, the references, the produced
anonymous nominals, and the rendered diagnostic stream, so a divergence in
artifacts, producer-nominal identity, or diagnostic *order* would have failed.
One template served every body of a revision in every case
(`templates_built == 1`, `template_input_misses == 0`) — including the
multi-module rooted-demand path and a corpus whose bodies materialize producer
nominals.

## Result

Semantic-phase medians, 3 iterations per point. `tmpl` is the one per-revision
derivation, `copy` is the total of all per-body copies, `anlz` is body analysis
and publication, and `resid` is the remainder of the clone arm.

| B | D | N | rebuild | clone | speedup | tmpl | copy | anlz | resid |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 25 | 200 | 226 | 64.8 ms | 24.2 ms | 2.68× | 1.90 | 1.06 | 1.17 | 20.0 |
| 50 | 200 | 251 | 133.2 ms | 36.7 ms | 3.63× | 2.36 | 2.28 | 2.73 | 29.3 |
| 100 | 200 | 301 | 275.8 ms | 55.5 ms | 4.97× | 2.48 | 4.84 | 5.67 | 42.5 |
| 200 | 25 | 226 | 432.5 ms | 82.5 ms | 5.24× | 1.92 | 8.71 | 10.55 | 61.3 |
| 200 | 50 | 251 | 463.3 ms | 86.3 ms | 5.37× | 2.05 | 8.38 | 9.98 | 65.9 |
| 200 | 100 | 301 | 527.7 ms | 91.0 ms | 5.80× | 2.49 | 9.21 | 11.16 | 68.1 |
| 200 | 200 | 401 | 718.3 ms | 118.9 ms | 6.04× | 3.70 | 14.74 | 17.70 | 82.8 |
| 200 | 400 | 601 | 1099.6 ms | 176.5 ms | 6.23× | 5.69 | 22.30 | 22.87 | 125.7 |
| 200 | 800 | 1001 | 1971.0 ms | 315.8 ms | 6.24× | 10.96 | 39.55 | 30.52 | 234.8 |
| 400 | 200 | 601 | 2194.8 ms | 336.6 ms | 6.52× | 6.10 | 45.19 | 45.72 | 239.6 |

Both arms are O(bodies × declarations), so the honest unit is nanoseconds per
`body × declaration`. Normalizing collapses both curves onto flat lines — which
is itself the finding, because a repair would have bent one of them:

| B | D | rebuild | clone | of which copy | of which residual | units copied per body |
|---:|---:|---:|---:|---:|---:|---:|
| 25 | 200 | 11 032 | 4 113 | 180 | 3 410 | 1 368 |
| 50 | 200 | 10 404 | 2 867 | 178 | 2 291 | 1 518 |
| 100 | 200 | 9 073 | 1 824 | 159 | 1 397 | 1 818 |
| 200 | 25 | 9 520 | 1 816 | 192 | 1 350 | 1 368 |
| 200 | 50 | 9 183 | 1 710 | 166 | 1 306 | 1 518 |
| 200 | 100 | 8 722 | 1 503 | 152 | 1 126 | 1 818 |
| 200 | 200 | 8 911 | 1 476 | 183 | 1 027 | 2 418 |
| 200 | 400 | 9 103 | 1 461 | 185 | 1 040 | 3 618 |
| 200 | 800 | 9 796 | 1 570 | 197 | 1 167 | 6 018 |
| 400 | 200 | 9 107 | 1 397 | 188 | 994 | 3 618 |

(ns per `body × declaration`. The two smallest-body rows — B=25 and B=50 —
still carry an unamortized fixed per-revision cost and are excluded from the
means below; the remaining eight points are used.)

**The split.** Of the ~9 180 ns per `body × declaration` a cold compile spends
today:

- **~7 580 ns (83%) is derivation.** It disappears when the epoch is derived
  once and copied.
- **~180 ns (2%) is the copy** — the structural cost of moving a whole
  declaration epoch into a body.
- **~190 ns (2%) is body analysis and publication**, the only per-body cost that
  is genuinely about the body.
- **~1 180 ns (13%) is a residual** that is none of the above and that still
  grows with `bodies × declarations`.

The clone arm's own ~1 595 ns therefore decomposes as 11% copy, 12% analysis,
74% residual.

The epoch itself is 6.0 units per declaration (`units copied per body` divided
by `N` is 6.01–6.05 at every one of the ten points), split almost exactly evenly
between the declaration namespace and the stable endpoint tables:

| family | units per body at B=200 / D=800 | share |
|---|---:|---:|
| declaration namespace | 3 009 | 50.00% |
| endpoints | 3 005 | 49.93% |
| type pool | 3 | 0.05% |
| module registry | 1 | 0.02% |
| parameters | 0 | 0% |

## Allocation and peak memory

Cold, `B=200 / D=400`, counting global allocator for the allocation rows and an
isolated child process for the peak:

| arm | allocations | bytes allocated | peak resident |
|---|---:|---:|---:|
| rebuild (production) | 4 640 860 | 1 196.1 MiB | 26.8 MiB |
| clone | 944 106 | 194.7 MiB | 26.6 MiB |

Copying the epoch allocates 4.9× fewer times and 6.1× fewer bytes than deriving
it. Peak resident memory is unchanged (−0.7%): a per-body epoch is transient
either way, so neither arm retains more than one at a time. **Clone-from-template
is not a memory risk; it is also not a memory win.**

## The read RUE-1133 asks for

> An explicit read on whether a shared-base scheme (A3) can reach pre-cutover
> parity, or whether the residual structural cost means only the full provider
> path can.

**A3 cannot reach pre-cutover parity on this evidence, and the reason is not the
structural cost of the epoch.**

The probe brackets A3 tightly. A3 shares an immutable base instead of copying
it, so relative to the clone arm it can remove *at most* the copy — 178 ns of
the clone arm's 1 595 ns per `body × declaration`, 11%. Its best possible result
is therefore ~1 417 ns, against ~9 180 today: a **6.5× cold improvement, versus
the naive copy's 5.75×**.

That is a large win and worth having. It is not parity. Pre-cutover work was
O(declarations + Σ bodies) — the declaration epoch was built once for the whole
program — and **both** the clone and A3 leave an O(bodies × declarations) term
standing. The probe shows where that term is not:

- it is **not** the epoch copy (178 ns, 11% of what the clone arm still spends);
- it is **not** body analysis and publication (194 ns, 12%);
- it is ~1 176 ns per `body × declaration` that this probe removes nothing from,
  and that neither A3 nor any other epoch-sharing scheme touches, because it is
  outside the declaration epoch entirely.

So the sequencing implication is concrete, and it is the opposite of the
intuition that motivated the probe. The structural cost of the *epoch* turned
out to be negligible (2% of today's cold work) — the fear that a per-body epoch
is irreducibly expensive to materialize is not supported. What is not negligible
is the per-body work that remains after the epoch is free. Removing the epoch
derivation is worth 5.75×, and A3 is worth about 13% more than that; reaching
pre-cutover parity additionally requires narrowing what a body observes, which
is the provider path (RUE-1092) and the warm-locality work tracked separately.

This probe deliberately does not attribute the residual term. It measures that
the term exists, that it scales with `bodies × declarations`, and that it is
disjoint from the three costs the probe does account for. Attributing it is the
next measurement, and it is now the highest-value one: it is 74% of what remains
once the derivation is gone.

## What this probe does not establish

- **Nothing about invalidation.** The clone observes exactly what production
  observes. It narrows no dependency and recomputes exactly the same bodies. It
  is not a partial RUE-1091 repair and must not be counted as one.
- **Nothing that can ship.** Copying the epoch per body is O(D × B); it is
  cheaper than deriving it per body and is otherwise the same mistake.
- **Nothing about the large examples.** The corpora here are the synthetic
  scaling and module-axis harnesses, which vary declaration count and body count
  independently in seconds. The large example programs are the milestone gate,
  not a development loop, and were deliberately not used.

## Counters stay honest

The ordinary per-body prepare/project/install/endpoint counters were not
reclassified. In the clone arm those stages genuinely run once per revision, so
those counters record **one** prefix rather than one per body, and the probe's
own counters report every unit the copies moved. Neither number is allowed to
hide the other; `the_clone_arm_charges_one_prefix_and_reports_the_copy_that_replaced_it`
in `crates/rue-compiler/src/clone_probe.rs` pins that relationship.
