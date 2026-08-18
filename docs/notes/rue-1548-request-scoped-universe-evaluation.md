# RUE-1548 evaluation: body-local semantic epochs against shared alternatives

Status: measurement and design-evaluation note, 2026-08-18. It supplies the
comparison RUE-1548 asks for — one request-scoped immutable semantic universe,
a shared base with body-local overlays, and the current body-local epochs —
as measured evidence for the maintainer decision. It decides nothing.

Method: `valgrind --tool=callgrind` instruction attribution on the
release-built compiler (`--target-platforms //platforms:release`), cold
`-O3 -j1` builds, `RUE_STD_PATH` at the repository `std`, on trunk
containing the RUE-1576 seam handoff and the RUE-1583 batch-authority seed.
Instruction counts are deterministic; wall clock on this host is not quoted
as evidence anywhere. Deterministic work counters come from
`--benchmark-json` on the same binary.

Three shapes: the fixed Lattice reference workload
(`performance/workloads/lattice/main.rue`, 1,263 bodies), Caldera
(`examples/caldera/main.rue`, 8,613 bodies, the 100K-line graph), and a
regenerated chain256 falsifier (256 single-call modules plus a root, no
shared nominal closure, matching the shape described in
`per-body-identity-closure-materialization.md`).

## Result

The per-body re-materialization stack is one third of the cold build, its
share is scale-stable from 1.2K to 8.6K bodies, and it falls to almost
nothing on a shape with no shared closure. The largest single term is no
longer the semantic identity cluster the earlier note measured — post
ADR-0076 and the spelling memos, it is the CFG-side selection, epoch
construction, and projection stack, which none of the previously landed
fixes reach.

Program totals: Lattice 6,927,879,774 instructions, Caldera 53,127,158,325,
chain256 496,020,152. Caldera per body is 6.2M instructions against
Lattice's 5.5M — near-linear, matching the compile-time and counter
linearity in `examples/caldera/SCALING.md`.

## Where the stack is, exactly

Inclusive attribution, non-overlapping call trees (caller/callee verified
from the callgrind data: `select_materialization_facts` runs under the
session's rooted CFG request, outside `evaluate_cfg`;
`materialize_semantic_body_with_indexes` runs under `evaluate_cfg`;
`CfgDomainProjection::from_local_body` runs directly under `evaluate_cfg`
and `codegen_domain` inside `build_cfg` — the three rows below are mutually
disjoint subtrees, so their sum is meaningful):

| subtree | Lattice | Caldera | chain256 |
| --- | ---: | ---: | ---: |
| `select_materialization_facts` | 10.84% | 11.18% | below threshold |
| `materialize_semantic_body_with_indexes` | 6.87% | 7.38% | 2.29% |
| `CfgDomainProjection::{codegen_domain, from_local_body}` | 5.03% | 5.42% | below threshold |
| CFG-side per-body re-materialization, total | ~22.7% | ~24.0% | ~2.3% |

Inside the selection subtree on Caldera: hashing `DurableAnonymousNominal`
is 1,942,198,440 instructions — 3.66 percent of the entire build — from
125,064 hash invocations against 68 distinct anonymous nominals, and
`Selection::semantic_type` runs 3,196,173 times for 1.74 percent. Inside
epoch construction, `SemanticImportEpoch::new_local` is 4.38 percent and
`materialize_local_body_with_types` 2.71 percent, once per body each.

The semantic identity cluster measured at 11.43 percent by the earlier note
remains substantial after ADR-0076: the entry points
(`resolve_provider_type` 4.94%, `find_or_create_anon` 4.25%,
`resolve_function` 3.82%, `build_function_signature` 3.79%, `intern_params`
3.73%, `resolve_callable_type` 3.71%, anonymous-method endpoint
registration 3.28%, import nominal registration 3.04% on Caldera) overlap
one another, so they cannot be summed; a conservative non-overlapping
reading puts the cluster at roughly 10 percent of the build. ADR-0076
removed the interner-insertion half; the mint-and-key half — parameter
arenas, key clones, durable-fact reads, per-body pool writes — is what
remains, and the deterministic counters agree
(`nominal_materializations` 58,852 for ~105 named structs,
`import_nominal_type_visits` 206,607, on Caldera).

The falsifier behaves as the earlier note predicts: chain256 has no shared
closure, its selection and projection terms vanish, and its remaining
`materialize_semantic_body_with_indexes` share is fixed per-body epoch
setup. Any design note predicting a uniform speedup is wrong; the honest
prediction is a saving proportional to closure reuse — which the 100K-line
application shape has in abundance.

## The three candidates against the measurements

**Current body-local epochs (status quo).** Every body pays selection,
epoch construction, and projection against durable facts; every CFG record
retains a small private interner (Caldera: 336,445 entries and 12.5MB
across 8,605 records, averaging 39 entries and 1.45KB per record, for a
program whose distinct identifier text is 61KB) and a private type pool
(258,396 entries for a few hundred distinct types). The cost is ~24 percent
of the build plus the ~10 percent identity cluster; the benefit is exact
per-body invalidation, request-independent artifacts, and no shared mutable
state — the ADR-0063 properties the current design deliberately buys.

