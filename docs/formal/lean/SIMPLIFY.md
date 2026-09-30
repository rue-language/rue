# Proof simplification: strategy and baseline

The proofs in this package grew one slice at a time, under deadline. This page
(RUE-2471) is the plan for making them simpler without moving what they
prove. It sets the rules every simplification follows, the metrics recorded
before and after each pass, the baseline those metrics give at the start, and
a ranked list of candidates drawn from the metrics and the proof map
([MAP.md](MAP.md)).

The work lands in three parts, each its own pull request:

1. this page, and the metrics script `bin/simplify-metrics.py`;
2. one clean-room re-derivation of a spine theorem (the protocol is below),
   reported here;
3. a first reduction pass on the module recommended below, with its measured
   deltas, and one follow-up issue per remaining candidate.

Simpler proofs are easier to review, cheaper to keep working as Phase D adds
loans and the store, and a smaller surface for a reader to trust. The model is
the Conway write-up's experience: a proof map finds the unnecessary detours in
a proof's spine, golfing handles local cleanup, and the large gains come from
reorganizing concepts, not from shortening lines.

## Invariants

Every simplification pull request keeps all of these, and shows the evidence
in its description:

- Spine statements never change. The Spec layer (`RueCore/Spec/*.lean`) is
  not edited, and `spine-fingerprints.txt` is unchanged, so every statement
  fingerprint the chain checks still matches.
- Lean Comparator passes (`comparator/run.sh`): every spine theorem is still
  proved, against the unchanged challenge, with only `propext` and
  `Quot.sound`.
- The trusted-base lint is clean (`lake exe ruecore-lint`), the layering audit
  passes (`lake exe ruecore-layers`), and `leanchecker` replays every module.
- `DIGEST.md` changes only by removed or renamed helpers: no spine theorem's
  entry changes, and no definition's.
- The chain (build, generated files, the seed corpus and 200 generated cases
  against the compiler) and `scripts/rue lean-bridge` are green.
- The metrics below are recorded before and after the pass, and the pull
  request quotes the rows it moved.
- Every lemma replacement is followed by the axiom gate (`TRUST.md`, or
  `#print axioms` on the changed theorems). A shorter proof that pulls in
  `Classical.choice` is rejected: TOOLING.md records one that compiles, passes
  every golf rule, and does exactly that.

## The metrics

`bin/simplify-metrics.py` prints them as one markdown report. Run it from
`docs/formal/lean`:

```bash
python3 bin/simplify-metrics.py              # every metric, build times included (about 6 minutes)
python3 bin/simplify-metrics.py --no-time    # the deterministic metrics only (about 20 seconds)
python3 bin/simplify-metrics.py --runs 5     # more timed runs per module
```

It builds every module of the package first (a no-op on a warm tree). The
half of it that reads the compiled environment is
`bin/simplify_metrics.lean`, run with `lake env lean --run`, as
`bin/mutate_polarity.lean` is for `bin/mutate.py`. Both files are L3
tooling: neither is a module of the package, nothing imports them, and the
layering audit and the trusted-base lint, which walk the package's roots, do
not see them (both pass with them present).

Every metric but build time is deterministic: two runs on one tree print the
same report, byte for byte, less the timing columns.

### Metric methods

- Lines per module: from the sources. Lines is every line of the file. Code
  lines are the lines with anything left once comments are blanked: `--`
  comments, nested block comments and doc-comments, and string literals. So a
  doc-comment counts as lines but not as code lines.
- Theorems per module: from the compiled environment. A theorem counts when a
  module of the package declares it, it has a declaration range, and Lean did
  not generate it: equation and unfolding lemmas, `match` and `proof_`
  auxiliaries, and a structure's `Prop`-field projections are left out, and a
  `private` theorem counts under its own name. Checked against a scan of the
  sources for the `theorem` keyword: every module agrees, less the scan's
  false hits in prose.
- Helper lemmas per spine theorem: from the compiled environment, by walking
  proof terms the way `RueCore/Map.lean`'s `walk` does, with its own
  `proofValue?` (a theorem's proof, and the value of a proof-bearing or
  compiler-generated `def` on the way), staying inside the package. The count
  is authored non-spine theorems, in two forms: all, walking through the
  other spine theorems a proof uses, which is the question's transitive
  closure; and own, stopping at another spine theorem, which is what the
  proof adds by itself. The MAP column is `MAP.md`'s size-stats number,
  computed by `Map.walkAll` itself. It is not the same quantity: it stops at
  every marked node, milestones included, and it counts generated lemmas too.
  The metaprogram is exact for the question asked; MAP's number is not, so
  the table reports both.
- Build time per module: `lake env lean <file>`, with every import already
  built, run three times per module in turn. Reported: the median wall time,
  the spread between the fastest and the slowest run, and the median CPU time
  (user plus system, of the child processes). `lake build <module>` on a warm
  cache is not per module, and a cold `lake build` runs modules in parallel,
  so neither is used. The numbers include about 0.2 s of process start and
  import loading per module (the smallest modules take 0.21 s). They are
  noisy: Lean elaborates in parallel, so the wall time of a module with many
  independent declarations depends on the machine's cores (Equivalence: 0.9 s
  wall, 3.4 s CPU). The spread at baseline was at most 0.17 s
  (`Nonvacuous.Glue`, a 6.5 s module), and an earlier run on the same tree
  saw 1.9 s on `Explain`. Compare CPU time first, and treat a change
  under about 10 percent, or under a quarter of a second, as noise. The
  baseline ran on a 10-core arm64 Mac.
- Overrides: every `set_option` in the package's sources outside comments and
  strings, with its file and line, `maxHeartbeats` among them. The trusted-base
  lint separately forbids an unbounded heartbeat budget in L0 to L2.
- Warnings: from `lake build`'s output, which replays each module's cached
  log on a warm tree, so the count does not depend on a clean build. Unused
  `simp` arguments are counted apart from every other warning.
- Duplicated lemma shapes: from the compiled environment. Each authored
  theorem's type is normalized (universe parameters renamed by position,
  binder names and binder annotations erased, metadata stripped), so
  alpha-equivalent statements are equal expressions; the groups are the hash
  buckets, checked by equality, that hold two or more names. Exact groups
  compare that normalized type. Skeleton groups first replace every predicate
  the package defines (a constant whose type ends in `Prop`) by one
  placeholder, so the same lemma about two different predicates falls in one
  group. The glue modules (`Nonvacuous.Glue`, `Sharp.Glue`) re-prove one
  statement per witness by design, and are summarized rather than listed; a
  skeleton group whose statement is a single predicate (`Spine`'s one-line
  restatements of the Spec layer) is left out.

## Baseline

Measured by `python3 bin/simplify-metrics.py` at `c62961ed3`, toolchain `leanprover/lean4:v4.33.1`.

### Lines, theorems, build time, overrides and warnings per module

