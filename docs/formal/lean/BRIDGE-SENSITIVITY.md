# Bridge sensitivity: would the bridge catch a broken compiler?

The bridge (ADR-0097; [README.md](README.md), "The bridge corpus") compares
the compiler with the Lean model on every corpus case. Its agreement rate
says the compiler and the model agree on those cases. It does not say the
cases would notice a compiler that is wrong. This page measures that
**sensitivity** (RUE-2464). We put known bugs back into scratch copies of the
compiler, one at a time, and ask whether the seed corpus or the generator
catches each one. This is mutation analysis (DeMillo, Lipton & Sayward,
"Hints on Test Data Selection", 1978) applied to the bridge's corpus, with
real, historical compiler bugs as most of the mutants.

We say a mutant is **caught** when at least one case disagrees with the model
under it. The mutation-testing literature says *killed*, and calls the
fraction killed the *mutation score*. [FIELD.md](../FIELD.md) does not list
those terms yet.

Measured on trunk `f4ac09fc9` (2026-09-25), before the three seeds this page
adds. The rule coverage is measured after them.

## Method

**Mutants.** A mutant is one of these:

* A historical fix from this project with its non-test hunks reverted. When a
  clean revert did not apply to current trunk, the mutant is the smallest
  patch that restores the old behaviour: a disabled check, or a restored
  condition. The bug's shape had to be expressible in the core fragment, or
  we note that it is not.
* A classic hand-written mutant in the lowering, codegen or sema. Examples:
  skip a drop, double a drop, reverse a drop order, drop a moved-out field,
  an off-by-one bounds check, a wrong overflow check.

Most mutants are one- or two-line patches; the clean reverts (`h2335b`,
`h2341`, `h2442`) are the fix's full non-test diff. None touches test files.

**Procedure, per mutant.** Each mutant ran in its own detached scratch
worktree, one at a time. We built the compiler and then ran three stages,
recording the first disagreement:

1. The **seeds**: the 171 seed cases, all of them.
2. **`--gen 200 --seed 7`**: the 200 generated cases, stopping at the first
   disagreement.
3. **`--gen 1000 --seed 23`**: the 1,000 generated cases, stopping at the
   first disagreement. This stage runs only if stage 2 found nothing.

Each case is compiled at the compiler's default level and run. We compare the
result with the Lean expectation, with the same test the loop's per-lane
check applies to the exported corpus. That test has four parts:

* The compiler must reject a case the checker rejects.
* A case the checker accepts must print the expected stdout and exit with the
  expected status.
* A trap must exit 101 after the expected stdout.
* A compiler crash or internal error on an accepted case counts as a
  disagreement. (An ICE on a checker-rejected case exits 1, which the
  per-case test treats as agreement, the same as any other rejection.)

When all three stages missed a mutant, we also ran the full bridge harness
(`rue-oracle-diff lean-corpus`, the program behind `scripts/rue lean-bridge`)
on the same 1,371 cases. The harness adds the reference interpreter
(`rue-oracle`), native code at O1, O2 and O3, and a trap-kind comparison. The
`array_elem_self_assign` seed (RUE-2346) disagrees on unmutated trunk too, so
we set it aside everywhere. RUE-2480's re-verification of the generated
corpus found two more unmutated disagreements at the standard settings,
`gen_7_3` and `gen_23_343` ("Observable destructors" below); both are set
aside the same way in that section's reruns.

**Checking the mutants themselves.** A mutant that changes no behaviour
(an *equivalent* mutant) would count as a miss that means nothing. So we ran
every mutant that nothing caught on its bug's own reproducer:

* the fix's spec case, or
* the issue's repro.

Three candidate mutants failed that check, and we excluded them:

* The first drop-glue mutants patched `create_struct_drop_glue_function`
  (`crates/rue-compiler/src/drop_glue.rs`). Nothing in the pipeline calls
  that function; the glue comes from `synthesize_canonical_drop_glue`. We
  re-ran both mutants there (the "v2" rows).
* The RUE-2438 mutant (the call-operand check disabled): not shown live. Its
  bug's repro is still rejected (redundant checks), and its shape is outside
  the fragment. Only one repro was checked; RUE-2438's fix spans 8 commits
  and several slots, and this mutant disables just one call-operand check,
  backed up by the other slot checks and RUE-2439's constructor-head
  reduction. Its shape (inline-import constructor heads) is also outside the
  fragment.

**Programs to detection** counts through the generated stream in order: 200
cases at seed 7, then 1,000 at seed 23. So `gen_23_181` is program 382.

## Results

"Own seed" means the seed that catches the mutant was added when the bug was
fixed, as its regression case. A catch by the bug's own seed is the corpus
remembering the bug, not finding it. The column after it asks whether anything
else would have found it.