**Shared base plus body-local overlays.** Reaches the identity cluster (the
mint-and-key work becomes once-per-revision) but not the CFG-side stack:
selection still copies facts into per-body closures, epochs are still
constructed per body, and projections still run per body, because what CFG
consumes is still body-local. Reachable pool: roughly the ~10 percent
cluster, minus overlay bookkeeping. The earlier note's pool-only variant of
this measured 0.71 percent reachable; the difference is whether signatures,
parameter arenas, and durable-fact reads share too.

**Request-scoped immutable universe.** Eliminates the CFG-side stack by
construction — CFG consumes shared typed identities directly, so there is
nothing to select into, materialize, or project per body — and subsumes the
shared-base saving. Reachable pool: the ~24 percent CFG-side stack plus the
~10 percent cluster, bounded by whatever replaces them (universe
construction once per revision, plus small per-body index setup). On the
measured shapes that is on the order of one third of the cold build, and
approximately zero on chain256-shaped programs.

## What the decision has to resolve

These are the prior note's ADR questions, unchanged, now with the stake
quantified:

1. **Index-space stability.** Pool indices and interner handles become
   shared. `ConstraintGenerator::expr_types` ordering, `CfgDomainProjection`
   reads, and every index consumer must either be order-independent or the
   universe must assign indices deterministically from durable identity, or
   emitted output is not byte-identical and the executable digests and
   counter baselines all rebaseline.
2. **Invalidation scope.** The universe is immutable after semantic
   fixed-point completion and owned by the request or published root. A
   declaration edit rebuilds it; the design must show a body edit does not,
   and must state what happens to bodies analyzed against a superseded
   universe (RUE-1548's acceptance requires proving exact per-body
   invalidation survives).
3. **`require_rir_authority`.** The pointer-equality contract between a
   body's RIR interner and its analysis state does not survive a shared
   space unchanged; ADR-0076's shared-generation machinery is the template
   for what replaces it.
4. **Retention and memory.** Today's retained body-local charge (12.5MB of
   interners, 258K type-pool entries on Caldera) collapses into one shared
   structure; the retained-charge accounting, the cross-request cache
   story, and the 2.05GB Caldera peak all need restating. The counter
   contract changes by construction — the `cfg_local_epoch` and
   `cfg_materialization` families describe work that no longer exists.

## Recommendation ordering

The measurements invert the incremental-first instinct. The shared-base
option pays most of the universe's contract cost (shared index space,
`require_rir_authority`, rebaselining) for less than a third of its
reachable pool, because the CFG-side stack — the largest term — only falls
when CFG consumes shared identities directly. If the contract cost is going
to be paid at all, the evidence says pay it once, for the universe. The
alternative consistent position is to keep the status quo and continue
harvesting bounded subsets (the RUE-1547/RUE-1574/RUE-1563 pattern; the
`DurableAnonymousNominal` hashing term inside selection looks like one more
such subset — a precomputed digest or interned handle would remove ~3.7
percent without any sharing decision).

The decision stays with the maintainers; RUE-1548 records it.

## Phase 0 verdict: index order does not reach emitted output

The maintainers accepted the universe direction on 2026-08-18 and asked for
the gating unknown — contract question 1 — to be answered before the ADR.
The spike answered it empirically with a mint-order perturbation rather
than a prototype shared pool: an env-gated change to
`BodyIdentityPool::mint_named_provider` resolved struct fields and enum
variant payloads in reversed order, permuting the per-body pool ids of
every nominal first reached through a field or payload while the completed
definitions kept declaration order. A stderr witness confirmed the
perturbation fired on real multi-field nominals (`StrBuf`, `Result`,
application structs) rather than measuring a dead knob — an earlier
attempt at the import-registration layer reversed only bookkeeping order,
because `resolve_provider_type` on the parent mints fields before the
registration recursion runs, and was discarded for exactly that reason.

Result: emitted executables are byte-identical under the reversed mint
order on the fixed Lattice workload, Caldera, and the chain shape, and
`compiler_work` is identical except a three-count wobble in
`query_runtime.reuses` on Lattice from shifted query timing. This matches
the pool's design statement (`body_identity.rs`: transient pool indices,
durable-keyed exports — the RUE-1091 pool-keystone) and retires the
`expr_types` ordering concern for the named-nominal family: the one raw-id
sort on that path (`string_literal_types`, sorted by `Type::as_u32`)
feeds a dedup and an exact `Str(N)` match, not a selection.

Answer to contract question 1: a request-scoped shared assignment is
viable behind the existing byte-identity gate for the named-nominal
family. Two bounded caveats for the ADR: the same perturbation test
should be repeated per family as each vertical migrates (anonymous
nominals, array/pointer interning — whose creation order today follows a
fixed-seed `HashMap` iteration — and callable/parameter identities were
not perturbed here), and sparse universe-wide id values in per-body dense
tables are a memory-layout question the reversal cannot probe, though the
durable-keyed export seam means they are not a byte-identity question.