| Module | Layer | Lines | Code lines | Theorems | Build wall s (median of 3) | Spread s | CPU s | `set_option` | Unused `simp` args | Other warnings |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `Float` | L0 | 1197 | 553 | 15 | 0.81 | 0.04 | 1.49 | 0 | 0 | 0 |
| `Syntax` | L0 | 1143 | 406 | 5 | 0.97 | 0.01 | 1.49 | 0 | 0 | 0 |
| `Checker.Defs` | L1 | 694 | 495 | 0 | 0.68 | 0.02 | 0.77 | 0 | 0 | 0 |
| `Dynamics` | L1 | 1840 | 751 | 0 | 0.90 | 0.01 | 1.27 | 0 | 0 | 0 |
| `Equivalence.Defs` | L1 | 41 | 14 | 0 | 0.23 | 0.01 | 0.22 | 0 | 0 | 0 |
| `Soundness.Defs` | L1 | 239 | 95 | 0 | 0.27 | 0.00 | 0.28 | 0 | 0 | 0 |
| `Statics` | L1 | 1834 | 536 | 0 | 0.76 | 0.01 | 0.93 | 0 | 0 | 0 |
| `Step` | L1 | 1073 | 624 | 0 | 0.66 | 0.02 | 0.87 | 0 | 0 | 0 |
| `Trace.Defs` | L1 | 486 | 225 | 0 | 0.39 | 0.01 | 0.47 | 0 | 0 | 0 |
| `Checker` | L2 | 1218 | 1003 | 24 | 3.03 | 0.09 | 3.91 | 0 | 0 | 0 |
| `Dynamics.Lemmas` | L2 | 56 | 29 | 4 | 0.28 | 0.00 | 0.38 | 0 | 0 | 0 |
| `Equivalence` | L2 | 2004 | 1437 | 104 | 0.85 | 0.01 | 3.45 | 0 | 0 | 0 |
| `Float.Lemmas` | L2 | 642 | 459 | 55 | 1.58 | 0.00 | 4.66 | 0 | 0 | 0 |
| `Nonvacuous` | L2 | 479 | 403 | 22 | 0.61 | 0.00 | 1.55 | 0 | 0 | 0 |
| `Nonvacuous.Glue` | L2 | 2907 | 2334 | 250 | 6.46 | 0.17 | 10.06 | 0 | 0 | 0 |
| `Sharp` | L2 | 1362 | 1214 | 56 | 0.83 | 0.01 | 2.34 | 0 | 0 | 0 |
| `Sharp.Glue` | L2 | 1232 | 1010 | 89 | 0.52 | 0.00 | 1.07 | 0 | 0 | 0 |
| `Soundness` | L2 | 4371 | 3166 | 181 | 3.27 | 0.01 | 8.41 | 0 | 0 | 0 |
| `Spine` | L2 | 245 | 117 | 95 | 0.25 | 0.01 | 0.28 | 0 | 0 | 0 |
| `Statics.Lemmas` | L2 | 2919 | 2216 | 145 | 5.44 | 0.09 | 8.52 | 0 | 0 | 0 |
| `Step.Lemmas` | L2 | 481 | 367 | 32 | 1.02 | 0.02 | 4.37 | 0 | 0 | 0 |
| `Tombstone` | L2 | 1496 | 1245 | 56 | 1.53 | 0.08 | 3.63 | 0 | 0 | 0 |
| `Trace` | L2 | 2187 | 1758 | 106 | 1.69 | 0.07 | 4.36 | 0 | 0 | 0 |
| `TraceExact` | L2 | 2944 | 2367 | 99 | 2.29 | 0.01 | 8.48 | 0 | 0 | 0 |
| `TraceOrder` | L2 | 1439 | 1065 | 73 | 1.05 | 0.02 | 3.69 | 0 | 0 | 0 |
| `TracePrefix` | L2 | 762 | 581 | 31 | 1.80 | 0.02 | 2.73 | 0 | 0 | 0 |
| `TraceWhole` | L2 | 1587 | 1264 | 66 | 0.95 | 0.02 | 3.41 | 0 | 0 | 0 |
| `RueCore` | L3 | 35 | 35 | 0 | 0.23 | 0.01 | 0.22 | 0 | 0 | 0 |
| `Corpus` | L3 | 1290 | 1052 | 0 | 0.52 | 0.01 | 0.68 | 0 | 0 | 0 |
| `CorpusMain` | L3 | 127 | 75 | 0 | 0.29 | 0.00 | 0.30 | 0 | 0 | 0 |
| `Digest` | L3 | 949 | 610 | 0 | 1.42 | 0.02 | 1.64 | 0 | 0 | 0 |
| `DigestMain` | L3 | 757 | 563 | 0 | 1.61 | 0.01 | 1.85 | 0 | 0 | 0 |
| `Examples` | L3 | 4292 | 1817 | 10 | 5.75 | 0.09 | 9.85 | 1 | 0 | 0 |
| `Explain` | L3 | 3125 | 2378 | 6 | 27.09 | 0.18 | 29.22 | 2 | 16 | 0 |
| `Explain.Html` | L3 | 353 | 279 | 0 | 0.33 | 0.02 | 0.40 | 0 | 0 | 0 |
| `Explain.Ledger` | L3 | 130 | 73 | 0 | 0.28 | 0.00 | 0.30 | 0 | 0 | 0 |
| `Explain.Text` | L3 | 268 | 187 | 0 | 0.32 | 0.02 | 0.38 | 0 | 0 | 0 |
| `ExplainMain` | L3 | 63 | 36 | 0 | 0.26 | 0.00 | 0.26 | 0 | 0 | 0 |
| `Gen` | L3 | 3049 | 1183 | 0 | 3.51 | 0.07 | 5.02 | 0 | 0 | 0 |
| `Layers` | L3 | 115 | 74 | 0 | 1.06 | 0.04 | 1.06 | 0 | 0 | 0 |
| `LayersMain` | L3 | 234 | 128 | 0 | 1.22 | 0.03 | 1.28 | 0 | 0 | 0 |
| `Lint` | L3 | 1148 | 657 | 0 | 1.88 | 0.03 | 2.27 | 0 | 0 | 0 |
| `LintMain` | L3 | 139 | 94 | 0 | 1.26 | 0.02 | 1.27 | 0 | 0 | 0 |
| `Literature` | L3 | 285 | 237 | 0 | 1.07 | 0.01 | 1.08 | 0 | 0 | 0 |
| `Map` | L3 | 365 | 162 | 0 | 1.17 | 0.02 | 1.25 | 0 | 0 | 0 |
| `Print` | L3 | 817 | 399 | 0 | 0.52 | 0.03 | 0.65 | 0 | 0 | 0 |
| `Witnesses` | L3 | 596 | 318 | 23 | 3.73 | 0.03 | 4.71 | 0 | 0 | 0 |
| `Spec` | Spec | 554 | 455 | 0 | 0.30 | 0.06 | 0.33 | 0 | 0 | 0 |
| `Spec.Checker` | Spec | 32 | 10 | 0 | 0.23 | 0.02 | 0.22 | 0 | 0 | 0 |
| `Spec.Equivalence` | Spec | 104 | 44 | 0 | 0.23 | 0.01 | 0.22 | 0 | 0 | 0 |
| `Spec.Nonvacuous` | Spec | 400 | 246 | 0 | 0.29 | 0.00 | 0.29 | 0 | 0 | 0 |
| `Spec.Safety` | Spec | 146 | 45 | 0 | 0.22 | 0.01 | 0.21 | 0 | 0 | 0 |
| `Spec.Sharp` | Spec | 1105 | 692 | 0 | 0.49 | 0.01 | 0.52 | 0 | 0 | 0 |
| `Spec.Step` | Spec | 102 | 38 | 0 | 0.23 | 0.02 | 0.22 | 0 | 0 | 0 |
| `Spec.Trace` | Spec | 153 | 55 | 0 | 0.23 | 0.00 | 0.22 | 0 | 0 | 0 |
| all 55 modules | | 57611 | 37676 | 1547 | 94.9 | | 149.0 | 3 | 16 | 0 |

