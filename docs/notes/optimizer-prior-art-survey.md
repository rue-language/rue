# Optimizer prior-art survey: Rue vs LLVM and GCC

Prompted by [RUE-1865](https://linear.app/steve-klabnik/issue/RUE-1865), which
observed that LICM materializes preheaders mid-sweep where both production
compilers establish them as a precondition. That is one instance of a broader
pattern, so this note surveys the whole `crates/rue-cfg/src/opt/` pipeline
against LLVM and GCC and records where the shapes differ, which differences
cost correctness, capability, speed, or allocations, and which are deliberate
and should stay.

## Method and status of the claims

* Rue claims are from current trunk source (`ef2163c`), cited by path and line.
* LLVM claims are from `llvm/lib/Transforms/Scalar/LICM.cpp`,
  `llvm/lib/Transforms/Utils/LoopSimplify.cpp`, and `llvm/docs/Passes.html` at
  `main`.
* GCC claims are from `gcc/tree-ssa-loop-im.cc`, `gcc/tree-ssa-loop.cc`, and
  `gcc/cfgloop.h` at `master`.
* **Nothing here is measured.** Every performance statement is a structural
  argument about asymptotics or allocation counts, not a profile. The items
  that claim a speed win are marked with what would have to be measured to
  confirm them. Rue's own measured optimizer numbers live in RUE-1765
  (verifier at 4.5% of a fresh Lattice compile) and RUE-1693 (9,555 dominator
  rebuilds in one compile); those are cited as evidence, not re-derived.

## The thesis

Nearly every difference below is a consequence of one structural choice.

**LLVM and GCC separate three things that Rue's passes each own privately:**

1. **Canonicalization** — putting the IR into a form later passes may assume
   (LLVM: `LoopSimplify`, `LCSSA`, `loop-rotate`; GCC: `loop_optimizer_init
   (LOOPS_NORMAL)`, which is `LOOPS_HAVE_PREHEADERS | LOOPS_HAVE_SIMPLE_LATCHES
   | LOOPS_HAVE_MARKED_IRREDUCIBLE_REGIONS`).
2. **Analysis** — dominators, loop forest, alias/memory information, held by a
   manager with an explicit preservation and invalidation contract.
3. **Transformation** — passes that consume 1 and 2 and declare what they
   preserve.

Rue fuses all three into each pass. `licm::run` computes its own dominator tree
and forest (`licm.rs:155-156`), canonicalizes loop form in the middle of
transforming (`licm.rs:410`), and then must throw its own analysis away and
start over (`licm.rs:188-191`). `unroll::run_with_budget` does the same thing
independently (`unroll.rs:53-58`). `forward::run` builds a third dominator tree
(`forward.rs:279`). The verifier builds a fourth (`verify.rs:451`).

ADR-0054 makes this explicit and deliberate — "Dominators and loops are
**recomputed by each pass that needs them, not cached**" — on the stated
grounds that a cache "would need invalidation discipline" and is "a ready
source of stale-analysis miscompiles," while recompute is "O(blocks) and
trivially correct at Rue's scale." That reasoning was sound when written. The
findings below are the accumulated cost of it now that there are two loop
passes, a verifier, and a forwarding pass all paying it, plus one case
(finding 1) where the recompute rule is what *creates* the staleness hazard it
was meant to avoid.

The prior-art answer is not "add a cache." It is that **canonicalization is
what lets a transform preserve an analysis**, which is what makes caching safe
rather than clever. LLVM's LICM cannot invalidate `LoopInfo`/`DomTree` because
`LoopSimplify` already gave it a preheader to hoist into, so it only ever moves
instructions. That is the same insight RUE-1865 reached independently.

## Findings, ranked

| # | Finding | Axis | Est. size |
| --- | --- | --- | --- |
| 1 | Loop form is established mid-transform, not as a precondition | speed, architecture | already filed: RUE-1865 |
| 2 | LICM's memory-invariance gate is whole-body, discarding the per-slot facts Rue already computes | capability | medium |
| 3 | The never-hoist-trapping rule is stricter than it needs to be, and loop rotation is not the prerequisite RUE-934 assumes | capability | medium |
| 4 | No memory-to-register promotion, which caps what every downstream pass can see | capability | large (RUE-917) |
| 5 | `ensure_preheader` still deep-clones and re-verifies the whole function per preheader | allocations, speed | small |
| 6 | LICM only hoists; LLVM's LICM also sinks and promotes | capability | medium |
| 7 | Unrolling refuses all nested loops outright | capability | medium |
| 8 | CSE is block-local where both compilers are dominator-scoped | capability | medium |
| 9 | Constant propagation is unconditional, not SCCP; the pipeline hand-rolls a one-round fixpoint instead | capability | medium |
| 10 | Analyses are rebuilt from scratch per pass with whole-function allocations | speed, allocations | medium |
| 11 | Full structural verification runs at boundaries where LLVM/GCC verify only under a debug flag | speed | already filed: RUE-1765 |

Findings 2 and 3 are the ones I would act on first: they are the difference
between LICM being a pass that fires on real Rue code and one that mostly
does not.

---

## 1. Loop form as precondition, not as a mid-transform edit

Already filed as RUE-1865 with the LLVM and GCC citations, so this section only
adds what the wider survey turned up.

**The same defect exists in `unroll`, and RUE-1865 does not cover it.**
`unroll::run_with_budget` (`unroll.rs:53-58`) has the identical restart loop:
recompute `DominatorTree` + `loops()`, transform, `break` on any change, start
over. It is worse there than in LICM, because unrolling clones blocks — so the
forest is genuinely invalidated, not merely suspected of being stale, and the
restart cost is paid per unrolled loop rather than per materialized preheader.

RUE-1865's proposed normalization step ("one normalization serves both") is
right, but the issue frames unroll as a beneficiary rather than a second site of
the same bug. Whoever takes RUE-1865 should be told to fix both, and the
acceptance criterion should include `unroll`'s `forest_computations` counter,
not just LICM's.

**A second detail worth carrying into that work.** LICM orders loops innermost-
first by sorting on body size (`licm.rs:167`,
`order.sort_by_key(|&id| forest.get(id).body.len())`). That is correct — an
inner loop's body is a proper subset of its parent's, so it always sorts first
— but the forest already carries `parent`, and both LLVM's loop pass manager
and GCC's `loop_optimizer_init` walk actual nesting order. Deriving nesting from
a size proxy when the real edge is in hand is the kind of thing that is correct
today and quietly wrong after someone changes what `body` contains. Cheap to
fix while the file is open.

## 2. LICM's memory gate throws away information Rue already has

**What Rue does.** A `Load`/`PlaceRead` is treated as non-invariant whenever
the loop body contains *any* observable-effect instruction other than the
storage markers (`licm.rs:448`, `licm.rs:470-480`). The module docs are candid
about why: "Rue has no memory-versioning / alias analysis today, so this phase
is maximally conservative."

**Why this is more expensive than it looks.** `classify::has_observable_side_effect`
counts `Alloc` (`classify.rs:115`), and `CfgBuilder` materializes *every* `let`
as an `Alloc` (documented in `constopt.rs:19-20`). So a loop body containing a
single local variable declaration disables memory-read hoisting for that entire
loop. Combined with finding 4 — every variable use is a `Load` — the realistic
situation is that LICM's memory path never fires on ordinary Rue code, and what
survives is hoisting of pure arithmetic over already-SSA values, which finding 3
then blocks for anything that can trap.

**What LLVM does.** `MemorySSA` plus `AliasAnalysis`; LICM is a client, and its
`getAnalysisUsage` lists `MemorySSA` as both required and preserved.

**What GCC does.** `tree-ssa-loop-im.cc` builds `im_mem_ref` records carrying an
`ao_ref` with base, offset, and size, tracks `stored`/`loaded` bitmaps per
reference, and asks `ref_indep_loop_p` whether a specific reference is
independent of the loop — with distinct queries for RAW, WAR, and WAW
(`lim_raw`, `sm_war`, `sm_waw`). The disambiguation is per-reference, never
per-body.

**The recommendation.** Rue does not need general alias analysis to close most
of this gap, because Rue's locals are numbered slots and its whole-slot writes
name their slot directly. `slot_facts.rs` already computes exactly the required
per-slot write and escape classification — "exactly ONE whole-slot write,"
projected `PlaceWrite`s, by-ref call arguments, address-taken slots — and is
already shared by `constopt` and `forward` precisely so this discipline is
stated once. LICM is the third consumer that needs it and is the one pass not
using it.

Concretely: replace `body_has_memory_effect` with a per-slot query. A
`Load { slot }` is invariant when no in-loop instruction writes that slot, no
in-loop instruction escapes it, and no in-loop `Call`/`Intrinsic` could reach
it (the last clause is where the conservatism should live, and it is one
predicate, not a whole-body veto). An `Alloc` of slot B stops killing a load of
slot A. This is the single highest-leverage change in the survey relative to its
size, and it reuses infrastructure rather than adding any.

The one caveat: `slot_facts` currently answers a whole-function question
("does this slot have exactly one write anywhere"). LICM needs a loop-scoped
one ("is this slot written *inside this loop*"). That is a genuine extension of
the module, not a free call, and it should be added there rather than
reimplemented in `licm.rs` — the module's entire stated purpose is that this
knowledge lives in one place.

## 3. The trapping rule, and why loop rotation may not be the prerequisite

**What Rue does.** Trapping invariant ops never move, absolutely
(`licm.rs:14-19`, `licm.rs:439`). ADR-0054 Phase 4 and
[RUE-934](https://linear.app/steve-klabnik/issue/RUE-934) record the intended
relaxation and state its blocker: hoisting a trapping op requires proving the
body "provably executes at least once on every path reaching the preheader —
requires loop rotation and/or guard analysis that does not exist yet."

**What LLVM does.** From `LICM.cpp`: "Only sink or hoist an instruction if it is
not a trapping instruction, or if the instruction is known not to trap when
moved to the preheader, or if it is a trapping instruction and is guaranteed to
execute." The two predicates are `isSafeToSpeculativelyExecute` and
`SafetyInfo->isGuaranteedToExecute(Inst, DT, CurLoop)`.

**What GCC does — and this is the part that matters.** `movement_possibility`
returns a *three-valued* result: `MOVE_POSSIBLE`, `MOVE_IMPOSSIBLE`, and
`MOVE_PRESERVE_EXECUTION` — the last meaning "possible to hoist, but we must
avoid making it executed if it would not be executed in the original program
(e.g. because it may trap)." `determine_max_movement` then handles that case by
setting `level = ALWAYS_EXECUTED_IN(bb)` instead of the outermost superloop.

**`ALWAYS_EXECUTED_IN` is not loop rotation.** It is a per-block property
computed over the dominator tree Rue already builds: the outermost loop in which
that block runs on every iteration that begins. And the soundness argument
composes with a preheader trivially:

> The preheader's only successor is the header. So executing the preheader
> implies executing the header, which implies executing any block always-executed
> in the loop. A trapping op there traps in the original program too, on the
> first iteration.

Rue's `ensure_preheader` already guarantees the premise. `loops.rs:27-31` splits
the edge "**also when the single outside predecessor has other successors**,"
precisely so the preheader's only successor is the header. So the structural
precondition for GCC's mechanism is *already met in Rue today*.

The degenerate case needs no analysis at all: **an instruction in the loop
header always executes when the header does.** A trapping invariant instruction
in the header can be hoisted to the preheader with no new infrastructure
whatsoever.

**The complication Rue has that LLVM does not, and that GCC mostly dodges.**
Rue's traps are defined panics with locations, not UB. So it is not enough to
preserve *whether* a trap happens; a hoist must not change *which* trap happens
first. If a header contains `a / b` followed by `c + d` and both would trap,
hoisting only the division reverses the reported panic. LLVM's poison/UB model
makes this a non-question; GCC's `could_trap_p` set is dominated by memory
references where ordering is not observable, so it does not bite there either.

For Rue the rule therefore needs an ordering clause that neither compiler needs:
a trapping instruction may hoist only if every trapping instruction preceding
it on the path from header entry is also hoisted, in the same relative order.
Within a single block that is a simple prefix condition on the instruction list.

**Recommendation.** RUE-934 should be re-scoped. Its stated blocker — loop
rotation — is not required for the always-executed formulation, and the issue's
framing may be why it has sat in Backlog. Two follow-ups are worth filing
against it: the header-only degenerate case (small, no new analysis), and the
general `always_executed_in` computation modelled on GCC (medium). Both need
the trap-ordering clause above, which is new design work and should be written
down before either is implemented. I would not implement this from the survey
alone; it wants an ADR-0054 amendment.

## 4. No memory-to-register promotion

**What Rue does.** Nothing. `grep` for `mem2reg`, `promote`, or `SROA` across
`crates/` returns no promotion pass. Every `let` is an `Alloc` and every use a
`Load` (`constopt.rs:19-20`). [RUE-917](https://linear.app/steve-klabnik/issue/RUE-917)
tracks this as "mem2reg-lite" and is in Backlog; its own description already
calls it "the structurally biggest optimizer win available."

**What LLVM and GCC do.** LLVM runs `SROA`/`mem2reg` early — it is the pass that
creates SSA form for locals in the first place. GCC's into-SSA promotes every
non-address-taken local to a gimple register. In both, by the time any loop pass
runs, scalar locals are values, not memory.

**Why it belongs in a survey about LICM.** Findings 2, 6, 8, and 9 all describe
passes that are weaker than their counterparts because they reason about memory
where LLVM and GCC reason about values. Rue's CFG already has block parameters
as a phi mechanism, so the mechanism promotion needs exists. This is not a LICM
fix, but it is the reason several LICM fixes have a low ceiling. Worth saying
plainly on RUE-917: the loop optimizer's payoff is gated on it.

## 5. `ensure_preheader` still deep-clones and re-verifies the whole function

**What Rue does.** `ensure_preheader_transaction` clones the entire `Cfg`
(`loops.rs:452`) and runs a full-graph structural verification
(`loops.rs:473`) for each materialized preheader, purely so a mid-edit payload
failure leaves the original untouched.

**Why this is a leftover rather than a design.** RUE-1663 removed exactly this
clone class from `peephole`, `cse`, `forward`, and `simplify`; RUE-1842 removed
it from `unroll`, with the reasoning recorded at `unroll.rs:110-116`: the `Err`
propagates through `optimize_with_budget`'s `?` into `publish_optimization`,
"whose first statement is `pass_result?`" — so the preserved original is never
read. That argument applies verbatim here. RUE-1663 did touch this function, but
only its no-mutation reuse path; the materialization path kept both the clone
and the verification. `opt/mod.rs:479-524` has guard tests pinning the
no-clone property for the five other passes and does not watch this one.

**What LLVM and GCC do.** Neither takes a transactional copy of a function to
insert a preheader. `InsertPreheaderForLoop` edits in place and updates the
`DomTree` incrementally via `DomTreeUpdater`. GCC's `create_preheaders` splits
edges in place.

**Recommendation.** Small and well-precedented: drop the clone, keep the
verification behind `debug_assertions`, and extend the `opt/mod.rs` guard test
to cover `loops.rs`. Worth filing.

## 6. LICM hoists but never sinks or promotes

**What Rue does.** Hoisting only — `grep -i sink` over `opt/` returns nothing.

**What LLVM does.** From `Passes.html`: LICM removes code from the loop body "by
either hoisting code into the preheader block, **or by sinking code to the exit
blocks if it is safe**." And from `LICM.cpp`: "This pass also promotes
must-aliased memory locations in the loop to live in registers, thus hoisting
and sinking 'invariant' loads and stores." Sinking walks the dominator tree in
reverse depth-first order ("visit uses before definitions"); hoisting walks it
forward ("visit definitions before uses").

**What GCC does.** Store motion via `execute_sm`, with `execute_sm_if_changed`
conditionally sinking stores to loop exits behind a flag variable.

**Note on Rue's hoisting order.** Rue reaches the same dependency ordering LLVM
gets from its DT preorder walk, but via an explicit def-use worklist
(`licm.rs:360-400`). That is a legitimate alternative, not a defect — and it is
arguably better suited to Rue, whose block ordering carries no dominance
guarantee. Keep it.

**Recommendation.** Sinking is a real gap but is worth much less than findings 2
and 3 until finding 4 lands — there is little to sink when everything is a
`Load` from a slot. File it, rank it below them.

## 7. Unrolling refuses every nested loop

**What Rue does.** `unroll.rs:67` skips any loop with a parent or a child, with
the comment that nested bodies contain "a second induction protocol with no
canonical order."

**What LLVM and GCC do.** Both unroll innermost loops inside outer loops
routinely; the innermost loop is the *normal* unrolling target. LLVM's
`LoopUnrollPass` runs as a loop pass over the whole nest. Neither refuses a loop
for having a parent.

The refusal on `parent.is_some()` is the questionable half. Refusing to unroll a
loop that *contains* another loop is a defensible size and complexity call.
Refusing to unroll an innermost loop *because it sits inside another one* rules
out the single most valuable case in both production compilers, and the stated
reason — a second induction protocol in the body — does not apply to a loop
whose body contains no loop.

**Recommendation.** Worth filing on its own. Dropping just the
`lp.parent.is_some()` clause, keeping the has-children refusal, looks like a
small change with real reach. Would need oracle-diff to confirm.

## 8. CSE is block-local

**What Rue does.** "One forward walk of each block's instructions, with a
value-number table reset per block. The walk is strictly block-local, so no
dominance analysis is needed" (`cse.rs:10-16`).

**What LLVM does.** `EarlyCSE` uses a scoped hash table over the dominator tree
— the same one-walk structure, but the table is scoped rather than cleared, so
an expression computed in a dominating block is available in every dominated
one. `GVN` then handles full and partial redundancy.

**What GCC does.** `tree-ssa-dom` (dominator-based) and FRE/PRE.

The upgrade from Rue's shape to LLVM's `EarlyCSE` shape is genuinely small: the
walk becomes a DT preorder traversal and the table becomes a scoped one that
pushes and pops per node instead of clearing. Rue's `DominatorTree` already
provides preorder numbering with subtree intervals (`dominators.rs:41-44`), so
the scoping is available. Worth filing; note in the issue that it is a change of
traversal, not a new analysis.

## 9. Constant propagation is unconditional, and the pipeline hand-rolls the fixpoint

**Two related differences.**

**(a) Not SCCP.** `constopt` is a sparse worklist but has, by its own comment,
"no reachability filter" (`constopt.rs:70-71`), and neither `constopt` nor
`constfold` mentions `BlockParam`. So Rue does not propagate constants through
block parameters — its phis — and does not exclude values arriving on
provably-unreachable edges. LLVM's `SCCP` "assumes values are constant unless
proven otherwise" and interleaves constant lattice with reachability, which is
what lets it fold a phi that is constant on all *reachable* inputs; GCC's
`tree-ssa-ccp` is likewise conditional. Rue instead approximates by iterating
separate passes: `constopt` → `simplify` folds the branch → re-run. This is the
textbook case where the iterated formulation is strictly weaker than the
combined lattice, and no amount of iteration closes the gap.

**(b) The fixpoint is a hardcoded single extra round.** `opt/mod.rs:378-383`
re-runs `dce → constopt → peephole → simplify` if the first `simplify` folded
anything, and `opt/mod.rs:399-404` re-runs `constopt → simplify` if forwarding
fired. Each is exactly one extra round, chosen by hand, keyed on a stats field.
LLVM and GCC both express repetition declaratively — a pass pipeline with
explicit repeat counts, not a conditional re-invocation keyed on a previous
pass's counters.

The current arrangement is not wrong, but it is unusually load-bearing for how
implicit it is: the correctness of drop-flag elimination depends on a specific
re-run being triggered by a specific counter being non-zero, and nothing tests
that the round count is sufficient rather than merely sufficient-so-far.

**Recommendation.** (a) is a real capability gap and a well-understood
transform; worth filing as an ADR-scale item, and it interacts with finding 4
(promotion is what makes block parameters carry interesting values in the first
place). (b) I would file as a smaller note: at minimum make the re-run
condition and its bound explicit, rather than a bare `if stats.x > 0`.

## 10. Analyses are rebuilt from scratch, with whole-function allocations

**Rebuild count.** In a single `-O3` function compile: `forward` builds a
dominator tree (`forward.rs:279`), LICM builds one per sweep (`licm.rs:155`),
`unroll` builds one per sweep (`unroll.rs:55`), `ensure_preheader`'s
verification builds one (`verify.rs:451`) per materialized preheader, and the
final `finish_after_optimization` builds one more. RUE-1693 measured 9,555
dominator rebuilds in one fresh Lattice compile.

**Allocation shape.** `loops()` allocates a `Vec<Vec<BlockId>>` of successor
lists — a separate heap allocation per block — on every call (`loops.rs:173`).
RUE-1843 fixed the equivalent problem inside LICM by introducing a shared
`HoistWorkspace` reset per loop rather than reallocated (`licm.rs:244-259`); the
same treatment has not reached `loops()`.

**What LLVM and GCC do.** LLVM's analysis managers cache `DominatorTreeAnalysis`
and `LoopAnalysis` and invalidate on an explicit `PreservedAnalyses` contract;
transforms that only move instructions declare both preserved and pay nothing.
GCC uses `calculate_dominance_info` / `free_dominance_info` with explicit
validity state, plus `loops_state_satisfies_p` assertions, and updates
dominators incrementally where a pass can. Both allocate analysis scratch from
arenas or small-size-optimized containers sized by the working set, not by the
function.

**Recommendation.** Do *not* propose a general analysis cache in isolation —
ADR-0054's objection to unguarded caching is correct, and a cache without
finding 1 is exactly the stale-analysis hazard the ADR warns about. The right
order is: land RUE-1865's canonicalization first, at which point LICM provably
only moves instructions and *can* declare the forest preserved; then a cache
becomes a bookkeeping change rather than a correctness argument. Sequence the
issues that way explicitly.

`loops()`'s per-block `Vec` allocation is independent of all that and can be
flattened now (CSR-style offsets + a single backing `Vec`), exactly as RUE-1693
did for the dominator tree's adjacency. Small, worth filing.

## 11. Verification at every boundary

Already filed as RUE-1765. Recording the prior-art comparison for that issue's
benefit: LLVM runs its verifier once on module entry and otherwise only under
`-verify-each` or `EXPENSIVE_CHECKS`; GCC's `verify_ssa` / `verify_loop_structure`
run under `ENABLE_CHECKING`. Neither verifies after every pass in a release
compiler. Rue runs full structural verification at `Cfg::finish`,
`finish_after_optimization`, and per materialized preheader (finding 5) in all
builds.

Rue's choice here is more defensible than the raw comparison suggests — it is a
young compiler with a planted-miscompile suite (RUE-1816) and a lot to lose from
a silent codegen bug. But "always on, in release, at every boundary" is a
stronger position than either production compiler holds, and the 4.5% is what it
costs.

## What Rue does differently and should keep

Not every divergence is a defect. These are deliberate and, on the evidence,
correct:

* **Refusing irreducible control flow outright** (`loops.rs:12-16`) rather than
  guessing a body. GCC marks irreducible regions
  (`LOOPS_HAVE_MARKED_IRREDUCIBLE_REGIONS`) and works around them; refusing is
  simpler and Rue's frontend cannot produce irreducible CFGs from source anyway.
* **The `may_trap` / `has_observable_side_effect` split** (`classify.rs`). LLVM
  reaches the same place with `isSafeToSpeculativelyExecute` +
  `mayHaveSideEffects`, GCC with `gimple_could_trap_p` + `gimple_has_side_effects`.
  Rue naming both axes in one shared module, with the ADR forbidding either pass
  from restating them, is *better* factored than either — DCE's private
  `has_side_effects` conflating them is precisely the historical bug RUE-925
  fixed.
* **Trap-exactness as a hard rule** in `peephole` — refusing `x * 2^k → x << k`
  because multiplication traps and shifts do not (`peephole.rs:10-13`). LLVM
  would do this rewrite freely under its UB model. Rue's defined-panic semantics
  make the refusal correct, and it is the same reasoning that makes finding 3's
  ordering clause necessary.
* **The def-use worklist in LICM discovery** (finding 6, above).
* **Bounded-work counters on every pass** (the RUE-794 discipline). Neither LLVM
  nor GCC has an equivalent structural guard against a pass going quadratic;
  RUE-1300 and RUE-1176 are both cases where it caught one.

## Suggested issue set

Ordered by value per unit of work. None of these are filed yet except where
noted.

1. **LICM per-slot memory invariance via `slot_facts`** (finding 2) — medium,
   highest ceiling, reuses existing infrastructure.
2. **Extend RUE-1865 to cover `unroll`'s identical restart loop** (finding 1) —
   comment on the existing issue rather than a new one.
3. **Drop `ensure_preheader`'s transactional clone and release-mode
   verification** (finding 5) — small, directly precedented by RUE-1663/1842,
   extend the `opt/mod.rs` guard test.
4. **Re-scope RUE-934 around `always_executed_in` rather than loop rotation**
   (finding 3) — comment on the existing issue; wants an ADR-0054 amendment
   before implementation, because of the trap-ordering clause.
5. **Allow unrolling of innermost loops that have a parent** (finding 7) —
   small, gated on oracle-diff.
6. **Flatten `loops()`'s per-block successor `Vec`s** (finding 10) — small,
   precedented by RUE-1693.
7. **Dominator-scoped CSE** (finding 8) — medium, a traversal change.
8. **Make the pipeline's conditional re-runs explicit** (finding 9b) — small.
9. **LICM sinking** (finding 6) — medium, low value until RUE-917.
10. **SCCP** (finding 9a) — ADR-scale.
11. **Note on RUE-917** that the loop optimizer's ceiling is gated on it
    (finding 4).

## Sources

* [LLVM Passes reference](https://llvm.org/docs/Passes.html)
* [`llvm/lib/Transforms/Scalar/LICM.cpp`](https://github.com/llvm/llvm-project/blob/main/llvm/lib/Transforms/Scalar/LICM.cpp)
* [`llvm/lib/Transforms/Utils/LoopSimplify.cpp`](https://github.com/llvm/llvm-project/blob/main/llvm/lib/Transforms/Utils/LoopSimplify.cpp)
* [`gcc/tree-ssa-loop-im.cc`](https://github.com/gcc-mirror/gcc/blob/master/gcc/tree-ssa-loop-im.cc)
* [`gcc/tree-ssa-loop.cc`](https://github.com/gcc-mirror/gcc/blob/master/gcc/tree-ssa-loop.cc)
* [`gcc/cfgloop.h`](https://github.com/gcc-mirror/gcc/blob/master/gcc/cfgloop.h)
