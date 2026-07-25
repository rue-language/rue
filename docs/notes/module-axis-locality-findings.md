# Warm body locality is decided by the discovery protocol, not module count

Status: diagnostic measurement. Produced by
`rue-compiler-session-bench --module-axis`, which holds the call graph, reached
body count, and edit shape fixed and varies only the number of modules the
bodies are spread across and the protocol used to commit import discovery.

## The disagreement this resolves

Two checked-in workloads reported incompatible warm body locality:

- the N=128 completion scenarios reanalyze exactly the edited leaf after a
  reachable-body edit (`bodies_attempted == 1`), which is the evidence
  [`body-analysis-cfg-incrementality-audit.md`](body-analysis-cfg-incrementality-audit.md)
  cites for exact reverse invalidation; and
- the seven-file representative fixture reanalyzes all six of its bodies after a
  leaf edit with zero durable reuse, including three bodies that are provably
  outside the edit's reverse closure.

Those fixtures differ in body count, module count, standard-library use, and
discovery protocol simultaneously, so neither result could be attributed to a
cause. The obvious hypothesis was that the representative program is simply too
small for locality to be visible — that its reverse closure covers the whole
program. That hypothesis is wrong.

## Result

The module axis holds everything constant except module count and discovery
protocol. `staged` commits an already-complete snapshot; `rooted_demand` opens an
import-input request and drains the demand frontier, which is what the
representative fixture does and what the driver does when it reads imports from a
filesystem.

| modules | discovery | leaf edit: computed | reused | unrelated edit: computed | reused |
|---:|---|---:|---:|---:|---:|
| 1 | staged | 1 | 15 | 0 | 16 |
| 1 | rooted_demand | 16 | 0 | 16 | 0 |
| 2 | rooted_demand | 16 | 0 | 16 | 0 |
| 4 | rooted_demand | 16 | 0 | 16 | 0 |
| 8 | rooted_demand | 16 | 0 | 16 | 0 |

(16 reached bodies; `computed` is `semantic_work.body_analyses_computed`.)

The same shape holds at the completion workload's own N=128, where the
declaration-context cost is visible as well:

| modules | discovery | scenario | computed | reused | durable source records copied |
|---:|---|---|---:|---:|---:|
| 1 | staged | cold | 128 | 0 | 16512 |
| 1 | staged | leaf edit | 1 | 127 | 129 |
| 1 | staged | unrelated edit | 0 | 128 | 0 |
| 1 | rooted_demand | cold | 128 | 0 | 16512 |
| 1 | rooted_demand | leaf edit | 128 | 0 | 16512 |
| 1 | rooted_demand | unrelated edit | 128 | 0 | 16512 |
| 8 | rooted_demand | leaf edit | 128 | 0 | 17408 |
| 128 | rooted_demand | leaf edit | 128 | 0 | 32768 |

Under the rooted-demand protocol a one-line edit to a single leaf performs
*exactly* the declaration-context work of a cold build — 16512 records at one
module, identical to the cold row. Under the staged protocol the same edit costs
129. The warm path is not a reduced cold path on that protocol; it is the cold
path.

Two readings follow directly:

1. **Module count changes nothing.** Every `rooted_demand` row is identical from
   one module to eight. Spreading the same bodies across a module graph does not
   degrade locality, and the representative fixture's size was never the cause.
2. **The discovery protocol changes everything.** The first two rows are the same
   program, the same module count, and the same edit. Under `staged` a leaf edit
   reanalyzes one body; under `rooted_demand` it reanalyzes all sixteen, and an
   edit to an unreachable body reanalyzes all sixteen as well.

Durable CFG reuse is unaffected in every row: a leaf edit rebuilds one CFG and
reuses the rest under both protocols. Only body-query reuse collapses.

## Why this matters for the claimed evidence

`staged` is not a protocol a real program can use. With any import present the
observation ledger is incomplete and the attempted revision refuses to close —
the workload asserts this by construction, which is why the axis only runs
`staged` at one module. So the exact-reverse-invalidation result rests on a
discovery shortcut available only to single-file programs with no imports.

Every multi-module program takes the `rooted_demand` path. On that path, warm
body reuse is currently zero regardless of program size.

This does not contradict any individual assertion in the completion workload;
those assertions are accurate about the corpus they measure. It qualifies what
that corpus is evidence *for*.

## Consequences for RUE-1091

The per-body context repair addresses the O(bodies x declarations) cost inside
`analyze_body_query` — the work a body performs once it has been chosen for
analysis. The behavior measured here decides *how many* bodies are chosen. They
are different layers, and the numbers above suggest the second one is not
addressed by the first.

Worth confirming before the provider flip rather than after: if the flip lands
and warm rebuilds on multi-module programs are still recomputing every body, the
cause will be here rather than in the repaired per-body path.

## Reproducing

```sh
./buck2 run //crates/rue-compiler-session-bench:rue-compiler-session-bench -- \
  --module-axis --bodies 16 --module-counts 1,2,4,8
```

The workload gates on structural counters only and asserts warm/fresh parity for
every measured edit. It pins the `staged` locality result, because that is the
behavior the existing audit claim rests on; the `rooted_demand` rows are
deliberately left unasserted so a repair does not read as a failure.