By layer: L0 2340 lines (959 code), 20 theorems; L1 6207 lines (2740 code), 0 theorems; L2 28331 lines (22035 code), 1488 theorems; L3 18137 lines (10357 code), 39 theorems; Spec 2596 lines (1585 code), 0 theorems.

#### `set_option` overrides

* `RueCore/Examples.lean:3209`: `set_option exponentiation.threshold 400` (L3)
* `RueCore/Explain.lean:1772`: `set_option maxHeartbeats 400000` (L3)
* `RueCore/Explain.lean:2965`: `set_option maxHeartbeats 400000` (L3)

#### Build warnings

* `RueCore/Explain.lean` (L3): 16 unused `simp` arguments (lines 1792, 1796, 1800, 1804, 1808, 1812, 1816, 1820, 1824, 1829, 1910, 1914, 1918, 1922, 1992, 3041)

### Helper lemmas per spine theorem

Authored non-spine theorems a spine theorem's proof depends on transitively (`bin/simplify_metrics.lean`). All: walking through other spine theorems too. Own: stopping at another spine theorem. MAP: `MAP.md`'s size-stats count, which stops at every marked node and counts generated lemmas too.

| Spine theorem | Module | Proof lines | Helpers (all) | Their lines | Helpers (own) | Spine theorems used | MAP |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `soundness` | `Soundness` | 861 | 236 | 3667 | 236 | 0 | 309 |
| `run_safe` | `Soundness` | 24 | 240 | 3703 | 2 | 2 | 2 |
| `no_refusal` | `Soundness` | 46 | 241 | 3718 | 1 | 1 | 3 |
| `no_use_after_move` | `Soundness` | 3 | 241 | 3718 | 0 | 1 | 0 |
| `no_use_after_drop` | `Soundness` | 12 | 241 | 3718 | 0 | 1 | 0 |
| `run_no_use_after_drop` | `Tombstone` | 12 | 32 | 699 | 32 | 0 | 51 |
| `no_linear_leak` | `Soundness` | 4 | 241 | 3718 | 0 | 1 | 0 |
| `no_linear_overwrite` | `Soundness` | 7 | 241 | 3718 | 0 | 1 | 0 |
| `no_linear_discard` | `Soundness` | 3 | 241 | 3718 | 0 | 1 | 0 |
| `fuel_mono` | `Soundness` | 21 | 5 | 285 | 5 | 0 | 8 |
| `no_masking` | `Soundness` | 11 | 5 | 285 | 0 | 1 | 0 |
| `run_ne_returned` | `Soundness` | 22 | 2 | 24 | 2 | 0 | 2 |
| `check_sound` | `Checker` | 706 | 6 | 38 | 6 | 0 | 22 |
| `checkProgram_sound` | `Checker` | 13 | 19 | 200 | 13 | 1 | 24 |
| `no_double_free` | `Trace` | 23 | 333 | 5416 | 1 | 3 | 6 |
| `step_no_double_free` | `TracePrefix` | 12 | 453 | 7391 | 212 | 5 | 167 |
| `freed_once` | `Trace` | 10 | 87 | 1586 | 87 | 0 | 14 |
| `dtor_once` | `Trace` | 7 | 90 | 1662 | 90 | 0 | 20 |
| `drop_exactly_once` | `TraceExact` | 55 | 369 | 6457 | 135 | 1 | 148 |
| `rest_exactly_once` | `TraceExact` | 52 | 372 | 6521 | 138 | 1 | 151 |
| `whole_program_exactly_once` | `TraceWhole` | 37 | 568 | 10267 | 247 | 3 | 307 |
| `drop_order` | `TraceOrder` | 36 | 478 | 7778 | 144 | 1 | 21 |
| `drop_glue_order` | `TraceOrder` | 25 | 439 | 7221 | 105 | 1 | 82 |
| `Step.det` | `Step.Lemmas` | 11 | 1 | 4 | 1 | 0 | 2 |
| `Step.terminal` | `Step.Lemmas` | 5 | 0 | 0 | 0 | 0 | 3 |
| `Config.trichotomy` | `Step.Lemmas` | 9 | 8 | 66 | 4 | 1 | 13 |
| `step_iff` | `Step.Lemmas` | 14 | 4 | 32 | 4 | 0 | 12 |
| `Config.stuck_iff` | `Step.Lemmas` | 17 | 8 | 66 | 4 | 1 | 13 |
| `step_stuck_isStuckState` | `Step.Lemmas` | 48 | 10 | 127 | 10 | 0 | 27 |
| `step_progress` | `Equivalence` | 11 | 343 | 5278 | 0 | 2 | 0 |
| `step_safeAt` | `Equivalence` | 13 | 345 | 5300 | 3 | 4 | 2 |
| `step_type_safety` | `Equivalence` | 25 | 328 | 5104 | 87 | 3 | 2 |
| `step_no_use_after_drop` | `Tombstone` | 10 | 42 | 712 | 38 | 1 | 57 |
| `eval_big_to_small` | `Equivalence` | 17 | 290 | 4616 | 0 | 2 | 0 |
| `run_sim` | `Equivalence` | 18 | 49 | 898 | 48 | 1 | 0 |
| `eval_small_to_big` | `Equivalence` | 23 | 335 | 5208 | 0 | 2 | 0 |
| `run_small_to_big` | `Equivalence` | 25 | 96 | 1514 | 93 | 4 | 10 |
| `never_refused_iff` | `Equivalence` | 14 | 343 | 5278 | 0 | 2 | 0 |
| `step_never_stuck_of_run` | `Equivalence` | 17 | 104 | 1584 | 0 | 2 | 0 |
| `run_refused_of_step_stuck` | `Equivalence` | 13 | 97 | 1522 | 95 | 4 | 12 |
| `eval_diverges_iff` | `Equivalence` | 26 | 333 | 5168 | 91 | 5 | 8 |

### Duplicated lemma shapes

Exact: the same statement up to alpha-equivalence (universe and binder names, binder annotations and metadata erased).

* 2 (type size 83): `Activation.unwindScope_push` (Equivalence, 5 lines), `Tombstone.Activation.unwindScope_ext` (Tombstone, 6 lines)
* 2 (type size 15): `Nonvacuous.withTrace_nil` (Nonvacuous, 3 lines), `Sharp.withTrace_nil` (Sharp, 3 lines)
* 2 (type size 31): `Sharp.storeCC_one` (Sharp, 5 lines), `StoreCC.single` (Trace, 7 lines)
* 2 (type size 125): `no_double_free` (Trace, 23 lines), `no_double_free_of_step` (TracePrefix, 20 lines)
* and 6 groups (263 theorems) entirely inside the witness glue modules (`Nonvacuous.Glue`, `Sharp.Glue`), where one statement is re-proved per witness by design: sizes 250, 3, 2, 2, 4, 2