| # | Mutant | Source and patch | Shape, in the fragment? | Caught? | First seed (its position in 171) | Caught by another seed or the generator? | Programs to detection (generated) | What disagreed |
|---|---|---|---|---|---|---|---|---|
| 1 | `h2318` | RUE-2318: `multiplier_shift` takes `2^(bits-1)` for a positive power of two at a signed width (codegen) | `i64::MIN * -1`; yes | yes | `i64_min_times_neg1` (41), own seed | no: generator 0 of 1,200 | — | native exits 0 with `MIN`; the model traps with overflow |
| 2 | `h2345` | RUE-2345: `frame_place_has_no_storage` sees only a zero-sized root, not a zero-sized field on the path (codegen) | dynamic index into a zero-length array field; yes | yes | `array_zero_length_field_dyn_read` (154), own seed | yes: `gen_23_500` | 701 | the compiler panics (ICE at `place_lower.rs`) |
| 3 | `h2335` | RUE-2335, root half: a declared-linear destructure of a root skips the moved-descendant check (sema) | three declared-linear levels, `@drop(y.x0)` after `y.x0.x0.x0`; yes | **no** | — | no, not even the full harness | — | (the mutant accepts the program and runs a destructor twice: destructor lines `2 4 2 3`) |
| 4 | `h2335b` | RUE-2335, drop half: `d7eda72e6` reverted, so `@drop` of a declared-linear ancestor counts its own obligation as residue (sema) | `@drop` of an ancestor after an inner destructure; yes | yes | `destructure_ancestor_dropped` (101), own seed | no | — | the compiler rejects (E0406) a program the checker accepts |
| 5 | `h2341` | RUE-2341: `b3aae3230` reverted, the partially-moved-array write rule keyed on the root (sema) | write into a field-reached array with a moved-out part; yes | yes | `array_dyn_write_after_destructure_via_field` (148), own seed | no | — | the compiler accepts a program the checker rejects |
| 6 | `h2344` | RUE-2344: `field_path` restarts at a dynamic index instead of stopping (sema) | `h.a[i].b` checked as `h.b`; yes | yes | `array_dyn_write_after_field_move` (149), own seed | no (the README records `gen_2_1694` at seed 2) | — | the compiler accepts a program the checker rejects |
| 7 | `h2442` | RUE-2442: `29b24b0f1` reverted, so a named array-repeat count is not validated (sema) | `const N = -1; [7; N]`; **no**: no named constants in the fragment | **no** | — | no | — | (the mutant compiles the repro, verified by hand) |
| 8 | `h2380` | RUE-2380: value forwarding re-roots a moved owned value at a reinitializable local (CFG opt) | loop-carried local moved out and reinitialized in one iteration; yes | **no** | — | no, not even the full harness at O2/O3 | — | (the mutant ICEs at `-O2` on the spec case, verified by hand) |
| 9 | `h2450` | RUE-2450: the drop-flag discipline skips a non-candidate compiler-owned slot's write (CFG verifier) | conditional drop after a discarded zero-width temporary; yes | yes | `loop_drop_after_zero_width_temp` (165), own seed | no | — | ICE (E9000, CFG verification) |
| 10 | `h2347` | RUE-2347: the drop-flag exemption needs the clearing in the drop's own block (CFG verifier) | `match` in one `if` arm on an affine scrutinee; yes | yes | `enum_match_one_arm_affine` (75), own seed | yes: `gen_23_181` | 382 | ICE (E9000) |
| 11 | `h2290` | RUE-2290: the verifier gets no drop-flag exemption at all (CFG verifier) | any conditional drop of an owner; yes | yes | `cond_drop_affine` (20), 9 seeds | yes: `gen_7_117` | 118 | ICE (E9000) |
| 12 | `h2449` | RUE-2449: `return {`/`break {` never take the block as their operand (parser) | `return { … }` as a block's tail; yes | yes | none of the 171 | yes: `gen_23_444` | 645 | the compiler rejects (E0206) a program the checker accepts |
| 13 | `c-skip-overwrite-drop` | `emit_overwrite_drop` emits nothing (CFG build) | whole-local reassignment of a live destructor-bearing value; yes | yes | `affine_overwrite` (15), 4 seeds | no: generator 0 of 1,200 | — | stdout misses the old value's destructor line |
| 14 | `c-double-field-drop` v2 | struct drop glue drops each droppable field twice (`synthesize_canonical_drop_glue`) | any struct with a destructor-bearing field; yes | yes | `struct_linear_field_dropped` (24), 33 seeds | yes: `gen_7_2` | 3 | a destructor line printed twice |
| 15 | `c-reverse-field-drops` v2 | struct drop glue drops fields in reverse declaration order (`synthesize_canonical_drop_glue`) | two destructor-bearing fields; yes | yes | `struct_field_drop_order` (26), 6 seeds | yes: `gen_7_2` | 3 | destructor lines out of §6.11's order |
| 16 | `c-reverse-scope-drops` | a block's scope exit drops its locals first-declared first (CFG build, block scopes only) | two destructor-bearing locals in an inner block; yes | yes | `enum_two_payload_bindings` (80), 1 seed | no: generator 0 of 1,200 | — | destructor lines swapped |
| 17 | `c-drop-moved-field` | scope exit drops the whole value even when a field was moved out (CFG build) | partial move, then scope exit; yes | yes | `partial_move_residue` (54), 15 seeds | yes: `gen_7_2` | 3 | ICE (E9000): the verifier sees the moved field dropped |
| 18 | `c-bounds-off-by-one` | the bounds check compares against `len + 1` (codegen, both targets) | dynamic index at exactly the length; yes | yes | none of the 171 | yes: `gen_7_117` | 118 | native reads past the end and exits 0; the model traps (bounds) |
| 19 | `c-overflow-signedness` | 32/64-bit checked arithmetic uses the other signedness's overflow flag (codegen) | any signed overflow; yes | yes | `overflow` (8), 2 seeds | yes: `gen_7_83` | 84 | a missed trap, and a spurious one |
| 20 | `c-overflow-kind` | the overflow trap calls the divide-by-zero helper (codegen) | any arithmetic overflow; yes | yes, **by the full harness only** | 6 seeds (`overflow` first) | yes: `gen_7_7` | 8 | `lean <-> native` and `oracle <-> native`, trap kind; the stdout-and-exit test cannot see it |
| 21 | `c-unguarded-flag-drop` | a path-dependent drop ignores its drop flag (CFG build) | a move in one `if` arm; yes | yes | `cond_drop_affine` (20), 6 seeds | yes: `gen_7_117` | 118 | ICE (E9000) |

