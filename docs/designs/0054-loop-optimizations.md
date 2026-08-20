---
id: 0054
title: Loop Optimizations — LICM and Unrolling
status: accepted
tags: [compiler, codegen, optimization]
feature-flag: none
created: 2026-07-16
accepted:
implemented: Phase 3 (RUE-928)
spec-sections: []
superseded-by:
---

<!-- Note: Loop optimizations are compiler-internal; they never change what a
     program means, only how fast/large the emitted code is (ADR-0044's
     observable-behavior invariant). No preview gate applies. -->

# ADR-0054: Loop Optimizations — LICM and Unrolling

## Status

Accepted on 2026-07-16, closing the RUE-916 design issue and filing the phase
issues below. Phases 1 and 2 shipped in July 2026: natural-loop analysis and
preheader materialization landed with RUE-926
(`crates/rue-cfg/src/opt/loops.rs`) and trap-free LICM landed with RUE-927
(`crates/rue-cfg/src/opt/licm.rs`, run at `-O3` from `opt/mod.rs`), both built
on the RUE-914 dominator tree. Constant-trip unrolling (RUE-928, Phase 3) is
implemented; guarded trapping-op hoisting (RUE-934, Phase 4) remains tracked
and unimplemented.

ADR-0044 places loop-invariant code motion (LICM) and unrolling at `-O3` (the
speculative, size-spending, speed-chasing tier). This note designs the shared
loop-analysis infrastructure both need, then the two passes — with the central
attention on LICM's trap-safety problem, which is the inverse of the RUE-57 bug
class and the single most dangerous way a loop pass can violate the
observable-behavior invariant.

## Summary

Both LICM and unrolling need to recognize natural loops, which needs a dominator
tree. **The shared dominator tree already landed with RUE-914**
(`crates/rue-cfg/src/dominators.rs`): a CFG-level `DominatorTree` computed via the
**Cooper–Harvey–Kennedy iterative algorithm** over `Cfg::compute_predecessors()`,
consumed by value forwarding's Rule 1 dominance check and by the loop analyses
that followed. **Natural-loop detection** (back edges + loop bodies) and
**preheader materialization** landed with RUE-926
(`crates/rue-cfg/src/opt/loops.rs`), extending the dominator base with the
loop forest this note designs, recomputed per pass (not cached). (The separate,
per-backend post-lowering back-edge scan in `rue-codegen` used for register
allocation stays unusable at the CFG level.) On
that base: **LICM** (landed with RUE-927, `crates/rue-cfg/src/opt/licm.rs`)
hoists loop-invariant *pure* ops into the preheader, and its
governing rule is conservative — **trapping ops are never hoisted in the initial
version** (only provably-trap-free ops move), because hoisting a possibly-trapping
op onto a path the source never executed manufactures a trap out of thin air.
**Unrolling** is limited initially to constant-trip-count full unrolling under a
size budget. Both carry the ADR-0044 differential-coverage obligation and the
repository's work-counter discipline.

## Context

### Dominators, natural-loop detection, and preheaders all exist (ground truth)

The shared dominator tree **landed with RUE-914** and lives at
`crates/rue-cfg/src/dominators.rs`: a `pub(crate) struct DominatorTree` with
`DominatorTree::compute(cfg)`, `idom`, and `dominates` queries, built by the
Cooper–Harvey–Kennedy fixpoint over a reverse-postorder traversal and consuming
`Cfg::compute_predecessors()`. The module's own docs mark it "shared analysis
infrastructure, not a pass"; its consumers are value forwarding's always-on
Rule 1 dominance check (`crates/rue-cfg/src/opt/forward.rs:317`), natural-loop
detection, and LICM. The loop-specific layer this note designed **shipped with
RUE-926**: `crates/rue-cfg/src/opt/loops.rs` holds natural-loop detection, the
`LoopForest`, and preheader materialization (`ensure_preheader`), built on the
existing dominator tree rather than introducing one — exactly the gap this note
set out to fill.

The rest of `crates/rue-cfg/src/` is `build.rs`, `inst.rs`, `verify.rs`,
`dominators.rs`, `lib.rs`, and `opt/`; the optimizer directory now holds
`constfold.rs` (the fold kernel), `constopt.rs` (the sparse worklist driver),
`simplify.rs`, `peephole.rs`, `cse.rs`, `forward.rs`, `dce.rs`, and `mod.rs`.