Skeleton, beyond the exact groups: the same statement once every predicate of the package is replaced by one placeholder. A group whose statement is a single predicate (type size 1: `Spine`'s re-statements of the Spec layer) is left out.

* 2 (type size 53): `Blocks.append` (TraceOrder, 9 lines), `DropGlueBlocks.append` (TraceOrder, 11 lines)
* 2 (type size 39): `Config.SafeAt.invariant` (Equivalence, 6 lines), `Config.SafeAt.steps` (Equivalence, 4 lines)
* 2 (type size 49): `EvalRes.bind_noBrk` (TraceExact, 9 lines), `EvalRes.bind_noRet` (TraceExact, 9 lines)
* 2 (type size 17): `EvalRes.withTrace_noBrk` (TraceExact, 4 lines), `EvalRes.withTrace_noRet` (TraceExact, 4 lines)
* 2 (type size 163): `MSim.absorb` (TraceWhole, 21 lines), `Sim.absorb` (Equivalence, 24 lines)
* 2 (type size 93): `MSim.peel` (TraceWhole, 12 lines), `Sim.peel` (Equivalence, 13 lines)
* 2 (type size 77): `MSim.pre` (TraceWhole, 7 lines), `Sim.pre` (Equivalence, 9 lines)
* 2 (type size 111): `MSim.withTrace` (TraceWhole, 14 lines), `Sim.withTrace` (Equivalence, 16 lines)
* 2 (type size 49): `MSteps.peel` (TraceWhole, 8 lines), `Steps.peel` (Equivalence, 9 lines)
* 2 (type size 27): `MSteps.toSteps` (TraceWhole, 5 lines), `Steps.single` (Equivalence, 3 lines)
* 2 (type size 39): `MSteps.trans` (TraceWhole, 6 lines), `Steps.trans` (Equivalence, 6 lines)
* 3 (type size 25): `Tombstone.steps_live` (Tombstone, 10 lines), `step_nested` (TraceOrder, 55 lines), `step_ordered` (TraceOrder, 62 lines)
* 2 (type size 83): `drop_glue_order` (TraceOrder, 25 lines), `step_blocks` (TraceOrder, 17 lines)
* 2 (type size 61): `evalArgs_noBrk` (TraceExact, 16 lines), `evalArgs_noRet` (TraceExact, 17 lines)
* 2 (type size 13): `eval_sim` (Equivalence, 46 lines), `eval_steps_of_outOfFuel` (Equivalence, 42 lines)
* 2 (type size 161): `fieldsSwapped_rejected` (Witnesses, 14 lines), `glue_fieldsSwapped_rejected` (Witnesses, 15 lines)
* 2 (type size 63): `msim_brk` (TraceWhole, 9 lines), `sim_brk` (Equivalence, 5 lines)
* 2 (type size 19): `reachable_nested` (TraceOrder, 12 lines), `reachable_ordered` (TraceOrder, 13 lines)
* 2 (type size 27): `run_blocks` (TraceOrder, 6 lines), `run_glue_blocks` (TraceOrder, 9 lines)

The package's sources at that commit are trunk `e9349bc42`'s; the commit adds
only the two metrics files under `bin/`.

What the baseline says, in short:

- The proofs are L2: 28,331 lines (22,035 code) of the 57,611, and 1,488 of
  the 1,547 theorems. Five modules hold over half of L2's code: `Soundness`
  (3,166 code lines), `TraceExact` (2,367), `Nonvacuous.Glue` (2,334),
  `Statics.Lemmas` (2,216) and `Trace` (1,758).
- L2 has no `set_option` at all and no build warning. The two
  `maxHeartbeats 400000` overrides and the 16 unused `simp` arguments the
  issue names are all in `Explain.lean`, which is L3 tooling (the step-table
  explainer), and so is `Examples.lean`'s `exponentiation.threshold`. None is
  on a claim's path.
- Elaborating every module once takes about 95 s of summed wall time and
  149 s of CPU; `Explain` alone is 27 s of that. The slowest proof modules
  are `Nonvacuous.Glue` (6.5 s), `Statics.Lemmas` (5.4 s), `Soundness`
  (3.3 s), `Checker` (3.0 s) and `TraceExact` (2.3 s).
- Five spine theorems depend on more than 400 authored helpers:
  `whole_program_exactly_once` (568, 10,267 lines of helper proofs),
  `drop_order` (478), `step_no_double_free` (453), `drop_glue_order` (439)
  and `rest_exactly_once` (372). What a spine proof adds by itself, stopping
  at the other spine theorems it uses, is at most 252 helpers
  (`whole_program_exactly_once`), then 236 (`soundness`).

## Three kinds of simplification

In order of expected payoff: structural, detours, local. Each candidate below
names its evidence in the metrics or the proof map, its size, and an expected
delta. The deltas are estimates made from reading the proofs, not
measurements; a pass records the measured one.

### Structural

Shared lemmas that several spine proofs re-prove in different forms.

1. The simulation, proved three and a half times. `Equivalence` proves the
   simulation between `eval` and `Step` twice over: `Sim` (34 theorems,
   756 lines, `eval_sim`) for a terminating result, and `Long` (29 theorems,
   404 lines, `eval_steps_of_outOfFuel`) for exhausted fuel. `TraceWhole`
   re-proves the first with a lossless-ledger side condition on every step:
   `MSim` and `MSteps` (45 theorems, 1,000 lines). `TracePrefix` re-proves the
   second with the conservation law's ledger added: `LongC` (24 theorems,
   507 lines, `eval_longc`). The skeleton groups show the copies directly,
   one statement shape each: `Sim.absorb`/`MSim.absorb` (24 and 21 lines),
   `Sim.peel`/`MSim.peel`, `Sim.pre`/`MSim.pre`, `Sim.withTrace`/`MSim.withTrace`,
   `sim_brk`/`msim_brk`, `Steps.peel`/`MSteps.peel`, `Steps.trans`/`MSteps.trans`
   and `Steps.single`/`MSteps.toSteps`. One simulation parametrized by a
   per-step invariant (true for `Sim`, the ledger for `MSim`), and one
   long-run lemma parametrized the same way (for `Long` and `LongC`), would
   remove most of one copy of each. Spine theorems whose helper closure
   shrinks: `whole_program_exactly_once` (568 helpers), `step_no_double_free`
   (453). Expected: 800 to 1,200 lines fewer across `Equivalence`,
   `TraceWhole` and `TracePrefix`, about 60 theorems fewer. The largest
   candidate, and it spans three modules.