Excluded: `c-double-field-drop` and `c-reverse-field-drops` (v1, dead code —
truly equivalent) and `h2438` (not shown live, on weaker evidence). See
"Checking the mutants themselves" above.

### Detection rate

| Measure | Caught | Rate |
|---|---:|---:|
| All 21 mutants, seeds + 1,200 generated, per-case test plus the full harness on misses | 18 | **86%** |
| All 21, per-case stdout-and-exit test only | 17 | 81% |
| The 20 mutants whose shape is in the fragment | 18 | 90% |
| The 20, after this page's three seeds (the seeds were written for these mutants; h2380's is caught only at -O2 or by the harness) | 20 | 100% |
| The 11 in-fragment historical mutants, **not counting a bug's own regression seed** | 4 (`h2345`, `h2347`, `h2290`, `h2449`) | 36% |
| The 9 classic mutants | 9 | 100% |
| The generator alone (1,200 programs), all 21 (one, `c-overflow-kind`, only by the full harness) | 11 | 52% |

The two misses in the fragment were `h2335` (root half) and `h2380`. The
third miss, `h2442`, is outside the fragment. The generator's catches all
came within 701 programs; eight of its eleven came within the first 200. On
this machine one pass of 171 seeds plus 1,200 generated programs takes about
three minutes of compile-and-run at four jobs. The full harness takes about
twelve.

### What the numbers say

* **The seeds are strong regression memory and a weaker detector.** Seven of
  the eleven were caught by nothing but their own regression seed (five:
  `h2318`, `h2335b`, `h2341`, `h2344`, `h2450`) or not at all (two: `h2335`,
  `h2380`). Those seeds do their job, since a reintroduced bug fails at once.
  But the corpus as it stood before each bug would have caught only four of
  the eleven.
* **The generator reaches ownership and drop-glue bugs quickly, but not
  every shape.** It catches every drop-flag and verifier mutant in 118
  programs or fewer, and the glue mutants in 3. Within 1,200 programs it
  never reached:
  * an overwrite-drop that prints: only 2 of the 684 accepted generated
    programs have one (`gen_7_112`, `gen_23_890`), and both are element
    writes, which the whole-local mutant does not touch. The coverage
    section below has the cause: few generated destructors print anything;
  * two destructor-bearing locals ending in the same inner block;
  * `i64::MIN * -1`;
  * three declared-linear levels;
  * a move-then-reinitialize inside a loop at an affine type.