The one existing loop analysis lives **downstream in codegen**:
`analyze_loops_adapter` / `has_back_edge` / `compute_loop_info` in
`crates/rue-codegen/src/liveness.rs:101, 336, 535` (the `LoopInfo` type itself is
defined in `crate::regalloc`). It runs on a **flat
instruction-index space** through a `LivenessAdapter`, is **per-backend**, runs
**after** CFG→MIR lowering, and exists to pick register-allocation live-range
strategies and loop depth. It is not reusable for CFG-level LICM/unrolling: it
sees machine-ish instruction indices, not CFG blocks; it runs too late (after the
CFG is gone); and it is duplicated per architecture. Building loop analysis at
the CFG level is the correct, target-independent home and avoids a third copy.

### The passes that exist, and the pass "shape" convention

The `-O1` set has grown well past the original three: `opt::optimize`
(`crates/rue-cfg/src/opt/mod.rs:142`) runs `constopt::run` (the sparse worklist
that interleaves constant folding — the `constfold` kernel — with store-to-load
constant propagation to an internal fixpoint, RUE-794), then `peephole::run`
(RUE-912), then `simplify::run` (CFG simplification, RUE-910/911), then `dce::run`.
Two more passes are gated to `-O2`/`-O3` inside that same arm: `forward::run`
(value forwarding / copy propagation, RUE-914) and `cse::run` (block-local CSE,
RUE-913), each behind `if matches!(level, OptLevel::O2 | OptLevel::O3)`
(`crates/rue-cfg/src/opt/mod.rs:184, 193`). So `-O2` already differs from `-O1`;
`-O3` still aliases `-O2` (there is no distinct `O3` arm yet). Loop passes are
`-O3` content and will be the first passes gated strictly above `-O2`.

### Work-counter discipline — what the codebase actually does

The brief refers to a "RUE-794 work-counter convention" and to `opt/simplify.rs`
and `opt/peephole.rs`. **All three now exist** — RUE-794 is the sparse worklist
driver in `crates/rue-cfg/src/opt/constopt.rs`; `simplify.rs` (RUE-910/911) and
`peephole.rs` (RUE-912) are landed passes. The convention they establish has two
layers, and loop passes should follow it:

1. **Per-pass `Stats` with bounded-work counters.** Each mutating pass exposes a
   `pub struct Stats` of counters and *returns it* from `run`, rather than a bare
   `bool`. `constopt::run -> Stats` (`fold_attempts`, `folded`, `loads_rewritten`;
   `crates/rue-cfg/src/opt/constopt.rs:56, 84`), and `simplify::run`,
   `peephole::run`, `forward::run`, and `cse::run` each likewise return a
   pass-specific `Stats` (`crates/rue-cfg/src/opt/simplify.rs:66, 81`,
   `peephole.rs:46, 56`, `forward.rs:87, 110`, `cse.rs:95, 241`). `dce::run`
   returns `()` (`crates/rue-cfg/src/opt/dce.rs:77`). The fold/propagate
   **fixpoint now lives inside `constopt`'s sparse worklist** (an instruction is
   revisited only when one of its operands becomes constant, RUE-794); the old
   outer `folded || propagated` loop in `opt::optimize` is gone. Passes are run
   once, in the fixed order `opt::optimize` sets.
2. **Structured work counters at the orchestration layer.** The session records
   pass work in `CfgConstructionWork`
   (`crates/rue-compiler/src/canonical_semantic.rs:282`) — fields like
   `optimization_attempts`, `optimization_completions`, `optimized_level_attempts`
   (lines 290–292) — aggregated across the parallel function map in
   `build_functions_and_cfgs` (`crates/rue-compiler/src/queries.rs:279-283`,
   `382-384`). This is the "wide events / structured completion" discipline
   AGENTS.md describes for `tracing`.

Loop passes should therefore **expose their own `Stats` struct** with
bounded-work counters (loops analyzed, invariants hoisted, loops unrolled,
budget-rejected) returned from `run`, and contribute into that structured
`CfgConstructionWork` surface.