2. One induction over `eval`, many times. Eleven proofs are a fuel induction
   over `eval` with one case per construct: `soundness` (861 lines),
   `eval_conserves` (390), `Tombstone.eval_live` (382), `rest_step` (326),
   `eval_exact` (306), `eval_glue_blocks` (246), `eval_quiet` (229),
   `eval_tidy` (228), `eval_succ` (218), `eval_longc` (65) and `eval_msim`
   (split into `msim_` lemmas per construct): 3,251 lines in the first ten,
   and `eval_sim` and `eval_steps_of_outOfFuel` are two more. Each
   invariant brings its own composition lemmas for the same two operations,
   `EvalRes.bind` and `EvalRes.withTrace`: `EvalOk.bind`, `Cons.bind`,
   `Exact.bind`, `Tidy.bind`, `DropGlueBlocks.bind`, `Sim.bind`, and the
   `withTrace`/`prefix` twins of each. A generic evaluation-context lemma in
   the Felleisen-Hieb style, stated once over an invariant on `EvalRes` that
   is closed under `bind` and `withTrace`, would discharge the constructs
   whose case is only "run the operands in order, then compute" (the
   literals, the operators, casts, `@dbg`, the aggregate literals, and the
   `...Bot` cases where an operand diverges and the form does too), leaving
   each proof only its binder, scope, call and unwind cases. In `soundness`
   alone the ten divergence arms (`binopBot`, `floatBinopBot`,
   `indexReadBot`, `indexWriteBotRhs`, `indexWriteBotIdx`, `letBot`,
   `assignBot`, `seqBot`, `iteBot`, `retBot`) are 65 lines of the same step,
   and it has 19 six-way case splits on an `eval` result. Expected: 20 to 30
   percent of those 3,251 lines, so 650 to 1,000 fewer, done one invariant
   at a time. It touches `Soundness`, the largest module, so it should
   follow a smaller pass that shows the generic lemma's shape works.
3. TraceExact's own families. Within `TraceExact` (99 theorems, 2,367 code
   lines): four of the fuel inductions above in one module (`eval_quiet`,
   `eval_exact`, `eval_tidy`, and `rest_step` under `eval_exact`, 1,089 lines
   together); three twin pairs that differ only in which abrupt result they
   exclude, `EvalRes.bind_noRet`/`bind_noBrk`,
   `EvalRes.withTrace_noRet`/`withTrace_noBrk` and
   `evalArgs_noRet`/`evalArgs_noBrk` (59 lines, all skeleton-equal); and the
   `Tidy` family (nine lemmas, 169 lines), where `Tidy` is `Local` plus what
   `Settled` asks (`Tidy.settled`), so the `Tidy` lemmas can be read off
   `Local`'s and `Settled`'s. `Tidy`, `Settled` and `Lead` themselves are
   statement vocabulary (they are in the closure of `drop_exactly_once` and
   `rest_exactly_once`, and `Lead` is in `Spec/Sharp.lean`), so only their
   lemmas move, never their definitions.
4. One invariant along `Steps`, three times. `Tombstone.steps_live` (10
   lines), `step_nested` (55) and `step_ordered` (62) are one skeleton group,
   and `reachable_ordered`/`reachable_nested` another: each proves that a
   property every `Step` preserves holds along `Steps` by the same
   induction. TOOLING.md found the same from the golf anchors. One lemma
   beside `Steps` replaces the inductions. Expected: 30 to 50 lines.
5. Exact duplicates. Four pairs state the same thing:
   `Activation.unwindScope_push` (`Equivalence`) and
   `Tombstone.Activation.unwindScope_ext` (`Tombstone`); `Nonvacuous.withTrace_nil`
   and `Sharp.withTrace_nil`; `Sharp.storeCC_one` and `StoreCC.single`
   (`Trace`); and `no_double_free` with `no_double_free_of_step`, which is a
   detour below. Removing the first three is about 15 lines; the value is
   one name per fact. Also `fieldsSwapped_rejected` and
   `glue_fieldsSwapped_rejected` in `Witnesses` (L3), 14 and 15 lines.

### Detours

Spine paths in the proof map that go through an intermediate the conclusion
does not need. The edges are MAP.md's; the helper counts are the baseline's.

1. The trace grammar goes through `eval`. `drop_order` and `drop_glue_order`
   are statements over `Step`, but their within-value half is proved by
   going back to `eval`: `eval_small_to_big` to `step_blocks` to `drop_order`,
   and `eval_conserves` to `run_blocks` and `drop_glue_order` (through
   `eval_glue_blocks`, a 246-line fuel induction). `eval_small_to_big` in
   turn rests on `no_refusal`, `soundness`, `run_sim` and
   `eval_steps_of_outOfFuel`. So `drop_order` depends on 478 helpers where
   its own proof adds 144, and `drop_glue_order` on 439 where it adds 105.
   A drop's marker and its whole walk are emitted by one `Step`
   (`dropCell` returns `.drop` followed by the walk), so `DropGlueBlocks` may
   be a plain `Step` invariant, proved along `Steps` the way the across-cells
   half already is, with no typing and no simulation; the statements keep
   their `ProgramTyped` hypothesis, which that half would then use only for
   `DtorNotCopy`. To verify before relying on it: that every event-emitting
   rule (unwinds, destructure residues, consumes) appends whole blocks.
   Expected: `drop_order`'s helper closure from 478 to about 150, and
   `eval_glue_blocks`, `run_glue_blocks` and `run_blocks` retired if nothing
   else uses them. The clean-room run below tests this detour directly.

   Verdict (part 2): refuted. The clean-room agent, with no sight of
   `TraceOrder.lean`, reached for exactly this route and could not close it
   as a plain `Step` invariant: a `Copy` residue's no-event property needs a
   copy-closed store, and `Step` maintains no such invariant without a
   configuration typing the package does not have (RUE-2423). It went back
   through `eval` for the within-value half, the same route `drop_order`
   already takes. The expected reduction is not available by rewriting
   inside `Step`; it needs RUE-2423's typing first. Close this detour rather
   than retry it without that typing.
2. `step_blocks` re-derives `drop_glue_order`. `step_blocks` (17 lines) and
   `drop_glue_order` (25) are skeleton-equal, and `run_blocks` is already
   `run_glue_blocks` read through `DropGlueBlocks.toBlocks`. So `step_blocks`
   is `drop_glue_order` mapped by `toBlocks`, a few lines, and the path
   `eval_small_to_big` to `step_blocks` to `drop_order` becomes
   `drop_glue_order` to `drop_order`. Expected: about 15 lines, and one
   route instead of two. Worth doing even if detour 1 is not.
3. Two routes to `no_double_free`. The spine's `no_double_free` (in `Trace`,
   23 lines) is proved over `eval` from `freed_once`, `dtor_once` and
   `no_refusal`. `no_double_free_of_step` (in `TracePrefix`, 20 lines) proves
   the same statement again from `step_no_double_free` and
   `eval_big_to_small`, as the Spec layer's doc-comment notes. Nothing uses
   it. Either keep it as a documented cross-check and say so in its
   doc-comment, or remove it. Expected: at most 20 lines; low value.

Two edges that look like detours are not, and are recorded so nobody chases
them:

