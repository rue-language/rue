---
id: 0044
title: Optimization Levels (-O0/-O1/-O2/-O3)
status: accepted
tags: [compiler, codegen, process]
feature-flag: none
created: 2026-07-03
accepted: 2026-07-03
implemented:
spec-sections: []
superseded-by:
---

<!-- Note: Optimization levels are a compiler-internal policy, not a language
     feature. They never change what a program means, only how fast/small the
     emitted code is (see the Invariant below). No preview gate applies. -->

# ADR-0044: Optimization Levels (-O0/-O1/-O2/-O3)

## Status

Accepted

Steve endorsed the straw-man direction on RUE-245 ("sounds pretty good to me
at the moment, and what i'd expect, but look into it"). This ADR documents and
refines that direction after surveying how mainstream compilers assign passes
to levels. It is a **policy/plan document**: it defines the contract each level
promises and the pass-to-level mapping, but implements nothing. The
implementation tasks (adding new passes and slotting them into levels) follow
as separate Linear issues.

## Summary

Rue exposes the conventional `-O0`/`-O1`/`-O2`/`-O3` optimization levels. This
ADR fixes what each level *promises* to a user (its cost/benefit contract) and
which passes populate it, so that each level has an explicit current contract
and future additions have a documented home. The single non-negotiable rule across
every level is the **observable-behavior invariant**: for any program with
defined behavior, exit code and I/O are byte-for-byte identical at `-O0`,
`-O1`, `-O2`, and `-O3`. Levels may trade compile time, code size, and run time
against each other; they may never change what a program computes. That
invariant is mechanically enforced by the RUE-236 differential-opt harness,
which is the prerequisite safety net for ever giving the higher levels
distinct content.

## Context

Today the Rue compiler accepts `-O0` through `-O3` (`OptLevel` in
`crates/rue-cfg/src/opt/mod.rs`, driven by `-O<level>` in `crates/rue/src/main.rs`),
but the mapping is:

| Level | Today's behavior |
|-------|------------------|
| `-O0` | Nothing. CFG passes straight to lowering (plus the always-on structural `verify()`). |
| `-O1` | Constant folding ⇄ store-to-load constant propagation to a fixpoint, then dead-code elimination. |
| `-O2` | `-O1` plus the conservative whole-program free-function inlining batch (RUE-930) and Phase 5 reachability elimination (RUE-933). |
| `-O3` | `-O2` plus trap-free LICM (RUE-927), bounded constant-trip unrolling (RUE-928), and larger-cap bounded non-leaf free-function inlining (RUE-931); guarded trapping-op hoisting remains planned. |

The levels now have distinct behavior: `-O2`/`-O3` run the compiler's
conservative whole-program inlining and reachability batch, and `-O3` adds the
landed LICM, constant-trip unrolling, and larger-cap bounded non-leaf inlining
passes. Guarded trapping-op hoisting remains future work. RUE-245 asks us to
*deliberately decide*
what belongs at each level rather than leave the top two as accidental
synonyms — both so users get the contract they expect from `-O` flags, and so
that further passes already have a documented home.

### The current pipeline (ground truth)

The optimizer lives entirely in `crates/rue-cfg/src/opt/` and runs as
CFG → CFG transforms between CFG construction and MIR lowering.
`optimize_with_budget()` is the single choke point every build funnels through;
the compiler batch later applies general inlining/DFE and calls it again for
changed callers:

```
AIR -> CfgBuilder -> CFG -> [opt::optimize_with_budget(level)] -> [whole-program inline/DFE batch] -> [changed-caller optimize_with_budget] -> CfgLower -> MIR -> RegAlloc -> Emit
```

The passes that exist:

- **`constfold.rs`** — folds arithmetic/comparison/bitwise/unary ops on
  `Const` operands (with overflow and div-by-zero guards). Complements the
  RIR-level `try_evaluate_const` from ADR-0003.
- **`constopt.rs`** — sparse store-to-load constant propagation and constant
  folding for single-assignment local slots, revisiting dependent values to an
  internal fixpoint (RUE-794).
- **`dce.rs`** — liveness-based dead-code elimination: drops unused values and
  unreachable blocks, preserving side-effecting instructions (calls, stores,
  intrinsics, drops).

`cfg.verify()` (RUE-227) runs at **every** level, `-O0` included — it is a
correctness check, not an optimization, so it is outside the level contract.

### Why this is timely

The RUE-236 differential-opt harness (landed, PR #1092) runs a marked subset of
CLI cases across `["-O0", "-O1", "-O2", "-O3"]` and asserts identical exit code
and stdout at every level (`run_case_differential` in
`crates/rue-cli-tests/src/main.rs`, gated by `differential_opt = true`). The
harness was established before the higher levels had distinct passes; it now
guards the live O2/O3 inlining and reachability batch as well as the local
transforms. The `rue-cfg` review exercised exactly this net when it caught the
RUE-348 enum-fold miscompile.

## Prior art: how mainstream compilers assign passes to levels

The industry consensus is remarkably uniform. The levels are a *cost ladder*:
each rung buys more run-time performance (or smaller size) with more compile
time and, from `-O2` up, more divergence between emitted code and source
structure.

**GCC** ([Optimize-Options](https://gcc.gnu.org/onlinedocs/gcc/Optimize-Options.html)):

- `-O0`: no optimization; fastest compile, best debugging. Default.
- `-O1`: the cheap, almost-always-worth-it set — basic DCE/DSE, simple inlining,
  jump/branch cleanups, if-conversion. Reduces code *and* compile time vs naive.
- `-O2`: the recommended release level — turns on nearly all optimizations that
  do **not** involve a space/speed tradeoff: global CSE, loop strength
  reduction, more inlining, and (in modern GCC) vectorization.
- `-O3`: `-O2` plus the transforms that may **grow code** to chase speed —
  aggressive `-finline-functions`, `-frename-registers`, heavier loop work.
- `-Os`: `-O2` minus anything that tends to increase size; `-Oz`: size at all
  costs.

**Clang / LLVM** ([Clang User Manual](https://clang.llvm.org/docs/UsersManual.html)):
Same ladder. `-O1` = quick, keeps most debug info; `-O2` = "all optimization"
(the balanced default most software ships with); `-O3` = `-O2` plus transforms
that take longer or **generate larger code** to run faster — chiefly aggressive
loop unrolling, more inlining, and auto-vectorization. `-Os`/`-Oz` optimize for
size.

**rustc** ([rustc codegen options](https://doc.rust-lang.org/rustc/codegen-options/index.html)):
Feeds LLVM's pass-manager builder. `opt-level` 0/1/2/3 plus `s`/`z`. Both `2`
and `3` optimize for speed at the expense of size; **`3` does more
vectorization and inlining than `2`**. Loops are unrolled at `>= 2`. `s`/`z`
sharply lower the inline threshold to shrink binaries (`z` smaller than `s`).

**Zig** ([Zig overview](https://ziglang.org/learn/overview/)): orthogonal axes
rather than a single ladder — `Debug` (no opt, all safety checks),
`ReleaseSafe` (optimized, safety checks retained), `ReleaseFast` (all major
opts — inlining, vectorization, unrolling — safety checks off), `ReleaseSmall`
(size). The instructive part for Rue is that Zig makes the *safety* dimension
explicit and separate from the *optimization* dimension.

### The consensus, distilled

- **`-O0`** = no optimization; the debugging / fast-compile baseline; the level
  whose codegen tracks source structure one-to-one.
- **`-O1`** = the cheap, always-safe, compile-time-positive set: local folding,
  propagation, DCE, trivial cleanups. Never a space/speed gamble.
- **`-O2`** = the release default: the full set of *balanced* optimizations that
  reliably help without blowing up code size — CSE, peephole, better regalloc,
  conservative inlining. This is the level real software ships at.
- **`-O3`** = `-O2` plus the *speculative* transforms that spend code size and
  compile time to chase speed — bounded larger-cap non-leaf inlining, loop
  unrolling, and (eventually) vectorization — with no guarantee of a win on
  every program.
- The size levels (`-Os`/`-Oz`) are a *separate* axis; Rue does not have them
  yet (see Future Work).

## Decision

### The invariant (applies to all levels)

> **For any Rue program whose behavior is defined, the observable behavior —
> process exit code and all I/O — is identical at `-O0`, `-O1`, `-O2`, and
> `-O3`.**

Optimization is permitted to change only *non-observable* properties: compile
time, binary size, instruction count/selection, register allocation, and run
time. A pass that can change exit code or I/O for a well-defined program is a
**miscompile**, not an optimization, regardless of level.

Two clarifications that keep the invariant honest:

1. **Undefined behavior is exempt.** A program that already has UB (e.g. reads
   an uninitialized slot, overflows where overflow is UB) has no guaranteed
   behavior to preserve, so levels may differ there. The invariant is a promise
   about *defined* programs only — the same carve-out every optimizing compiler
   makes.
2. **Checked-arithmetic semantics are part of "defined behavior."** Rue's
   overflow/div-by-zero traps are observable (they set the exit status), so no
   level may optimize them away or let a fold silently wrap. `constfold` already
   guards these; any future pass inherits the obligation. This is precisely the
   class RUE-348 lived in.
3. **Bounds *traps* are observable; bounds *checks* are not.** Spec 8.2:5 states
   the obligation as observable behavior — an out-of-range access traps as if
   tested at the point of navigation — rather than as a mandated instruction, so
   a pass may elide, combine, or hoist the dynamic test for an access it proves
   in range. Spec 8.2:9 fixes the limits: a transformation may not introduce a
   trap on an execution that would not have trapped (including speculating a
   check onto an untaken path, or letting a hoisted check fire for a zero-trip
   loop), may not remove a trap the program would take, may not reorder a trap
   across an observable effect, and must preserve which access traps first. Any
   range-analysis or bounds-check-elimination pass cites 8.2:9 and ships with
   differential coverage for these cases.

The invariant is not merely aspirational: it is **mechanically enforced** by the
RUE-236 differential-opt harness. Every pass we add at `-O2`/`-O3` must keep the
marked differential set green, and the marked set must grow to cover the code
shapes each new pass targets (see Sequencing).

### Level contract for Rue

| Level | Contract | Passes (today) | Passes (planned home) |
|-------|----------|----------------|-----------------------|
| `-O0` | No optimization. Codegen tracks source; fastest compile; best debugging. **Default.** | none (only always-on `verify()`) | stays empty |
| `-O1` | Cheap, always-safe, compile-time-neutral-or-positive local cleanups. | sparse `constopt` (RUE-794) → peephole (RUE-912) → simplify-cfg (RUE-910/911) → DCE | complete for now |
| `-O2` | **Release default.** All balanced optimizations that reliably help without a size blow-up. Superset of `-O1`. | `-O1` + copy propagation / store-to-load forwarding (RUE-914) → block-local CSE (RUE-913) → conservative whole-program inlining and Phase 5 reachability (RUE-930, RUE-933) | wider GVN; better regalloc |
| `-O3` | `-O2` plus speculative, size-spending, speed-chasing transforms. Superset of `-O2`. | current O2 batch + trap-free LICM (RUE-927) + bounded constant-trip unrolling (RUE-928) + larger-cap bounded non-leaf inlining (RUE-931) | broader/profile-guided inlining; guarded trapping-op hoisting (RUE-934); later, vectorization |

Rules that make the table a *contract* rather than a wishlist:

- **Monotonic supersets.** `-O3 ⊇ -O2 ⊇ -O1 ⊇ -O0`. A higher level never *drops*
  a lower level's pass; it only adds. This is the property `OptLevel::O1 |
  OptLevel::O2 | OptLevel::O3` already encodes and that new passes must respect.
- **`-O0` means `-O0`.** The default stays genuinely unoptimized so `-O0` remains
  the reliable debugging baseline and the differential oracle's ground truth.
- **`-O2` is the recommendation.** When Rue documents a "release build", it means
  `-O2`. `-O3` is opt-in for users who measured a win.
- **A pass's level is set by its cost/risk, not its cleverness.** Cheap + always-a-win
  → `-O1`. Reliable-win-but-not-free → `-O2`. Might-grow-code / might-not-pay-off
  → `-O3`. This is the same triage GCC/LLVM/rustc use.

### How current passes map

The local passes (`constopt`, `dce`) remain the cheap,
always-safe set at **`-O1`**. O2/O3 also run the canonical whole-program batch
owned by `rue-compiler`, which performs conservative free-function inlining and
post-inline reachability elimination while preserving O0/O1 source-faithful
behavior. O3's larger-cap non-leaf inlining and constant-trip unrolling are
bounded by their shared per-function growth budget; future profile-guided
extensions remain separate policy decisions.

### How plausible future passes map

These are *illustrative placements*, not commitments — each lands via its own
Linear issue and ADR-if-warranted, and each must ship with differential
coverage. Placement follows the cost/risk triage above:

- **Peephole / strength reduction** (`x*2 → x<<1`, `x/const` → mul-shift):
  cheap, local, always-a-win → **`-O1`**.
- **CFG simplification** (empty-block removal, jump threading, block merging):
  cheap, enables later passes → **`-O1`**.
- **Common subexpression elimination / GVN**: reliable win, moderate cost →
  **`-O2`**.
- **Copy propagation, more aggressive DCE**: **`-O2`**.
- **Conservative inlining** (small / single-use / leaf functions): reliable win,
  modest size cost → **`-O2`**.
- **Register-allocation quality improvements** (better spilling, coalescing):
  these live in `rue-codegen` (per-backend, post-CFG), but their *aggressiveness*
  is gated by the same level knob → tuned up at **`-O2`**.
- **Bounded larger-cap non-leaf inlining**: may grow code and compile time, so it
  is gated at **`-O3`** and debited against the shared per-function budget.
- **Loop optimizations** (invariant hoisting / LICM, unrolling): compile-time and
  size cost, speculative payoff → **`-O3`**.
- **Range analysis / bounds-check elimination**: removes the dynamic test for
  accesses proven in range, under the trap-preservation limits of spec 8.2:9
  (see clarification 3 above) → **`-O2`** for the locally provable cases, with
  loop-carried range facts arriving alongside the loop passes at **`-O3`**.

- **Vectorization / SIMD**: far future, most speculative, per-backend → **`-O3`**.

### Where the level knob lives

`OptLevel` stays the single source of truth in `crates/rue-cfg/src/opt/mod.rs`,
threaded through `optimize_with_budget(cfg, level, type_pool, budget)`. As passes
accrue, its level match gains per-level arms (e.g. `O2 | O3 => { cse::run(cfg); … }`).
layered on top of the `O1` set, preserving the monotonic-superset rule.
Codegen-side knobs (regalloc aggressiveness) read the same `OptLevel` rather
than inventing a parallel flag, so there is one dial for the whole pipeline.

## Implementation Phases

This ADR remains the plan for future phases; the checklist records which
phase-specific pieces are implemented in the current compiler.

- [x] **Phase 0: Differential net** — RUE-236 (done; PR #1092). Prerequisite.
- [x] **Phase 1: Define the contract** — this ADR (RUE-245).
- [x] **Phase 2: `-O1` completions** — peephole/strength-reduction + simplify-cfg,
  added at `-O1` with differential coverage. (RUE-910, RUE-911, RUE-912; merged
  2026-07-16)
- [ ] **Phase 3: `-O2` content** — CSE/GVN, copy prop, and conservative
  inlining. Partial: block-local CSE (RUE-913), copy propagation /
  store-to-load forwarding (RUE-914), and conservative whole-program inlining
  plus Phase 5 reachability (RUE-930, RUE-933) have landed; broader GVN and
  other O2 tuning remain. (file RUE-NNN)
- [ ] **Phase 4: `-O3` content** — partial: trap-free LICM (RUE-927), bounded
  constant-trip unrolling (RUE-928), and larger-cap bounded non-leaf inlining
  (RUE-931) have landed; broader/profile-guided inlining and guarded trapping-op
  hoisting (RUE-934) remain. (file RUE-NNN)
- [ ] **Phase 5 (optional): size levels** — `-Os`/`-Oz` if a size-sensitive
  target (WASM/embedded) materializes. (file RUE-NNN)

Each of Phases 2–4 must, in the same change, (a) place the pass at the level
this ADR assigns, (b) add multi-case differential CLI coverage for the code
shapes the pass rewrites, and (c) keep the full differential set green.

## Consequences

### Positive

- **`-O2`/`-O3` have an explicit current contract** and a documented path for
  broader future optimization; users get the `-O` contract they expect from
  GCC/Clang/rustc.
- **Every future pass has a documented home** decided by a consistent cost/risk
  rule, so pass placement is not re-litigated case by case.
- **The invariant is explicit and enforced**, making "optimization" vs
  "miscompile" a bright line the differential harness already polices.
- **Incremental level evolution**: the differential net and phase checklist
  make each O2/O3 addition explicit while preserving the O0 baseline.

### Negative

- **Growing the differential set is now mandatory** for every opt PR — more test
  authoring per pass (deliberate: coverage must follow capability, the gap that
  let RUE-311 ship).
- **Higher levels will eventually diverge in codegen**, so `-O3` bug reports may
  not reproduce at `-O0`; the differential harness is the mitigation.

### Neutral

- **No language-semantics change; no spec sections; no preview gate.** Levels are
  compiler-internal policy (echoing ADR-0012's framing).
- **`-O0` remains the default**, matching debug-first ergonomics until a release
  profile decides otherwise.

## Open Questions

- **Should `-O2` become the default for a future `--release`/`build` mode?** Out
  of scope here; this ADR only defines what each level *contains*, not which the
  driver picks. (Relates to RUE-45's release-CI work.)
- **Do we want `-Os`/`-Oz`?** Deferred until a size-sensitive target exists
  (Phase 5).

## Future Work

- Size-optimization axis (`-Os`/`-Oz`) — separate cost axis, per prior art.
- A possible Zig-style split of the *safety* dimension from the *optimization*
  dimension, if Rue ever gains checks that a level might elide. Today Rue's
  traps are always-on and observable, so there is no such axis yet.
- Profile-guided and link-time optimization — far future, their own ADRs.

## References

- [ADR-0012: Compiler Optimization Passes](0012-optimization-passes.md) — the
  CFG opt framework and the original (now refined) level table this ADR builds on.
- [ADR-0003: Constant Expression Evaluation](0003-constant-evaluation.md) —
  RIR-level folding that `constfold` complements.
- RUE-245 (this design), RUE-236 (differential-opt harness, the enforcing net),
  RUE-45 (release-mode CI), RUE-348 (enum-fold miscompile the net caught),
  RUE-311 (heap corruption a coverage gap let ship).
- [GCC Optimize Options](https://gcc.gnu.org/onlinedocs/gcc/Optimize-Options.html)
- [Clang User Manual — Optimization](https://clang.llvm.org/docs/UsersManual.html)
- [rustc Codegen Options — opt-level](https://doc.rust-lang.org/rustc/codegen-options/index.html)
- [Zig Language Overview — build modes](https://ziglang.org/learn/overview/)