* **Some mutants are invisible to the per-case stdout-and-exit test.**
  * `c-overflow-kind` changes only which trap fires. The exit code is still
    101 and the message is on stderr. Only the full harness's `lean <->
    native` comparison sees it.
  * `h2380` shows only at `-O2` and `-O3`. The per-case test compiles at the
    default level, so it cannot see this mutant even with a seed; the new
    seed `loop_move_out_then_reinit` is caught by the harness's O2/O3
    compile lanes (`checker <-> compiler [O2]`) and by the per-case test at
    `-O2`.

## Rule coverage

We rebuilt, for every case, the checker's derivation (`Explain.explain`,
one node per §5 rule) and the interpreter's step table
(`Explain.runTrace`, one row per §6 rule). We then counted the cases whose
derivation or run uses each rule. These are the same trees and tables that
`lake exe ruecore-explain` renders; `explain_result` and `traceEval_res` prove
that they agree with `check` and `eval`.

Each cell is *cases (accepted cases)*. Only an accepted case's run is
compared with the compiler's output. A rejected case contributes only its
verdict. The seed column counts the 174 seeds, this page's three included.

A row marked *where a destructor prints* counts the steps whose drop runs a
destructor that prints a line. Only those steps can reveal a missing,
doubled or reordered drop: a destructor prints `self.x0`, and it prints
nothing when `x0` is not an integer ([README.md](README.md); `Print.lean`).
Most rows name the rule as the trace does. Some carry a sub-case in
parentheses, and the `(mint #n)` identities are merged into one row.

#### §5 derivation nodes (the checker, `Explain.explain`)

| Rule, as the trace names it | Seeds (174) | `--gen 200 --seed 7` | `--gen 1000 --seed 23` |
|---|---:|---:|---:|
| (@Drop) §5.3 | 60 (42) | 46 (9) | 225 (36) |
| (@Drop) §5.3 at a declared-linear plan | 3 (2) | 6 (1) | 26 (5) |
| (@Drop-Copy) §5.3 | 1 (1) | 53 (38) | 260 (172) |
| (@Drop-Copy) §5.3 below a dynamic index, with (Use-Untrackable-Dynamic-Copy) §5.1's premises | 1 (1) | 9 (5) | 63 (32) |
| (@Drop-Copy)/(@Drop) §5.3 | 0 (0) | 1 (0) | 11 (0) |
| (Arith) §5.8 | 31 (30) | 91 (49) | 416 (221) |
| (Array-Intro) §5.8 | 36 (29) | 90 (45) | 401 (210) |
| (Array-Intro) §5.8, through §2's repeat elaboration | 1 (1) | 24 (17) | 99 (72) |
| (Assign) §5.2 below a dynamic index | 8 (6) | 14 (9) | 52 (37) |
| (Assign) §5.2, 3.8:72, 7.1:46 | 2 (0) | 2 (0) | 4 (0) |
| (Assign) §5.2, 3.8:77 | 20 (15) | 77 (43) | 345 (170) |
| (BitNot) §5.8 | 1 (1) | 3 (2) | 26 (20) |
| (Break) §5.7 + (Sub-Never) §5.7 | 12 (9) | 83 (45) | 368 (179) |
| (Call) §5.8 | 25 (21) | 0 (0) | 0 (0) |
| (Dbg) §5.8 | 55 (52) | 27 (15) | 124 (65) |
| (Enum-Intro) §5.5 | 19 (15) | 112 (55) | 587 (265) |
| (Float-Arith) §5.8 | 9 (9) | 7 (6) | 28 (24) |
| (Float-Cast) §5.8 | 2 (2) | 3 (3) | 11 (10) |
| (Float-Neg) §5.8 | 5 (5) | 2 (1) | 17 (17) |
| (Float-Ord) §5.8 | 3 (3) | 8 (5) | 30 (14) |
| (Float-Round) §5.8 | 2 (2) | 3 (1) | 20 (19) |
| (Float-To-Int) §5.8 | 4 (4) | 7 (5) | 16 (14) |
| (If) §5.5 join | 27 (18) | 134 (81) | 657 (357) |
| (Int-Cast) §5.8 | 2 (2) | 1 (1) | 17 (14) |
| (Int-To-Float) §5.8 | 1 (1) | 1 (1) | 15 (10) |
| (Let) §5.3 + the §5.6 scope-exit leak check | 125 (94) | 171 (90) | 835 (431) |
| (Let) §5.3, the body diverging | 5 (4) | 15 (15) | 52 (52) |
| (Lit) §5.8 | 173 (140) | 197 (113) | 978 (553) |
| (Loop-Break) §5.7 | 12 (9) | 88 (45) | 387 (179) |
| (Loop-Div) §5.7 | 1 (0) | 0 (0) | 0 (0) |
| (Match) §5.5 | 2 (0) | 41 (0) | 239 (0) |
| (Match) §5.5 join | 17 (14) | 46 (35) | 193 (149) |
| (Neg) §5.8 | 1 (1) | 2 (2) | 10 (9) |
| (Not) §5.8 | 1 (1) | 3 (2) | 31 (17) |
| (Ord) §5.8 | 12 (11) | 84 (45) | 348 (174) |
| (Panic) §5.8 + (Sub-Never) §5.7 | 3 (3) | 9 (5) | 43 (27) |
| (Return-Value) §5.7 | 6 (5) | 25 (21) | 134 (105) |
| (Seq) §5.3, 3.8:64 | 115 (93) | 164 (89) | 802 (434) |
| (Struct-Intro) §5.8 | 124 (91) | 149 (75) | 713 (343) |
| (Total-Cmp) §5.8 | 2 (2) | 0 (0) | 1 (0) |
| (Use-Copy) §5.1 | 54 (51) | 111 (64) | 523 (290) |
| (Use-Copy)/(Use-Move) §5.1 | 0 (0) | 0 (0) | 4 (0) |
| (Use-Declared-Linear-Destructure) §5.1 | 16 (10) | 5 (1) | 30 (1) |
| (Use-Move) §5.1 | 35 (23) | 46 (12) | 283 (83) |
| (Use-Untrackable-Dynamic-Copy) §5.1 | 9 (9) | 24 (12) | 104 (56) |

#### §6 step rows (the interpreter, `Explain.runTrace`)

| Rule, as the trace names it | Seeds (174) | `--gen 200 --seed 7` | `--gen 1000 --seed 23` |
|---|---:|---:|---:|
| (D-Arith) §6.4 | 24 (23) | 59 (33) | 267 (141) |
| (D-Arith) §6.4, the unary case | 1 (1) | 2 (2) | 10 (9) |
| (D-Array) §6.5 | 36 (29) | 85 (42) | 368 (192) |
| (D-Array) §6.5, through §2's repeat elaboration (7.1:39) | 1 (1) | 18 (13) | 83 (58) |
| (D-Assign) §6.8 | 2 (0) | 0 (0) | 11 (1) |
| (D-Assign) §6.8 (overwrite-drop) | 6 (5) | 46 (25) | 207 (108) |
| (D-Assign) §6.8 (overwrite-drop), *where a destructor prints* | 9 (7) | 1 (1) | 3 (0) |
| (D-Assign) §6.8 (reinitialization, 3.8:55) | 7 (5) | 4 (1) | 28 (8) |
| (D-Assign) §6.8 below a dynamic index | 2 (0) | 2 (1) | 9 (5) |
| (D-Assign) §6.8 below a dynamic index (overwrite-drop) | 2 (2) | 6 (4) | 15 (12) |
| (D-Assign) §6.8 below a dynamic index (overwrite-drop), *where a destructor prints* | 4 (4) | 0 (0) | 1 (1) |
| (D-Bit) §6.4 | 1 (1) | 6 (4) | 31 (24) |
| (D-Bit) §6.4, the complement | 1 (1) | 4 (2) | 22 (18) |
| (D-Break) §6.10 | 12 (9) | 61 (37) | 263 (142) |
| (D-Break) §6.10 (unwind to the loop) | 11 (8) | 61 (37) | 263 (142) |
| (D-Break) §6.10 (unwind to the loop), *where a destructor prints* | 2 (2) | 0 (0) | 0 (0) |
| (D-Call) §6.9 (push the frame) | 174 (140) | 200 (115) | 1000 (569) |
| (D-Div) §6.4 | 4 (4) | 2 (2) | 21 (15) |
| (D-EndScope) §6.6 (end the arm) | 9 (6) | 45 (25) | 240 (113) |
| (D-EndScope) §6.6 (end the arm), *where a destructor prints* | 8 (7) | 5 (4) | 14 (8) |
| (D-EndScope) §6.7 | 28 (14) | 73 (28) | 329 (101) |
| (D-EndScope) §6.7 (retire the binding) | 87 (71) | 125 (74) | 611 (362) |
| (D-EndScope) §6.7 (retire the binding), *where a destructor prints* | 40 (36) | 10 (8) | 44 (32) |
| (D-Enum-Intro) §6.6 | 19 (15) | 109 (53) | 569 (253) |
| (D-Float-Arith) §6.4 | 9 (9) | 5 (4) | 24 (21) |
| (D-Float-Cast) §6.4 | 2 (2) | 3 (3) | 10 (9) |
| (D-Float-Neg) §6.4 | 5 (5) | 2 (1) | 18 (16) |
| (D-Float-Ord) §6.4 | 3 (3) | 8 (5) | 30 (14) |
| (D-Float-Round) §6.4 | 2 (2) | 2 (1) | 19 (18) |
| (D-Float-To-Int) §6.4 | 1 (1) | 7 (5) | 13 (11) |
| (D-Float-To-Int-Trap) §6.4 | 3 (3) | 0 (0) | 0 (0) |
| (D-If-F) §6.6 | 16 (12) | 92 (55) | 405 (227) |
| (D-If-T) §6.6 | 20 (15) | 90 (52) | 432 (254) |
| (D-If-T)/(D-If-F) §6.6 | 0 (0) | 6 (2) | 60 (5) |
| (D-Index) §6.5 | 7 (7) | 12 (4) | 60 (34) |
| (D-Index-Trap) §6.5 | 8 (8) | 19 (11) | 68 (35) |
| (D-Int-Cast) §6.4 | 1 (1) | 1 (1) | 16 (12) |
| (D-Int-Cast-Trap) §6.4 | 1 (1) | 0 (0) | 0 (0) |
| (D-Int-To-Float) §6.4 | 1 (1) | 1 (1) | 10 (6) |
| (D-Let) §6.7 | 5 (1) | 21 (4) | 108 (18) |
| (D-Let) §6.7 (mint the binding) | 129 (97) | 161 (84) | 789 (416) |
| (D-Loop-Iter) §6.10 | 2 (0) | 18 (3) | 63 (10) |
| (D-Loop-Iter) §6.10 (re-enter the body) | 7 (5) | 26 (15) | 123 (68) |
| (D-Match) §6.6 | 1 (0) | 12 (1) | 77 (0) |
| (D-Match) §6.6 (bind the arm's payload) | 18 (14) | 62 (30) | 336 (133) |
| (D-Panic) §6.12 | 2 (2) | 4 (2) | 16 (10) |
| (D-Return) §6.9 | 0 (0) | 0 (0) | 1 (0) |
| (D-Return) §6.9 (unwind the frame) | 1 (0) | 21 (17) | 91 (69) |
| (D-Return) §6.9 (unwind the frame), *where a destructor prints* | 2 (2) | 0 (0) | 1 (1) |
| (D-Return)/(D-Return-Main) §6.9 (the callee's return is the call's value) | 2 (2) | 21 (17) | 92 (70) |
| (D-Return-Value) §6.9 (pop the frame) | 154 (120) | 154 (83) | 819 (450) |
| (D-Return-Value) §6.9 (pop the frame), *where a destructor prints* | 4 (4) | 0 (0) | 0 (0) |
| (D-Seq) §6.7 | 116 (93) | 155 (85) | 754 (410) |
| (D-Seq) §6.7 (drop the temporary) | 0 (0) | 12 (9) | 60 (44) |
| (D-Seq) §6.7 (drop the temporary), *where a destructor prints* | 1 (1) | 5 (4) | 15 (12) |
| (D-Shl) §6.4 | 1 (1) | 2 (1) | 5 (4) |
| (D-Shr) §6.4 | 0 (0) | 2 (2) | 11 (5) |
| (D-Struct) §6.5 | 124 (91) | 134 (71) | 673 (321) |
| (D-Total-Cmp) §6.4 | 2 (2) | 0 (0) | 1 (0) |
| (D-Use-Copy) §6.3 | 57 (50) | 105 (59) | 451 (258) |
| (D-Use-Copy)/(D-Use-Move) §6.3 | 3 (0) | 15 (0) | 96 (0) |
| (D-Use-Declared-Linear) §6.3 | 6 (3) | 3 (0) | 18 (1) |
| (D-Use-Declared-Linear) §6.3, *where a destructor prints* | 10 (7) | 0 (0) | 1 (0) |
| (D-Use-Move) §6.3 | 35 (23) | 34 (8) | 209 (62) |
| (Dbg) §5.8, the observable output of §6.12 | 56 (52) | 22 (11) | 88 (47) |
| (Panic-Lift) §6.2 | 24 (24) | 25 (15) | 89 (49) |
| @drop §6.11 | 8 (4) | 24 (4) | 121 (17) |
| @drop §6.11 (Copy: no glue) | 1 (1) | 32 (21) | 151 (108) |
| @drop §6.11 at a Copy place below a dynamic index | 1 (1) | 5 (2) | 45 (21) |
| @drop §6.11 at a declared-linear plan (§6.3) | 0 (0) | 4 (1) | 17 (3) |
| @drop §6.11 at a declared-linear plan (§6.3), *where a destructor prints* | 3 (2) | 0 (0) | 0 (0) |
| @drop §6.11, *where a destructor prints* | 50 (36) | 5 (3) | 24 (3) |
| literal §6.3 | 164 (131) | 197 (113) | 966 (544) |
| literal §6.3 (3.12:9 rounds the decimal) | 18 (18) | 47 (38) | 233 (168) |
| ordering compare §6.4 | 12 (11) | 72 (39) | 289 (148) |
| §6.4's `not` on bool | 1 (1) | 3 (2) | 29 (15) |
| §6.4's remainder arm, beside (D-Div) | 2 (2) | 2 (0) | 11 (9) |

### Never exercised

Each rule named in §5 or §6 of the calculus falls in one of three groups.

**Outside the fragment.** No case can reach these, and that is a scope fact.
The bridge says nothing about these rules:

* (Eq) §5.8 and (D-Eq) §6.4: equality borrows its operands, and the
  fragment's `BinOp` has no `==`;
* (Accessor-Call) §5.8: accessors return places (ADR-0062);
* (D-Use-Shared-Read) §6.3: the fragment has no borrows.

**Exercised, but the trace names them differently.** These are exercised;
only their row label differs:

* (D-Arith-Trap), (D-Div-Zero) and (D-Div-Overflow) §6.4 appear as
  (D-Arith) and (D-Div) rows followed by a trap outcome. The seeds end in
  an overflow trap 9 times and a divide-by-zero 3 times; the generated cases
  end in an overflow trap 5 times and a divide-by-zero once.
* (D-Use-Untrackable-Dynamic-Copy) §6.3 is the (D-Index) row.
* (D-Loop-Enter) §6.10 has no row of its own; the first body row follows the
  `loop`.
* (Fn) §5.8 is `checkProgram`'s per-function check, which runs on every case
  but emits no node.
* `Holder`, `OrdinaryDynamic` and `Owned-Base` §5.1 are judgments inside
  other rules' premises, not derivation nodes.

**In the fragment, and never exercised.** No seed and no generated case
reaches these. The divergence rules below type a form *after* a diverging
operand; the generator never draws syntax after a `return`, `break` or
`@panic` in the same block (RUE-2376). The loop rule needs a loop no break
leaves.

* (Seq-Bottom), (Let-Bottom) and (Strict-Bottom) §5.3;
* (Call-Bottom) §5.3 and (Panic-Operand) §5.7, which `Explain` has no row
  for;
* (Return-Bottom) and (Loop-Div-Backedge) §5.7;
* (Loop-Div) §5.7, which only one seed exercises, and that seed is rejected.

**Exercised by the seeds only.** These rules have zero generated cases:

* (Call) §5.8: every generated program is a single function, so the
  generator never passes an argument, drops a by-value parameter, or returns
  through a call. The (D-Call) and (D-Return-Value) rows it does reach are
  the entry call alone.
* (D-Int-Cast-Trap) and (D-Float-To-Int-Trap) §6.4, (D-Total-Cmp) §6.4 on an
  accepted case, a `break` unwind that runs a printing destructor, a frame
  pop that runs one, and (D-Use-Declared-Linear) §6.3 with a printing
  residue on an accepted case.

**Observable drops are rare in generated programs.** Only 14 of the 115
accepted programs at seed 7, and 51 of the 569 at seed 23, print any
destructor line. Among the seeds it is 76 of 140. A drop that prints nothing
is invisible to the bridge. That is why two drop mutants got past 1,200
generated programs: `c-skip-overwrite-drop` and `c-reverse-scope-drops`. The
same mutants are caught by the seeds, where destructors print. `h2335`'s
miss is different: it is a sema-acceptance mutant, caught by no existing
seed and by nothing that prints a destructor; the new seed catches it
through the verdict (the compiler accepts what the checker rejects), and
the generator missed it for its shape, not because drops are invisible.
RUE-2480 (below, "Observable destructors") fixes the generator so this is no
longer true — every generated destructor now prints — and rechecks all four
of these mutants against the fixed generator.

## Observable destructors (RUE-2480)

The measurement above found the cause: `Print.structItem` prints a
destructor's `@dbg` only when the declaration's first field is an `int`, and
`genDecl` drew that field like any other, so only some destructor-bearing
declarations happened to have one. `genDecl` (`Gen.lean`) now draws
`drawnDtor` *before* the fields and forces field 0 to a plain `int` whenever
a destructor is coming — never one of `fieldTy`'s array wraps, a struct or an
enum — which can only ever pull the field join away from `Linear`, never
toward it (`3.9:44`'s condition is unaffected). Reordering the draw changes
every case at every seed from that point on, so this also regenerates the
corpus (README.md's "Generated programs" section has the case-identity
fallout).

**Share of accepted generated programs that drop anything and print a
destructor line**, before and after, measured the same way as above (a
one-off tool over `Explain`'s trace, not committed):

| Setting | Before | After |
|---|---:|---:|
| Seeds (193 cases) | 74 of 74 (100%) | 74 of 74 (100%) |
| `--gen 200 --seed 7` | 13 of 26 (50%) | 16 of 16 (100%) |
| `--gen 1000 --seed 23` | 48 of 127 (37%) | 114 of 114 (100%) |

The seeds were already fully observable (hand-written to have an `int` first
field); only the generator was blind, and now is not.

**Rechecking the three mutants this was filed for, plus `h2335b`**, with
`drill.sh` against the regenerated corpus. The first pass wrongly credited
the fix with catching all four at `gen_7_3` — the program that turned out to
be `array_elem_self_assign`'s shape, disagreeing on the *unmutated* compiler
too (below); once `gen_7_3` (and `gen_23_343`, the other unmutated
disagreement this turned up) are set aside, one real catch remains:

| Mutant | Before (this page) | After RUE-2480 | Programs to detection |
|---|---|---|---:|
| `c-skip-overwrite-drop` | seeds only (`affine_overwrite`, 4 seeds); generator 0 of 1,200 | seeds unchanged; **generator catches it**, `gen_23_689` | 890 |
| `c-reverse-scope-drops` | seeds only (`enum_two_payload_bindings`, 1 seed); generator 0 of 1,200 | unchanged: seeds only, generator 0 of 1,200 | — |
| `h2335` (root half) | seeds only (`destructure_root_through_moved_part`, 1 seed); generator 0 of 1,200 | unchanged: seeds only, generator 0 of 1,200 | — |
| `h2335b` | seeds only (`destructure_ancestor_dropped`, 1 seed); generator 0 of 1,200 | unchanged: seeds only, generator 0 of 1,200 | — |

Observability was necessary but not sufficient for three of the four.
`c-reverse-scope-drops` needs two destructor-bearing locals ending the *same*
inner block (a follow-up below); a quick check of the fixed generator's own
traces (`--gen 1000 --seed 23`) finds 20 programs with a pair of consecutive
`.dtor` events at all, and of those pairs 28 of 50 already print two
*different* lines (so a swap would be visible) — the miss looks like the
draw not reaching this mutant's exact shape (a block's own scope-exit order,
not an enum arm's or a struct's field-drop order) within 1,200 programs,
rather than a values-collide problem. `h2335` and `h2335b` are unaffected by
observability at all, matching the original follow-up: they need three
declared-linear levels, a different generator capability.

**Two new unmutated disagreements**, found while confirming the regenerated
corpus still agrees with the compiler (methodology, above) — reported here,
not fixed:

* `gen_7_3` (`--gen 200 --seed 7`, program 4): the checker rejects it; the
  unmutated compiler accepts and runs it (exit 0). Its source builds
  `[[S0; 3]; 2]` (an array of arrays of a destructor-bearing struct) and then
  writes `v0[0] = v0[0]` — the same shape as the `array_elem_self_assign`
  seed (RUE-2346, RUE-228: the model refuses the write into the
  self-move-holed array, `3.8:72`/E0480, and the compiler accepts on
  purpose). Not a new bug: README.md's "Generated programs" section already
  documents this shape reaching these settings, masked by an unrelated E0406
  until now; RUE-2480's reordered draw is what unmasks it here.
* `gen_23_343` (`--gen 1000 --seed 23`, program 344): both accept it, and the
  compiler reports an internal error — `E9000`, CFG verification: "reads
  already-consumed owner root `Local { slot: 0, ... }`" — where the model
  runs it to `6, 1`. Its source moves a destructor-bearing `S2` into a `let
  mut v0`, loops with a conditionally-taken `@drop(v0); break` on one arm,
  and then writes `v0.x1 = v0.x1` (a struct **field** self-assignment, not
  RUE-2346's array-element one) on the arm that does not drop. A genuinely
  new finding, different in shape (field, not element) and in symptom (an
  ICE, not an unsound accept) from RUE-2346.

Both are set aside next to `array_elem_self_assign` when reproducing this
section's drills (below).

## Follow-ups

Every miss has a follow-up. Mutants caught only by their own seed, and one
blind spot, get a generator or tooling proposal too.

| Mutant | Follow-up | Kind |
|---|---|---|
| `h2335` (root half) | `destructure_root_through_moved_part`: three declared-linear levels, `@drop(y.x0)` after `y.x0.x0.x0`. The checker rejects it; the mutant accepts it | seed, added here |
| `h2380` | `loop_move_out_then_reinit`: a counted loop that moves `mut b` into `t` and reinitializes `b`. The mutant ICEs at `-O2`; the harness's O2/O3 compile lanes, and the per-case test at `-O2`, catch it | seed, added here |
| `c-bounds-off-by-one` (no seed caught it) | `array_bounds_trap_at_len`: reads at `len - 1` and then at `len`. The other bounds seeds index further past the end, or into a zero-length array, which does not go through the length compare | seed, added here |
| `h2442` | named constants and comptime parameters are outside the fragment | scope note |
| `c-skip-overwrite-drop`, `c-reverse-scope-drops` (generator 0 of 1,200) | done (RUE-2480, above): every generated destructor-bearing struct gets an integer `x0`. Caught `c-skip-overwrite-drop` (`gen_23_689`); `c-reverse-scope-drops` still needs its own shape (below) | generator issue, partly done |
| `c-reverse-scope-drops` (still generator 0 of 1,200 after RUE-2480) | generate two destructor-bearing locals ending the same inner block | generator issue |
| `h2335`, `h2335b` (generator 0 of 1,200; unaffected by RUE-2480) | generate three declared-linear levels | generator issue |
| `h2318` (own seed only) | draw boundary literals (`MIN`, `MAX`, `±1`, powers of two) as arithmetic operands | generator issue |
| (Call) §5.8 and the call-boundary drop paths (no generated case) | generate multi-function programs: by-value parameters, including destructor-bearing ones, and calls in operand position | generator issue |
| `c-overflow-kind` (the per-case test is blind to it) and `h2380` (default level only) | the loop's per-case check should also compare the trap kind (stderr's panic message) and compile at `-O2` as well as the default level, or the lane should run `scripts/rue lean-bridge`, which does both | tooling issue |
| The divergence rules (never exercised) | seed one accepted case for each of (Seq-Bottom), (Let-Bottom), (Strict-Bottom) and (Return-Bottom), with syntax after the diverging form where the compiler accepts it. Otherwise record that the bridge does not cover these rules | seed or scope note, for RUE-2376's owner |

## Reproducing

* `lake exe ruecore-corpus` exports the corpus with the seeds;
  `lake exe ruecore-corpus --gen 200 --seed 7` and `--gen 1000 --seed 23`
  export the generated cases (drop the seed cases from the front).
* Build each mutated compiler in a scratch worktree and compare each case
  with the per-case test described under "Method". Run
  `rue-oracle-diff lean-corpus --corpus <file>` for the full harness.
* The rule counts come from `Explain.programDerivs` and `Explain.runTrace`
  over the same cases.

Most mutants are one- or two-line patches against `f4ac09fc9`; the clean
reverts (`h2335b`, `h2341`, `h2442`) are the fix's full non-test diff. They
are kept with the run logs in the loop's scratch directory, and are not
committed. The 171-seed drill corpus there is regenerable from `f4ac09fc9`;
`chain.sh`'s own output later overwrote it with this page's 174-seed export,
since both land in the same `scratch/<worktree basename>` directory.