- `step_type_safety`, `step_safeAt`, `step_progress` and `never_refused_iff`
  derive safety over `Step` from safety over `eval` through the simulation
  (`run_safe` to `init_safeAt` to `step_safeAt`), so each depends on 328 to
  363 helpers while adding at most 87. The direct route, preservation step
  by step, needs a typing of every intermediate configuration, which the
  package does not have (RUE-2423, and `TraceWhole.lean`'s module doc); the
  simulation is the cheaper proof.
- The semantic-equivalence theorems (`eval_big_to_small`, `eval_small_to_big`,
  `never_refused_iff`, `step_never_stuck_of_run`) add no helpers of their
  own, yet each depends on 104 to 343, through `no_refusal`. Their statements
  include "the run is never refused", so the dependency on `soundness` is the
  statement's, and statements do not move.

### Local

Golfing, unused `simp` arguments and heartbeat overrides. Expected payoff:
small, a percent or two of a module's lines, and no build time.

1. `Explain.lean` (L3): the 16 unused `simp` arguments (lines 1792 to 1829,
   1910 to 1922, 1992 and 3041, per the baseline) and the two
   `maxHeartbeats 400000` overrides (lines 1772 and 2965). `Explain` is 27 s
   of the 95 s total, so the override removal, if the proofs still close
   at the default budget, may also be the only build-time win on this list.
   Not a claim's path, so the lowest risk here.
2. The golf candidates in the loop's `rue-2455-golf.diff` (TOOLING.md,
   "Golf, on a scratch copy"): eleven accepted edits to `TraceOrder.lean`,
   25 lines fewer (1.7 percent), at `dropEvents_allCopy`,
   `Config.Ordered.keep`, `Config.Ordered.push`, `step_ordered`,
   `dropLocs_dropEvents`, `step_drop_order` and `StackDiscipline.newer`:
   merged identical `match` arms and `case` tags, `rcases` blocks as
   `Or.elim` terms, and single-use `have`s inlined. No time or heartbeat
   change. One rejected edit, `range'_increasing` by core's
   `List.pairwise_lt_range'`, adds `Classical.choice`. The diff is not
   applied here; it applies with detour 2's `TraceOrder` pass.
3. `lean4-skills-find-golfable` found 18 more candidates, in `Soundness`,
   `TraceExact`, `Float`, `Float/Lemmas`, `Statics/Lemmas` and `Equivalence`
   (TOOLING.md). Each is taken with its module's structural pass, not
   separately, so a module is reviewed once.

## The first pass (part 3)

Recommended: `TraceExact`. It is the issue's own candidate and the metrics
agree:

- It is the largest proof module after `Soundness` (2,944 lines, 2,367 code,
  99 theorems), and `Soundness` is a riskier first target (structural 2).
- It has the densest duplication inside one module: four fuel inductions,
  three skeleton-equal twin pairs, and the `Tidy` family (structural 3).
- Its spine theorems are only `drop_exactly_once` and `rest_exactly_once`
  (plus the milestones `eval_tidy`, `rest_step` and `pendingSafe_needed`),
  and every other change stays inside the module, so the pass tests the
  generic-invariant idea of structural 2 at the scale of one file before
  `Soundness` or the simulation (structural 1) is touched.

Planned edits: one lemma per twin pair, parametrized by the result it
excludes; the `Tidy` lemmas derived from `Local`'s and `Settled`'s; and
`eval_exact` and `eval_tidy` either sharing one induction over their
conjunction or sharing its per-construct scaffolding. Expected delta: 200 to
350 lines fewer (7 to 12 percent of the module), about 10 theorems fewer,
`drop_exactly_once`'s and `rest_exactly_once`'s own helper counts (135 and
138) down by about as many, and build time unchanged within noise (2.3 s
wall, 8.5 s CPU at baseline). Part 3 reports the measured numbers against
this page's baseline.

The simulation (structural 1) has the largest expected payoff and is the
first follow-up; it spans three modules, which is why it is not the first
pass.

## The clean-room re-derivation (part 2)

Run. The protocol:

- Theorem: `drop_order` (`Spec.drop_order_stmt`). It is mid-sized (a 36-line
  proof with 144 helpers of its own, most of `TraceOrder.lean`'s 73
  theorems), its statement has two independent halves, and its within-value
  half is detour 1, so the run tests that detour as well. Fallback, if the
  run cannot start from `drop_order`: `step_no_use_after_drop`, 38 helpers of
  its own and no typing hypothesis.
- What the agent gets: a copy of the package at this page's baseline commit
  with `RueCore/TraceOrder.lean` removed, together with every module that
  imports it (`TracePrefix`, `TraceWhole`, `Sharp`, `Spine`, `Witnesses`, the
  glue modules and the root `RueCore.lean`), so the environment it builds on
  is exactly what `TraceOrder` itself may use: L0, L1, the Spec layer and
  the proofs up to `TraceExact`, with their proofs. It gets the statement
  (`Spec/Trace.lean`), the calculus (`../01-core-calculus.md` and
  `../03-metatheory.md`), `README.md`'s build instructions and this
  protocol. It does not get `TraceOrder.lean` in any form, git history,
  `DIGEST.md`, `MAP.md`, `INDEX.md` or `TOOLING.md` (all of which list
  `TraceOrder`'s helpers or quote its proofs), or `rue-2455-golf.diff`.
  One leak is accepted: `Trace/Defs.lean` (L1) holds the invariants our proof
  uses (`Rec`, `Config.Ordered`, `Config.Nested`, `Nest`) beside the
  statement's own definitions, and the modules the agent builds on need that
  file, so it sees them. The report says whether it used them.
  It works in a new module, `RueCore/CleanRoom.lean`, whose last line is
  `theorem drop_order' : Spec.drop_order_stmt := ...`.
- Budget: one agent session, fixed ahead (for example four hours of wall
  time), with no hints during the run. It may give up; a partial result is
  reported as such, with what it had proved.
- What is measured:
  - whether it closes, with only `propext` and `Quot.sound`
    (`#print axioms drop_order'`) and no `sorry`;
  - proof size: code lines and theorem count of `CleanRoom.lean`, against
    `drop_order`'s own closure at baseline (144 helpers) and
    `TraceOrder.lean`'s 1,065 code lines;
  - how much of our helper structure it reinvents: `bin/simplify_metrics.lean`
    run over a package holding both its module and ours, with the exact and
    skeleton groups read across the two (a helper of its that lands in a
    group with one of ours reinvents it), and, by reading, which of our
    intermediate steps it reaches (`reachable_ordered`, `reachable_nested`,
    `StackDiscipline.newer`, `DropGlueBlocks.toBlocks`);
  - its route to the within-value half: through `eval` (`eval_small_to_big`,
    as ours does) or along `Steps` (detour 1);
  - the wall time and the number of build iterations it used.
- Reported here, in a section of its own: the numbers, the reinvention
  table, and what they say about which parts of `TraceOrder` are essential
  and which are accidental. The clean-room proof is not merged; what it
  teaches goes into part 3 or the follow-ups.

### What the run got, and its deviations

The run started from trunk `e9349bc42`, with `TraceOrder.lean` and its seven
importers removed, as planned. Two deviations from the protocol above. First,
the copy withheld more prose than the protocol lists: `03-metatheory.md`,
`README.md` and `GUIDE.md`, not only `DIGEST.md`, `MAP.md`, `INDEX.md`,
`TOOLING.md` and `rue-2455-golf.diff`, because those three sketch our proof
route rather than only quoting it. Second, the copy removed all of L3, not
only `TraceOrder`'s importers, because `RueCore/Digest.lean` imports the
package root `RueCore.lean`, which would have pulled `TraceOrder` back in
through that import; the agent built and worked from L0, L1, the Spec layer
and the proofs up to and including `TraceExact`, plus `01-core-calculus.md`,
this page's protocol, and the leaked invariants of `Trace/Defs.lean`.

### Did it close, and how fast

Yes. `#print axioms drop_order'` shows only `propext` and `Quot.sound`, no
`sorry`, `admit`, `native_decide` or new axiom. About 13 minutes of wall time
(11:04 to 11:17) and roughly 23 build or check iterations: about 20
`lake env lean` checks plus 3 `lake build` runs. Its one false start was a
`Classical.choice` dependency pulled in by core's `List.pairwise_lt_range'`,
fixed by proving its own `range'_pairwise` by induction instead.

### Size, against `drop_order`'s own closure and against `TraceOrder.lean`

`CleanRoom.lean` is 812 lines, 743 of them code lines by this page's own
counting method, and 48 theorems (`drop_order'` included; three small `def`s
and one tactic macro besides). Measured against `drop_order`'s own baseline
closure of 144 helpers (478 transitively, 7,778 lines of helper proof) and
against `TraceOrder.lean`'s 1,065 code lines across 73 theorems, it is a
fraction of either.

That comparison overstates `TraceOrder.lean`'s size for this purpose,
though: the module also proves `drop_glue_order` and `step_blocks`, so not
every one of its 73 theorems serves `drop_order` alone. Walking proof terms
the way `bin/simplify_metrics.lean`'s spine metric does (stopping at spine
boundaries), `drop_order`'s own reach touches 69 of `TraceOrder.lean`'s own
theorems (about 1,030 of its lines by the same per-declaration count), and
`drop_glue_order`'s own reach touches 22 (about 480 lines) — and every one
of those 22 is also among `drop_order`'s 69. `drop_glue_order` adds nothing
`drop_order` does not already pull in, because `drop_order`'s within-value
half already goes through the same `run_blocks`/`run_glue_blocks`/
`eval_glue_blocks` chain `drop_glue_order` is built from. So of
`TraceOrder.lean`'s own content, about 47 theorems (roughly 550 lines) serve
`drop_order` alone, about 22 (roughly 480 lines) serve both, and a further
handful of exported corollaries — `StackDiscipline.newer`,
`reachable_stackDiscipline`, `Blocks.append` and its neighbors — serve
neither proof's own closure directly and are read instead by `TracePrefix`,
`TraceWhole` or `Witnesses`. The 22 shared theorems are still inside
`drop_order`'s own reach, not extra content only `drop_glue_order` needs,
so the fair basis for this comparison is the full 69-theorem, 1,030-line
figure above, not a 47/550 split. Read against that figure, the clean-room
proof (48 theorems, 743 code lines) is smaller by close to 30 percent on
both counts, a real reduction. The line counts are measured differently:
1,030 sums the theorems' own lines, while 743 is the whole file's code,
including its three `def`s, its tactic macro and its `open` lines. Summed
the same way over its 48 theorems alone, the clean room is 714 lines.