## Decision

### 1. Loop analysis infrastructure

**Dominator algorithm: Cooper–Harvey–Kennedy (CHK) iterative — already
implemented.** The shared analysis already exists at
`crates/rue-cfg/src/dominators.rs` (RUE-914): `DominatorTree::compute` builds the
immediate-dominator array by exactly the CHK iterative dataflow method —
reverse-postorder the blocks, then iterate `idom` to a fixpoint using the
two-finger "intersect" walk over the RPO numbering, consuming
`Cfg::compute_predecessors()` (`crates/rue-cfg/src/inst.rs:1137`). Loop analysis
does **not** need to add a dominator implementation; it extends this module. The
algorithm choice below is therefore a record of what landed, not an open call.

Justification vs Lengauer–Tarjan (LT), as realized:

- **Scale.** Rue functions are small (the CFG is one function; blocks number in
  the tens, rarely hundreds). CHK's *observed* running time on real,
  reducible-heavy CFGs of this size is at or below LT's, with a fraction of the
  code and no auxiliary DFS-forest/semidominator bookkeeping. Cooper, Harvey, and
  Kennedy's own "A Simple, Fast Dominance Algorithm" is precisely this argument.
- **Simplicity and maintainability.** CHK is ~40 lines over an RPO and a
  predecessor list — both of which the CFG already yields — versus LT's
  bucket/eval/link machinery. For a first loop-analysis landing, the smaller,
  auditable implementation wins, especially since it is on the correctness path
  for a trap-sensitive pass.
- **LT's asymptotic edge (near-linear vs near-quadratic worst case) does not pay
  off** until functions are far larger than Rue produces; if that ever changes,
  swapping the internals behind a stable `Dominators` API is a localized change.

**Natural-loop detection.** From the dominator tree, a back edge is an edge
`u → h` where `h` dominates `u`; the natural loop of that back edge is `h` plus
all blocks that can reach `u` without passing through `h`. Loops sharing a header
are merged. This yields, per loop: header, back edges, body block set, and
(computed on demand) a **preheader**. Loop nesting is the containment forest over
body sets.

**Preheader as a dedicated block (not just "the outside predecessor").** LICM
hoists *into* the preheader, so the preheader must be a real block that dominates
the header and is guaranteed to execute exactly once per loop entry. The rule for
obtaining it is not simply "reuse the single non-back-edge predecessor":

- Partition the header's predecessors into **back-edge predecessors** (inside the
  loop, `h` dominates them) and **outside predecessors** (loop entries).
- If there is exactly one outside predecessor `p` **and `p`'s terminator targets
  the header unconditionally as its only successor**, `p` may serve as the
  preheader directly.
- **Otherwise insert a dedicated preheader block** `ph`: redirect every outside
  predecessor edge to `ph`, and give `ph` a single `Goto(h, ...)` terminator.
  This is required not only when the header has multiple outside predecessors,
  but **also when the single outside predecessor `p` has other successors** (its
  terminator is a `Branch`/`Switch` where the header is one arm) — hoisting into
  such a `p` would execute the hoisted ops on the sibling arm's path too, which
  is exactly the manufacture-work-on-an-untaken-path hazard §2 guards against.
  Splitting that edge gives a block that runs only on the header-entry path.
- **Header block-argument forwarding.** The CFG passes phi-like values as block
  arguments on edges (`BlockParam`, `crates/rue-cfg/src/inst.rs:278`). When a
  dedicated `ph` is inserted, the header's block parameters must be preserved:
  `ph` takes the same parameter list as the header carried for its outside
  edges, each redirected outside predecessor forwards its header arguments to
  `ph` instead, and `ph`'s `Goto(h, ...)` forwards them on to the header. Back
  edges keep passing their own arguments to the header unchanged. The result must
  satisfy `cfg.verify()` (see Open Questions).

