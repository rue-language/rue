# Warm body locality is decided by the discovery protocol, not module count

Status: historical diagnostic measurement. The retired standalone runner held
the call graph, reached body count, and edit shape fixed while varying only the
number of modules the bodies were spread across and the protocol used to commit
import discovery.

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

## Mechanism

The measurement above narrows the cause to a single call. At one module the
demand path performs the same work as the staged path plus
`begin_import_input_request`, and locality is already gone. Reading that path
gives the exact mechanism, and it is not an oversight.

`rue_query::Revision` is a pair of an id and a *compatibility token*, and
retained work is validated across revisions by comparing the token alone
(`crates/rue-query/src/lib.rs:87`, consumed at `lib.rs:3609`):

```rust
pub const fn is_compatible_with(self, other: Self) -> bool {
    self.compatibility == other.compatibility
}
```

The two publication paths choose that token differently:

- an ordinary source update publishes `Revision::new(self.next_revision, 1)` —
  a **constant** token, so every retained terminal stays validatable across
  edits (`revisioned_query_database.rs:11053`);
- import discovery publishes `Revision::new(self.next_revision, generation)`
  (`revisioned_query_database.rs:10644`), where `generation` is incremented by
  every `begin_import_inputs` call (`revisioned_query_database.rs:10224`).

So each import-input request mints a fresh compatibility token, and no terminal
retained under the previous token can be validated against it. Every body is
recomputed — not because its inputs changed, but because the token it was
retained under no longer matches.

This is deliberate, and the code says so at the increment:

> A new request generation is a fresh filesystem observation epoch. Reuse
> requires a future explicit watch/read-policy proof token. The API deliberately
> has no carried-ledger input that could be mistaken for freshness authority.

That is a sound conservatism in isolation: without evidence that the filesystem
has not changed underneath it, the compiler refuses to trust anything it
retained. The finding here is not that the rule is wrong. It is that **nobody
had measured what it costs**, and the cost is the entire warm-rebuild benefit
for every program that reaches its sources through import discovery.

## The design question this raises

ADR-0063 §2 states:

> Terminals from the previous attempt are validated or red/green reused against
> the successor; an unchanged input leaf carried into the successor does not
> become different merely because its revision number advanced.

The epoch reset and that sentence are in tension. Both are defensible; they
cannot both be fully honored without the "future explicit watch/read-policy
proof token" the comment names, which does not exist yet.

Resolving this is a design decision, not a repair — it decides when the compiler
is entitled to believe the filesystem has not moved. It is deliberately not
changed here. The candidate directions, for whoever picks it up:

1. Introduce the proof token the comment anticipates, so a request that can
   demonstrate unchanged inputs carries its predecessor's compatibility token
   instead of minting a new one.
2. Separate "filesystem freshness" from "semantic input identity" so that a new
   observation epoch invalidates only the leaves it actually re-observed, rather
   than every terminal in the graph.
3. Accept the cost and state plainly that warm incremental rebuild is available
   only to in-process hosts that manage their own source snapshots — which would
   make the north-star interactive rebuild unreachable for the CLI driver.

Option 3 is the status quo by default, and it is worth being explicit that it is
the status quo rather than arriving there without deciding.

## Consequences for RUE-1091

The per-body context repair addresses the O(bodies x declarations) cost inside
`analyze_body_query` — the work a body performs once it has been chosen for
analysis. The behavior measured here decides *how many* bodies are chosen. They
are different layers, and the mechanism above confirms the second is untouched by
the first: the compatibility token is chosen in the import-input publication
path, which the provider rewire does not go near.

So the flip can proceed without waiting on this. What must not happen is reading
the post-flip warm numbers as a verdict on the flip. If multi-module warm
rebuilds still recompute every body afterward, that is the epoch reset, not a
failed repair — and the post-flip measurement should say which rows it expects
to move before it is run rather than after.

The historical workload gated on structural counters only and asserted
warm/fresh parity for
every measured edit. It pins the `staged` locality result, because that is the
behavior the existing audit claim rests on; the `rooted_demand` rows are
deliberately left unasserted so a repair does not read as a failure.