### Reinvention

Run over a package holding both `TraceOrder.lean` and `CleanRoom.lean`
together, `bin/simplify_metrics.lean`'s exact and skeleton groups place 25
of the clean-room's 48 theorems — a little over half — in a group with one
of ours. This excludes `drop_order'` itself, which lands in an exact group
with `Spine.drop_order` by construction: a naive rerun counts 26. Exact matches, up to alpha-equivalence: `Blocks.append`, `Rec.mono`,
`Rec.range` with our `Rec.fresh`, `dropContents_ok`/`dropContentsList_ok`
with our `dropContents_eq`/`dropContentsList_eq`, `allCopy_dropEvents`/
`allCopyList_dropEvents` with our `dropEvents_allCopy`/
`dropEventsList_allCopy`, `dropLocs_append`, `dropLocs_dropEvents`,
`dropLocs_dropEventsList`, `cc_ok` with our `eval_ok_cc`, `finished_blocks`
with our `step_blocks`, and `range'_pairwise` with our
`range'_increasing` (the one core lemma it had to replace, for the same
`Classical.choice` reason ours avoids it). Skeleton matches beyond those:
`bind_blocks`/`bindCall_blocks` with our `DropGlueBlocks.bind`/`.absorb`,
`destructure_blocks`, `dropCell_blocks`, `dropResidue_blocks`,
`dropRetire_blocks`, `evalArgs_blocks`, `unwindLocs_blocks`, all matching a
name of ours the same shape; `introVal_blocks` with our
`DropGlueBlocks.intro`; `matchConsume_blocks`; `eval_blocks` with our
`eval_glue_blocks`; and `steps_nested` in a three-way skeleton group with our
`Tombstone.steps_live`, `step_nested` and `step_ordered` — the "one
invariant along `Steps`, three times" duplication this page's structural
candidate 4 already names, so the clean-room proof adds a fourth copy of
that same shape rather than a new one.

Of the four intermediates the protocol named: it reached the equivalent of
`reachable_nested` (its `steps_nested` composed with `init_nested`, at the
call site in `drop_order'`, is exactly `reachable_nested`'s own proof, over
a general `Steps` instead of one specialized to `Config.init`). It did not
reach `reachable_ordered`: it proved no `Config.Ordered` at all, since its
own `Config.Nested` already carries the `Rec` fact `drop_order`'s statement
needs, and its report says as much (`Kont.Ordered`, `Config.Ordered` among
what it did not use). It did not reach `StackDiscipline.newer`: the
statement asks only for the raw `StackDiscipline` and `Pairwise` facts, not
that corollary, so nothing in its proof called for it. And it did not reach
`DropGlueBlocks.toBlocks`, or touch `DropGlueBlocks` at all — its brief was
`drop_order` alone, not `drop_glue_order`.

### Its route

As this page's detour 1 guessed, the within-value half goes through `eval`
(`eval_small_to_big`, its own `finished_blocks`), not along `Step`, and for
the reason detour 1's verdict above now gives on the strength of this run: a
`Copy` residue's no-event property needs a copy-closed store, which `Step`
does not maintain without a configuration typing the package does not have
(RUE-2423), so the agent could not keep that invariant over `Step` and went
back to `eval`, where `eval_conserves` already maintains it. The stack half
goes over single `Step`s, with `Config.Nested` and no typing, the same
shape this page's across-cells half already has.

The leak: it used `Config.Nested`, `Nest`, `Stk` and `Rec` from
`Trace/Defs.lean`, beyond the statement's own vocabulary — every one an
invariant the protocol accepted leaking, and every one vocabulary the stack
half needs regardless of who proves it.

### Essential versus accidental, and a recommendation for part 3

Where the two proofs overlap, they agree almost completely: the
trace-grammar-through-`eval` combinators (`Blocks.append`, the `withTrace`
and `bind` composition lemmas, one case per `eval` form) are essential, not
accidental — an independent proof needed nearly the same shapes, in nearly
the same number, with no shorter route found. The accidental part is what
`TraceOrder.lean` proves beside `drop_order` in the same file: `drop_order`
does not need `drop_glue_order`'s own reach for anything beyond what it
already pulls in on its own route, and it does not need `Config.Ordered` or
`reachable_ordered` at all, which the clean-room proof confirms by never
reaching for them.