**Where it lives, and reuse.** The dominator module is a standalone
`rue-cfg` module (`dominators.rs`), independent of `opt/`, and is **shared**:
value forwarding / copy propagation (RUE-914, `opt/forward.rs:317`) computes a
`DominatorTree` for its always-on check that a store's block dominates the loads
it forwards to, and the landed loop analysis (RUE-926) and LICM (RUE-927) are
its loop consumers; unrolling (RUE-928) becomes the next one. Putting dominators
in one shared place is the "one canonical computation path" principle from
AGENTS.md — realized here, not merely proposed; a second dominator
implementation would be exactly the duplicate-analysis smell that guidance warns
against, and the landed module already avoids it.

**Invalidation discipline: recompute per pass initially.** Dominators and loops
are **recomputed by each pass that needs them, not cached across passes.** Loop
passes mutate the CFG (LICM inserts a preheader and moves instructions;
unrolling clones blocks), which invalidates dominators; a cache would need
careful incremental maintenance that is not worth it at Rue's scale and is a
ready source of stale-analysis miscompiles. Recompute-per-pass is O(blocks) and
trivially correct. Caching within a *single* pass (compute once, use for all
loops in that function) is fine and expected; caching *across* passes is
deferred until profiling shows it matters.

### 2. THE central design problem — LICM trap-safety

Hoisting an operation out of a loop moves it from executing *once per iteration*
to executing *once in the preheader*, before the loop's entry test. If the loop
can execute **zero** iterations, a hoisted op runs on a path where the source
program never executed it. For a **trapping** op that is a catastrophe: it
**manufactures a trap out of thin air**, changing a program that exits 0 into one
that exits 101. This is the exact inverse of RUE-57 (where DCE *deleted* a
mandatory trap); here LICM would *invent* one. It is a direct violation of
ADR-0044's observable-behavior invariant, which counts Rue's overflow /
div-by-zero / bounds traps as *defined, observable behavior* that no level may
add or remove.

Rue's trapping ops are today enumerated inside DCE's private `has_side_effects`
(`crates/rue-cfg/src/opt/dce.rs:136`): `Add`, `Sub`, `Mul`, `Div`, `Mod`, `Neg`
(overflow / divide-by-zero checks, lines 176–181); **`IntCast`, which range-checks
and panics on an out-of-range narrowing conversion via `__rue_intcast_overflow`
(line 157, "IntCast can panic (range check), so it has side effects")**; and any
`PlaceRead` whose place contains an `Index` projection (bounds check, lines
166–169). Bitwise ops and shifts do **not** trap (shift counts are masked per
spec) and are pure. **`IntCast` must be in LICM's trapping set** — omitting it
(as an earlier draft did, listing only Add/Sub/Mul/Div/Mod/Neg + index reads)
would let LICM hoist an invariant narrowing cast into a zero-trip preheader and
manufacture an intcast panic.

**Factor a shared `may_trap` / `is_speculatable` classifier rather than reusing
DCE's private predicate.** DCE's `has_side_effects` conflates two distinct
properties: *observable side effect* (`Store`, `Call`, `Drop`,
`StorageLive`/`StorageDead`) and *may trap* (the arithmetic set, `IntCast`,
indexed `PlaceRead`). LICM needs the trap axis specifically — a pure-but-trapping
op is a hoist hazard, while a side-effecting non-trapping op is a different
concern — and unrolling's trap-on-iteration reasoning needs the same axis. Making
DCE's private `has_side_effects` the cross-pass authority couples three passes to
one function's incidental grouping and risks the two axes drifting apart. The
proposal: introduce a small shared classifier in `rue-cfg` (e.g.
`opt::classify` or an `inst`-level `CfgInstData::may_trap(&self, cfg) -> bool`
and companion `is_speculatable`) that names the trapping set **once**
— `Add`/`Sub`/`Mul`/`Div`/`Mod`/`Neg`, `IntCast`, indexed `PlaceRead` — and have
DCE, LICM, and unrolling all consult it. DCE's `has_side_effects` then becomes
`may_trap(v) || has_pure_side_effect(v)`, preserving its behavior exactly while
removing the private-authority coupling. This is the "one canonical computation
path" principle applied to trap classification.

**The rule set:**

1. **Hoist trap-free invariant ops freely.** An op is a LICM candidate if all its
   operands are loop-invariant (defined outside the loop or themselves hoisted)
   and the shared classifier reports it `is_speculatable` — i.e. **not**
   `may_trap` (arithmetic, `IntCast`, indexed `PlaceRead`) and with no observable
   side effect (`Call`, `Intrinsic`, `Store`, `PlaceWrite`, `Alloc`, `Drop`,
   `StorageLive`/`StorageDead`). Bitwise/shift/comparison/`Not`/`BitNot` on
   invariant operands are the sweet spot.