For part 3: do not adopt the clean-room proof itself. Read fairly against
`drop_order`'s own reach (69 theorems, about 1,030 lines, not the whole
module's 73 theorems and 1,065 lines), the clean room is shorter: 48
theorems and 743 code lines, close to 30 percent fewer on both counts. So
size is not the reason to decline it. The reasons are reinvention and
quality: 25 of its 48 theorems already exist in `TraceOrder.lean` in an
exact or skeleton-equal shape, and it is not committed-quality — a tactic
macro under `set_option hygiene false`, doc-comments on only one of its 48
theorems, and single-letter case patterns throughout. `Config.Ordered` and `reachable_ordered` cannot simply
follow `drop_order` out of the module on this finding, though: `Witnesses.lean`'s
`swappedMarkers_rejected` reads `Config.Ordered` directly (`swappedMarkers.Ordered`,
`Kont.Ordered`), to show that nesting, not per-cell order, is what a sibling
scope's drop order rests on, so `drop_order` is not `Config.Ordered`'s only
consumer and the definition stays. What the clean-room run does support:
`Config.Nested` alone is enough for `drop_order`'s own stack half, the way the
clean-room proof has it, with no separate use of `Config.Ordered` there — one
data point for a later pass that asks whether `TraceOrder.lean`'s proofs of
`drop_order` and `reachable_ordered` need to sit as close together as they do
now, not a change to make on its own. Detour 1 is closed by this run, not
merely left untried again: no further attempt at a `Step`-only within-value
proof should be made before RUE-2423 lands.

## First pass (RUE-2471 part 3)

Done on `TraceExact`, from trunk `815b9b7ed`, measured by
`python3 bin/simplify-metrics.py` before and after. Every Spec statement is
unchanged (`spine-fingerprints.txt` matches), Comparator re-certifies, the
trusted-base lint, the layering audit and `leanchecker` pass, and every
theorem of the module uses at most `propext` and `Quot.sound`, as before:
none gained `Classical.choice`.

### The delta

| Measure | Before | After | Change |
| --- | ---: | ---: | ---: |
| Lines | 2944 | 2540 | −404 (13.7%) |
| Code lines | 2367 | 1960 | −407 (17.2%) |
| Theorems | 99 | 95 | −4 |
| Build wall s (median of 3) | 2.40 | 2.31 | noise |
| Build CPU s | 8.78 | 6.90 | −1.88 (21%) |
| `lake env lean`, three runs, wall s | 2.40, 2.35, 2.46 | 2.31, 2.25, 2.24 | noise |
| Heartbeat floor (`-DmaxHeartbeats`) | builds at 12000, fails at 11000 | builds at 13000, fails at 12000 | +1000, in `eval_quiet` |
| `set_option`, warnings | 0, 0 | 0, 0 | |
| `drop_exactly_once` helpers: all, their lines, own, MAP | 369, 6457, 135, 148 | 365, 6065, 131, 136 | −4, −392, −4, −12 |
| `rest_exactly_once` helpers: all, their lines, own, MAP | 372, 6521, 138, 151 | 368, 6129, 134, 139 | −4, −392, −4, −12 |
| `whole_program_exactly_once` helpers: all, their lines, own | 568, 10267, 247 | 564, 9875, 243 | −4, −392, −4 |
| Skeleton twin pairs in the module | 3 | 1 | −2 |
| `lean4-skills-find-golfable` (filtered) | 6 directness, 1 conditional | 0 directness, 1 conditional | |
| `lean4-skills-find-exact-candidates` | 16 low | 10 low | |

The four fuel inductions, in lines per declaration: `eval_quiet` 226 to
142, `eval_exact` 300 to 162, `rest_step` 323 to 313, `eval_tidy` 226
(unchanged); 1,075 to 843 together.

### What was done

- `eval_exact`: every form led by one operand goes through one local helper
  (the operand's ledger by `Exact.bind`, then the rest by `rest_step`), so its
  cases are one per shape of lead rather than one per form.
- The twin pairs: `EvalRes.bind_noRet`/`bind_noBrk` are one `EvalRes.bind_of`
  and `evalArgs_noRet`/`evalArgs_noBrk` one `evalArgs_abort_of`, each
  parametrized by a property of results that a prefixed trace keeps (`NoRet`
  or `NoBrk`, or both). `eval_quiet` proves its two halves by one induction
  over a selector `b` for the excluded result, and proves its list membership
  itself, so `Expr.returnsList_mem` and `Expr.breaksList_mem` are gone.
- `evalArgs_exact` is proved by its own recursion, and
  `evalArgs_exactQuiet`, which repeated its step, is gone: the "aborts
  quietly" half is `evalArgs_abort_of`.
- The `Tidy` family: `Tidy.scoped` and `Tidy.call` share `Local.lift`,
  `Tombstoned.lift` and `Tidy.teardown` (a scope of fresh cells opened above
  a record and torn down after its value), in place of two copies of the same
  reasoning (56 and 46 lines, now 24 and 17). `dropRetire_shape` carries the
  cell's existence, so `dropRetire_live` is gone.
- Local: `Exact.assign` is the old `Exact.assignDyn`, a static place being
  the empty dynamic path; `Contents.setAt_own_eq`'s struct and array cases,
  `Exact.prefix`'s value and `return` cases and `rest_step`'s three unary
  operators are one case each; `pendingSafe_needed` reuses
  `lostActivation_typing` and `lostStore_cc`, which proved the same two facts
  again inside it.
- Outside the module: `TraceWhole` calls `evalArgs_abort_of` where it called
  the two twins. `DIGEST.md`, `TRUST.md`, `INDEX.md` and `MAP.md` are
  regenerated; their diffs are the removed and replaced helpers, the new
  shared ones, `evalArgs_exact`'s binders (the list moved after the colon for
  the recursion) and `dropRetire_shape`'s added conjunct.

### What was not done, and why

- `eval_exact` and `eval_tidy` sharing one induction. `eval_exact`'s
  per-construct work is now in `rest_step`, reached through `Lead`, and what
  is left of it is the lead's scaffolding and the `use` and `@drop` cases;
  `eval_tidy` has no counterpart of `rest_step`. One induction over the
  conjunction would need a `Tidy` rest lemma first, and mixes the
  `pendingSafe` and copy-containment hypotheses with `φ.In`. The scaffolding
  it would share is now about 60 lines. Left for the generic
  evaluation-context lemma (structural 2), which would give both the same
  shape.
- `EvalRes.withTrace_noRet`/`withTrace_noBrk` remain a skeleton pair: they are
  the two instances of the parametrized lemmas' hypothesis, and `TraceWhole`
  uses them.
- The theorem count fell by 4, not about 10: nine went (the four twins, the
  two list-membership lemmas, `evalArgs_exactQuiet`, `dropRetire_live` and
  the static `Exact.assign`) and five came (the two parametrized lemmas and
  the three shared `Tidy` lemmas, which replace duplicated reasoning inside
  two proofs rather than whole theorems).
  `Tidy.same`, `Tidy.write`, `Tidy.opRes` and `Tidy.intro` stay: each is one
  line from `Local` and `Tombstoned`, and reading them off `Settled` does not
  shorten them.
- `rest_step`'s repeated four-line opening per form (twelve forms) would go
  with one more lemma (a `bind` of a value is injective in its rest); about
  seven lines net, so not taken.
- `Contents.own_struct_fresh`, `own_enum_fresh` and `own_array_fresh` have one
  proof for three constructors; one statement for all three needs vocabulary
  the package does not have.
- `evalUnOp_val_arg`, `evalIntCast_val_arg` and `evalFintrin_val_arg` pair
  with `Trace.lean`'s `evalUnOp_scalar` and the rest (an operator's value and
  its operand are scalars); `TraceWhole` uses both, so merging them is a
  follow-up outside this module.
- The heartbeat floor rose from 12000 to 13000: `eval_quiet`'s selector adds a
  little unfolding per case. The default budget is 200000.