2. **Trapping invariant ops: do NOT hoist, initially.** Every `may_trap` op —
   `Add`/`Sub`/`Mul`/`Div`/`Mod`/`Neg`, `IntCast`, and indexed reads — stays in
   the loop body even when invariant, in the first version. This is
   unconditionally sound — it can never manufacture a trap — at the cost of
   leaving some invariant arithmetic un-hoisted.
3. **The named relaxation path (not in the initial pass): hoist a trapping
   invariant op only when the loop provably executes it at least once on every
   path through the preheader.** That proof needs loop-rotation / guard analysis:
   if the loop is rotated so the body is known to run at least once (a
   guarded/do-while shape, or a statically non-zero constant trip count), or the
   entry guard dominates and guarantees ≥1 iteration, then the op already
   executes on every real path and hoisting adds no new trap. Only under that
   proof may a trapping op move. This is the standard "hoist is safe if the op is
   guaranteed to execute" criterion; Rue defers it until loop rotation exists.

**Recommendation: ship rule (2) — never hoist trapping ops — first**, and name
(3) as the explicit relaxation once loop rotation/guard analysis lands. Rule (2)
plus rule (1) is a strict subset of a fully general LICM, is trivially inside the
invariant, and still captures the common trap-free invariants (address
computations built from bitwise/shift ops, invariant comparisons, invariant
boolean logic).

### 3. What LICM moves

- **Moves:** pure, side-effect-free ops (per §2 rule 1) whose operands are all
  loop-invariant, into the loop preheader, in dependency order.
- **Does NOT move loads.** `Load` / non-indexed `PlaceRead` are memory reads;
  hoisting one is only sound if no store inside the loop can change the memory it
  reads. Rue has **no memory-versioning / alias analysis** today, so LICM cannot
  prove a load invariant. Load hoisting is **copy-propagation / store-forwarding
  territory (RUE-914)**, not LICM's, and is explicitly out of scope here. (Indexed
  `PlaceRead` is additionally trapping, excluded by §2 anyway.)
- **Does NOT move calls.** `Call` and `Intrinsic` may have arbitrary side effects
  and may trap; they are never hoisted.

Keeping LICM to pure invariant compute — no memory, no calls — makes it sound
without any alias analysis, which is the right initial scope for a language whose
memory model for optimization is still being built out (ADR-0050 territory).

### 4. Unrolling

**Initial scope: constant-trip-count full unroll only, on a canonical loop
shape, under a size budget.** When a loop's trip count is a compile-time constant
`N` and the unrolled body fits the budget, replace the loop with `N` copies of
the loop body subgraph and delete the back edge.

**Canonical-shape restriction.** The initial unroller recognizes and rewrites
**only** loops in an explicit canonical form, and bails (leaves the loop intact)
on anything else:

- **Single header, single latch** — exactly one back edge into the header, from
  one latch block. Multiple back edges or irreducible entries are rejected.
- **Recognized induction variable** — one IV stepping by a compile-time-constant
  stride from a constant initial value to a constant bound, so the trip count
  `N` is statically computable. Recognition reads the IV off the CFG *after*
  `constopt` has folded and propagated the initializer and bound (§ Open
  Questions covers how much IV analysis is needed).
- **No unsupported exits** — the only loop exit is the header's trip test. A loop
  body containing a secondary exit (an early `break`/`return` edge leaving the
  body, or a trapping op that could abort mid-iteration in a way the copies must
  order correctly) is out of scope for the first version.

**Full CFG-subgraph cloning, not "straight-line copies."** A loop body is a
*subgraph* of basic blocks, not a single straight line, so each of the `N`
iterations is a **clone of the whole body block set** — new `BlockId`s, new
`CfgValue`s, side-arrays (`extra`, `call_args`, `switch_cases`, `projections`)
appended and re-based, and internal edges rewired to the clone's blocks (the same
index-rebasing discipline ADR-0049's splice uses). Iteration `k`'s exit edge
targets iteration `k+1`'s header-equivalent entry instead of looping back; the
final iteration falls through to the loop's original exit; the back edge is
deleted. Describing this as "straight-line copies" is only accurate for a
single-block body and understates the work for the general canonical case.

This is the safest unroll: no residual/remainder loop, no runtime trip-count
test, and — once the post-unroll cleanup below runs — it collapses the
now-constant induction variable.

**Post-unroll cleanup is mandatory (constopt runs *before* unrolling).**
`constopt` is the *first* pass in `opt::optimize` (`crates/rue-cfg/src/opt/mod.rs:160`),
running before `peephole`, `simplify`, `forward`, `cse`, and `dce`. A loop pass
gated at `-O3` runs *after* that whole sequence, so the constants and dead
control flow unrolling exposes — the per-iteration IV values, the now-dead trip
test, the collapsed cloned branches — are **not** cleaned up by the constopt that
already ran. Unrolling must therefore be followed by **another
const-fold/simplify/DCE sequence** over the affected function: re-run
`constopt::run` (to fold each clone's now-constant IV and propagate it),
`simplify::run` (to fold the dead trip-test branches into `Goto`s and thread the
cloned chain), and `dce::run` (to sweep the dead IV arithmetic and orphaned
blocks). Without this second sequence, unrolling produces larger, slower code
than the original loop. The cleanest placement is a small fixpoint or a fixed
second cleanup pass in the `-O3` arm after the loop passes; the exact shape is an
implementation detail, but the *requirement* is not optional.

- **Budget knob.** A single integer `unroll_budget` = maximum *total instruction
  count of the unrolled body* (`N × body_size`). Proposed `-O3` default: a modest
  cap (a few hundred CFG instructions) — enough to unroll small fixed loops,
  small enough to bound code growth. `-O0`/`-O1`/`-O2` = 0 (disabled). The count
  source is the same `cfg.values`-based instruction count LICM and inlining use.
- **Interaction with inlining's thresholds (ADR-0049).** Both unrolling and
  aggressive inlining are `-O3` code-growth transforms. They should share **one
  code-growth budget per function**, debited by whichever runs, so a function
  that inlines heavily does not *also* unroll into a size explosion (and vice
  versa). This note proposes a shared per-function growth budget as the
  coordination point; ADR-0049 Phase 3 is where the two meet. Absent
  coordination, the two passes can multiply code size.
- **Out of scope initially:** partial unrolling, runtime-trip-count unrolling with
  a remainder loop, and unroll-and-jam. Each needs a remainder-loop construction
  and (for trap-safety) the same guard reasoning as LICM §2.

### 5. Both passes — obligations

- **Differential coverage (ADR-0044).** Each pass lands with multi-case
  `differential_opt` CLI coverage (`run_case_differential`, RUE-236,
  `crates/rue-cli-tests/src/main.rs:802`) for the shapes it rewrites, and keeps
  the full differential set green. For LICM the **mandatory** cases are the
  trap-safety ones: a zero-iteration loop containing an invariant trapping op
  (overflow, div-by-zero, invariant index read) must **not** trap after LICM —
  this is the regression that rule §2 exists to prevent, and it must be pinned
  the way `opt.toml` pins RUE-57 trap survival. For unrolling: a constant-trip
  loop with a trap on a specific iteration must trap on that iteration
  identically unrolled vs not.
- **Work-counter discipline.** Each pass exposes a `Stats` struct of bounded-work
  counters returned from `run` (matching `constopt`/`simplify`/`cse`/`forward`/
  `peephole`; see §"Work-counter discipline") and contributes structured counters
  (loops analyzed, invariants hoisted, loops unrolled, budget-rejected) into the
  `CfgConstructionWork` / session work surface
  (`crates/rue-compiler/src/canonical_semantic.rs:282`,
  `crates/rue-compiler/src/queries.rs:279-283`), per AGENTS.md tracing guidance.
- **Placement.** Gated strictly above `-O2` in `opt::optimize` — today a new `O3`
  case split out of the shared `O1 | O2 | O3` arm (which already carries the
  `-O2`/`-O3`-gated forward/CSE passes), preserving ADR-0044's monotonic
  superset rule. The post-optimization structural recheck
  (`verify_after_optimization_with_type_pool`, `opt/mod.rs:206`) continues to run
  after, as the
  structural safety net for the block cloning and preheader insertion these
  passes perform.

## Implementation Phases

- [x] **Prerequisite (landed, RUE-914): CHK dominator tree** in
  `crates/rue-cfg/src/dominators.rs`, with unit tests; already consumed by copy
  propagation (`opt/forward.rs`).
- [x] **Phase 1: Natural-loop analysis** — natural-loop forest + preheader
  materialization built on the existing `DominatorTree`, with unit tests.
  (landed, RUE-926, July 2026)
- [x] **Phase 2: LICM (trap-free only)** — hoist pure invariant ops; never hoist
  trapping ops; differential trap-safety cases. `-O3`. (landed, RUE-927,
  July 2026)
- [x] **Phase 3: Constant-trip full unrolling** — canonical shape only (single
  header, single latch, recognized IV, no unsupported exits); full CFG-subgraph
  cloning; mandatory post-unroll const-fold/simplify/DCE cleanup; under the size
  budget; shared code-growth budget with ADR-0049 inlining. `-O3`. (landed,
  RUE-928, August 2026)
- [ ] **Phase 4 (relaxation): guarded trapping-op hoisting** — after loop
  rotation/guard analysis exists. (file RUE-934)

## Consequences

### Positive

- First target-independent natural-loop analysis, built on the dominator tree
  already shared by copy propagation (RUE-914) and reused by LICM and unrolling —
  one dominator implementation, not three.
- LICM (even trap-free-only) and constant-trip unrolling give `-O3` its intended
  speculative speed content per ADR-0044.
- The conservative trap rule makes the highest-risk loop transform provably safe
  from day one.

### Negative

- Trap-free-only LICM leaves invariant arithmetic un-hoisted until loop rotation
  lands (the relaxation path).
- Unrolling and inlining together can grow code; they must share a budget.
- Every loop-pass PR must grow the differential set (deliberate, per ADR-0044).

### Neutral

- No language-semantics change, no spec sections, no preview gate.
- `-O0`/`-O1`/`-O2` behavior unchanged; loop passes are `-O3`-only.

## Open Questions

- **Preheader insertion and `verify()`.** Inserting a preheader and cloning
  blocks must satisfy `cfg.verify()` (`crates/rue-cfg/src/verify.rs`); the exact
  block-param/terminator obligations for a synthesized preheader need a worked
  example before Phase 1.
- **Induction-variable recognition** for constant trip counts — how much IV
  analysis is needed, and how much constant folding/propagation (`constopt`) already exposes, before
  unrolling can read a constant `N` reliably.
- **Shared code-growth budget shape** with ADR-0049 — one budget debited by both
  passes, or independent caps with a combined ceiling? Resolve jointly with
  ADR-0049 Phase 3.
- **When (if ever) to cache dominators across passes** — deferred until profiling
  justifies the incremental-maintenance complexity.

## Future Work

- Guarded/loop-rotated hoisting of trapping invariant ops (the §2 relaxation).
- Load hoisting once store-forwarding / memory versioning (RUE-914) provides
  alias facts.
- Partial and runtime-trip-count unrolling with remainder loops; unroll-and-jam.
- Strength reduction of induction variables; loop fusion; later, vectorization
  (ADR-0044 Future Work).

## References

- [ADR-0044: Optimization Levels](0044-optimization-levels.md) — places loop opts
  at `-O3` and defines the observable-behavior invariant.
- [ADR-0012: Compiler Optimization Passes](0012-optimization-passes.md) — the CFG
  opt framework and the pass-shape convention.
- [ADR-0049: Function Inlining](0049-function-inlining.md) — the other `-O3`
  code-growth transform; shares the code-growth budget.
- Cooper, Harvey, Kennedy, "A Simple, Fast Dominance Algorithm" — the chosen
  dominator method.
- RUE-916 (this design), RUE-236 (differential-opt harness), RUE-57 (the
  mandatory-trap bug class LICM must not invert), RUE-914 (store-forwarding /
  copy-prop, which reuses the dominators and owns load hoisting).
