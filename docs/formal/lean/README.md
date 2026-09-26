# RueCore — the Lean 4 mechanization

A machine-checked mechanization of a fragment of the Rue core calculus
(`../01-core-calculus.md`), proving the fragment's slice of the §7
memory-safety theorems in Lean 4. Adopted as the fourth view of the language
by ADR-0097 (`docs/designs/0097-mechanized-formal-core.md`); the spike that
motivated adoption, its findings, and the project outline (the milestone
ladder that grows this fragment) live in
`../../notes/lean-mechanization-spike.md`.

**Status: zero `sorry`, axioms `propext`/`Quot.sound` only**
(no `Classical.choice`, no `native_decide`; `TRUST.md` is the generated
evidence, and `DIGEST.md` is every statement it is evidence for). ADR-0097
fixes the theorem shape, the authority rule, and the non-blocking posture the
project "Formal core mechanization" grows this fragment under.

## Building

Through Buck, with the SHA-pinned toolchain the repository fetches itself
(`toolchains//lean`, no `elan` needed):

```bash
scripts/rue lean          # builds, re-checks with leanchecker, prints the axioms report
./buck2 build root//:lean-ruecore --show-simple-output
```

Or directly, for editor work, with `elan` bootstrapping the same pinned
toolchain from `./lean-toolchain`:

```bash
lake build
```

The two pins are held equal by `scripts/validate-lean-toolchain-pin.py`. The
Buck target is build-only and carries no test tier: nothing in CI runs the
Lean build until ADR-0097's gate is met (RUE-2241). CI does read these
sources: the premerge cross-reference gate below fails on an uncited
declaration or a stale `INDEX.md`.

## The bridge corpus (ADR-0097, RUE-2227)

`lake exe ruecore-corpus` (or the `corpus.json` output of `scripts/rue lean`)
prints every corpus case as JSON: the fragment program — a list of function
definitions — printed as a complete Rue module, the proved-sound checker's
verdict, and the model's interpreter's outcome at one fixed fuel bound (a case
that bound does not complete is left out rather than given an outcome).
`crates/rue-oracle-diff` consumes it (RUE-2228) and runs the compiler, the
oracle, and the native binary on each source. A disagreement means the
compiler, the oracle, the model, the spec or the printer is wrong, or it is
a pending decision (e.g. RUE-2346); a person decides which (RUE-305). One
exclusion: the checker types nothing past a `return` or `@panic` (§5.3's
`-Bottom` rules), so it accepts dead code the compiler may reject, as §5.3
allows; a program with syntax after a diverging form is outside the verdict
contract, and no case has one (`Checker.lean`, "Dead code").

Running the consumer needs neither `lake` nor `elan`: the corpus it reads is
the Buck target's own `corpus.json`.

How sensitive the corpus is, that is, whether it would catch a compiler
that is wrong, is measured in [BRIDGE-SENSITIVITY.md](BRIDGE-SENSITIVITY.md)
(RUE-2464). That page reintroduced historical compiler bugs and classic
mutants one at a time and recorded which seed or generated case caught each.
Whether the proofs, the witnesses, the corpus and the bridge would notice a
wrong *definition* is measured in [MUTATION.md](MUTATION.md) (RUE-2465):
80 one-rule mutants of the definition layer, run by `bin/mutate.py`, with
what each kill rests on.

```bash
scripts/rue lean-bridge                      # or: ./buck2 run //:lean-bridge
scripts/rue lean-bridge -- --case overflow   # one case
scripts/rue lean-bridge -- --report-json /tmp/bridge.json
```

### Generated programs (RUE-2229)

The seed cases are hand-written, so `RueCore/Gen.lean` also generates
programs: closed, well-scoped, and simply typed by construction, with
ownership left to chance, so the checker's verdict on each is recorded and
never filtered (two in five are rejected). The generator is a pure function
of its seed, and a case named `gen_<seed>_<i>` is the same in every run with
that seed and more than `i` cases. Its bias toward moves in one arm of an
`if`, linear values reaching scope exit, and reassignment after a move is
documented in the module.

A generated program may declare its own **enums** as well as its own structs,
and about three in four do (144 of 200 at `--gen 200 --seed 7`, 777 of 1,000
at `--gen 1000 --seed 23`); such a program also draws enum construction and
`match` — one arm per variant in declaration order, each arm a block over that
variant's payload locals, which it may move, `@drop`, read or leave. A little
under half the cases contain a `match` (89 of 200 and 431 of 1,000 at those two
settings), and a `match` whose scrutinee is a **place** rather than a temporary
is the majority of them (93 of 161 sites and 509 of 811), because a drawn
`match` half the time binds its scrutinee to a `let` first where the scope
holds no enum place.

A generated program may hold **arrays** too (RUE-2331): one field type in five
and one `let` binder type in five is `[T; n]`, `n ≤ 3` with `0` among them, and
one in four of those is nested. Constant indices are place steps like field
slots, so the use, `@drop` and assignment draws reach `a[c]`, `a[c].f`,
`h.arr[c]` and `a[c][c']`; places below a **dynamic** index — `a[i]`,
`a[i].f`, `h.arr[i].f`, `a[i][j]` — are read at a `Copy` leaf, written, and
`@drop`ped at a `Copy` leaf, with one index in five out of bounds. About half
the programs contain an array literal or repeat form (106 of 200, 482 of 1,000;
a literal alone 100 and 453, a repeat form 26 and 126), 66 and 294 an
index form, 52 and 220 a dynamic one, and the bounds trap ends 19 and 71 runs.
Each index is bound by a `let` before the form that uses it, because the
compiler folds a literal index in a block to a constant one and rejects an
out-of-range constant at compile time (E0902): a block that can be fully
evaluated at compile time is a constant index under `8.2:4`. That a
`let`-bound index stays dynamic rests on the compiler's current reading of
`8.2:4`'s open list, which is RUE-2349. The draw does not avoid the shapes of
the array red seeds, so a generated case of one is attributed by hand: the
self-assignment `a[c] = a[c]` (RUE-2346) is the one still red. On the current draws the acceptance settings (200 at seed 7, 1,000
at seed 23) reach the self-assignment in four cases (`gen_7_145`, `gen_7_159`,
`gen_7_181`, `gen_23_752`), each masked by an E0406 the compiler reports
first, so every one of the 1,200 cases agrees with the compiler; a wider run
reaches it unmasked (`gen_101_207` at `--gen 400 --seed 101`, on the draws
just before the loop generator's restoring statements), which is an allowed
RUE-2346 disagreement. On the draws before loops they reached the
self-assignment (one case at seed 7, three at seed 23) and a dynamic index into a zero-length
array field (five at seed 23, agreeing since RUE-2345 was fixed); wider runs at
other seeds reached RUE-2344's shape (`gen_2_1694`, agreeing since RUE-2344
was fixed) and two compiler defects found and filed from them, RUE-2347 (a
CFG verification error on a `match` in one `if` arm) and RUE-2348 (an
internal error on a float array bound inside an enum-valued block). Any other
generated disagreement is a finding to file.

A use or `@drop` is drawn through a struct declared `linear` exactly as through
any other (RUE-2339), so the checker, not the draw, picks §4.2's declared-linear
destructure: 22 of the 200 programs and 109 of the 1,000 contain one, the
checker accepts 2 and 6 of those, and the destructure's own linear-residue
premise (E0474) is the deepest refusal of 1 and 6. RUE-2335's shape — a
`@drop` of a declared-`linear` place after a destructure under it, which the
compiler rejected until RUE-2335 was fixed and the model accepts — is not
drawn around. None of those 1,200 cases has it, but the draw reaches it,
rarely (on the draws before loops, first at seed 1 in `gen_1_151382`, the only
one in the first 300,000).

A generated program may hold **loops** too (RUE-2330): half of them do (102 of
200, 480 of 1,000). A loop is either **counted** — a `mut` counter, a guard
`if k >= n { break }` first, `n ≤ 3` — or **once-through**, a body that ends
in a `break`, so every generated program terminates and none is left out of
the export for running out of fuel. Inside a loop body, `break` appears as a
whole arm of an `if` or a `match` (at most one per branch) or as the last form
of a once-through body, often right after a `@drop` of a binder from outside
the loop: RUE-1615's shape when every path breaks, RUE-1614's exit join when
another exit keeps the value. Loops nest (10 and 41 programs), and two draws aim at the moves a random body
seldom gets accepted: a counted body may open with a restoring statement
(reassign-then-move, or move-then-reassign at a linear type), and a
once-through loop may consume a linear binder on every exit. Two shapes are
drawn around, because each waits on a decision rather than being a finding:
syntax after a `break` in the same block (RUE-2376), and a loop body that is
not `unit` (RUE-2379). A `return` or `@panic` is drawn under the same
discipline as `break` (RUE-2383): as a whole `if` or `match` arm, a loop exit's
arm or the function body's last form, never in an operand, at most one per
branch. They read a separate random stream, so a program without one is the
program drawn before; about a quarter of the programs have one (48 of 200 at
seed 7, 275 of 1,000 at seed 23).

```bash
lake exe ruecore-corpus --gen 1000 --seed 7 > /tmp/gen.json   # seed cases, then 1000 generated
scripts/rue lean-bridge -- --corpus /tmp/gen.json               # the last --corpus wins
```

Without `--gen` the output is the seed corpus alone, which is what the Buck
target's `corpus.json` holds. A finding filed from a generated run records
the seed and the case name.

It prints a line per case, then — for each disagreeing case — the printed
program, the four views side by side, and the pair(s) that disagree, with a
tally at the end; `--report-json` writes the same findings as JSON so two runs
can be diffed. It exits non-zero when any disagreement exists.

**The seed corpus is red on one case, and that is the bridge working.**
`destructure_ancestor_dropped` was red until RUE-2335 was fixed: after
`y.x0.x0` destructures the inner declared-`linear` place, §5.3's (@Drop)
discharges the declared-`linear` **ancestor** `y` — `Σ(y) = Owned`, no
still-owned linear sub-place remains below it — and the model runs the
program, while the compiler reported E0406, reading `y`'s own obligation as a
residue below it. The compiler now accepts it and runs the model's trace.
`array_dyn_write_after_destructure_via_field` was red until RUE-2341 was
fixed: a declared-`linear` destructure at `h.arr[0].x0` holes an array reached
through a field, and a write below a dynamic index into it follows
(`h.arr[i].x0 = …`). The model refuses it (E0480, `fully-owned` at `h.arr`
fails). The compiler's E0480 check fired only when the root binding was an
array, so it accepted the program, ran the moved-out element's destructor
twice and leaked the written value; the check now keys on the outermost array
the write steps into, and the compiler refuses it with E0480 too.
`array_elem_self_assign` is the red one: `a[0] = a[0]` moves `a[0]` out on the
right-hand side, so the model refuses the write into the holed array
(`3.8:72`, E0480), while the compiler accepts it on purpose since RUE-228;
which is right is a decision, RUE-2346. `array_zero_length_field_dyn_read` was
red until RUE-2345 was fixed: a dynamic-index read from a zero-length array
field traps with `bounds` in the model and was an internal compiler error in
code generation; the compiler now traps too.
`i64_min_times_neg1` was red until RUE-2318 was fixed: `min_T * -1` at
`i64`, which §6.4's (D-Arith-Trap), `3.1:6` and `8.1:3` all make an overflow
trap and which the model traps on. The compiler lowered a checked multiply by
a constant power of two as a left shift checked by shifting back, and took
`i64::MIN`'s bit pattern, `2^63`, for one, so the product wrapped and the
program exited 0; a signed minimum now takes the ordinary checked multiply and
the compiler traps too.
`array_write_after_destructure_via_field`, the constant-index form
(`h.arr[0].x0 = …`), was red for the same reason until RUE-2344 made the
compiler refuse it with E0205 (the write's base `h.arr[0]` is consumed); since
RUE-2341 it reports `3.8:72`'s E0480.
`array_dyn_write_after_field_move` was red until RUE-2344 was fixed too: the
array field `h.x0` is moved out and dropped, and `h.x0[i].x0 = …` then writes
into it. The model refuses it (E0205 on `h.x0`); the compiler move-checked a
place below a dynamic index through a field against the wrong path, accepted
it, ran the destroyed element's destructor a second time and leaked the
written value. It now refuses it with the same E0205, and the case stays as
the regression signal. A red case is what the bridge is for; the model is not softened to
match the compiler.

The mode is a `buck2 run` entry point and belongs to no test tier, so nothing
in CI requests it until ADR-0097's gate is met (RUE-2241).

One case, abbreviated:

```json
{
  "name": "affine_scope_drop",
  "description": "An affine resource silently dropped at scope exit; ...",
  "rules": ["(Let) §5.6", "endscope §6.7"],
  "source": "// Case: affine_scope_drop\n// ...\nfn main() -> i32 { ... }\n",
  "verdict": {"accept": {"type": "i64"}},
  "expected": {"kind": "ok", "stdout": ["7", "1"], "exit": 0}
}
```

`verdict` is `{"accept": {"type": T}}` or `{"reject": {}}`. `expected` is
`ok` with the stdout lines the native binary must print (one line per
observable event the run executes — a user destructor or a `@dbg` — in trace
order, then the lines `main` shows for the program's value) and exit 0;
`panic` with the trap kind and the stdout lines the run produced **before**
the trap (`overflow`, `divZero`, `remZero`, `castOverflow`, `bounds` or `user`;
the compiler's runtime reports these as `error: integer overflow`,
`error: division by zero`, `error: integer cast overflow`,
`error: index out of bounds` and `panic: <message>`, each with exit status 101, and the value never prints);
or, for a rejected program, `stuck` with the refusal the machine would
reach, which the bridge cannot observe because the compiler rejects the
program first (the compiler's diagnostics for the seed cases: E0406 linear
leak, E0205 use after move or use of a partially moved value, E0443 join,
E0456 move out of a destructor-bearing value, E0493 linear overwrite, E0478
linear discard). A rejected program the machine runs to completion carries
that path's `ok` or `panic` outcome instead, and its header says so, so a
compiler that accepts it unsoundly is compared against what the machine does.
That happens two ways: the refusal lies on a path the program does not take (a
join disagreement, or a refusal inside the arm the condition skips), which is
what generated programs produce; or the rule has no dynamic counterpart at
all — `3.9:34`'s restriction on moving a field out of a destructor-bearing
value (E0456), (@Drop) §5.3's residual side condition (E0406), and (Assign)
§5.2's `3.8:77` premise, which is keyed on the destination's *type* where the
machine's overwrite monitor reads the residue it is about to drop (E0493), are
static disciplines no monitor enforces, so a program they reject still runs.

How a *drop* becomes a printed line is the **user destructor**, and nothing
else: a Rue program has no other way to observe a drop happening, so a
declaration that declares one prints `drop fn S(self) { @dbg(self.x0); }` and
the interpreter records the same drop as a `dtor` event; a declaration with
no destructor drops silently in both. `@dbg` is the second channel, and the
two share one trace, so a `@dbg` line between two destructor lines comes out
between them. `RueCore/Print.lean`'s module docstring
is the reference for the rest. Two of the constraints are the spec's: a
`@copy` type must declare no destructor (`3.9:31`), and a declaration that
carries a linear value in a field may declare no destructor at all
(`3.9:44`). A projection prints as `x.f0`, an assignment to one as `x.f0 =
e;`, and `@drop` as `@drop(p)` at every class: all the identity elaboration
— including a place whose path has a proper prefix of declared-`linear`
struct type, where the compiler and the core both select §4.2's `Declared(d,
π_s)` plan and destructure. The one context where they disagree about
*whether* a place is used, `@dbg`'s operand — a borrow in the compiler, an
ordinary use in (Dbg) §5.8 — the printer already binds to a `let` first,
which is a value context in both (`Print.lean`'s module docstring). Five
images are *not* the identity elaboration: the four typed blocks below
`Print.lean`'s "Integer typing" heading and the invented `drop fn` body,
which the core declaration does not carry and which is the whole observation
channel. One limit, accepted at fragment scope: every line is a bare integer
or `true`/`false`, so a destructor line `n` swapped with a `@dbg` line `n`
or a value line `n` would not be told apart. Every printed program opens
with a comment naming its case, the rules it exercises, and its expected
outcome in words, so `corpus.json` doubles as a readable example set.

## Explaining a program (RUE-2246)

`lake exe ruecore-explain` turns any corpus case into a page a reader can
follow without Lean: the program's `fn` items in Rue surface syntax, the
proved-sound checker's verdict, one §5 derivation per function body as a tree
with the fused `Γ;Σ` at every node, and the §6 run as a single step table in
execution order — across frames, with the store before and after each node
and the drop events it emitted. Three administrative rows make the frames
readable: the push with its minted parameter cells, the pop with the drops
`run-all-scope-drops` ran, and the same walk taken early by a `return`. When
the checker rejects, the failing premise is stated first, in the calculus's
own words, with its §-rule, its prose paragraph, and the compiler's
diagnostic code.

```bash
lake exe ruecore-explain linear_overwrite   # one case, as text
lake exe ruecore-explain --all              # every case, as text
lake exe ruecore-explain --list             # the case names
lake exe ruecore-explain --text explain     # regenerate explain/*.txt
lake exe ruecore-explain --html /tmp/out    # a self-contained page per case
```

The text renderings are checked in under `explain/`, one file per corpus
case; `--text explain` regenerates them, and they should be refreshed in the
same change whenever the corpus, the printer, or the calculus citations
move. The HTML is generated on demand and not checked in: each page is
self-contained (inline CSS, no scripts, no external assets) and `--html`
also writes an `index.html` listing every case with its verdict and outcome.

The renderer is not a second opinion about the language. `Explain.lean`
defines two instrumented mirrors — `explain`, which walks the §5 rules and
returns a derivation, and `traceEval`, which walks the §6 machine and
returns a step table — and proves they agree with the definitions the
theorems are about:

```
theorem explain_result : (explain P R Γ e).result           = check P R Γ e
theorem traceEval_res  : (traceEval P fuel d Θ R H φ e).res = eval fuel P H φ e
```

So a rendered page cannot claim an acceptance, a rejection, or an outcome
that `check` and `eval` do not produce; a divergence would be a failed
proof, not a rendering bug. Both lemmas are in the Buck target's `trust`
list, so their axioms are checked with the safety theorems'.

## Deciding whether to believe it (RUE-2247)

Start with `SPINE.md` (generated, "The statement layer" below): the 36
statements that are the claim, each with its English reading, the §7
paragraph it realizes and the definitions it names, and the checks that tie
each to its proof. Then two generated reports for a reader who knows type
systems or proof assistants and wants to judge the mechanization without
trusting whoever wrote it:

```bash
lake exe ruecore-digest > DIGEST.md          # every statement
lake exe ruecore-digest --trust > TRUST.md   # every statement's axioms
```

`DIGEST.md` is every theorem in the `RueCore` namespace with the statement
Lean elaborated — not a transcription of it — its doc-comment, and every
definition those statements are written in terms of, in dependency order. It
opens with the fragment boundary, quoted from `INDEX.md`'s coverage lines, so
the scope is visible before the claims are. Proof bodies are deliberately
absent: a proof is checked by the kernel, and what the kernel appealed to is
the other report. A definition's body is printed when the definition *is* a
type or a predicate (`Ctx`, `CellMatches`, `InBounds`) or when it is short
enough to read: a signature alone cannot tell `Ty.mult` from `fun _ => .copy`
or `Ctx.join` from `fun _ _ => none`, and the linearity theorems are about
what those two decide. A long body (`eval`, `check`, `explain`) is left to
the module named beside its signature, and where the compiled value is the
elaborator's output rather than what was written, the entry prints the
equations Lean derived from it.

Two properties of the file are checked by the generator, which exits non-zero
and names the miss rather than printing a file whose preamble is false. Every
`RueCore` constant occurring in a signature or a printed body has an entry of
its own, or is a constructor listed under its type's entry. And every
declaration `INDEX.md` names — a file a different tool generates by reading
the sources rather than the compiled environment — is a constant that
survived the report's generated-declaration filter, with an entry of its own
when it is a theorem. Which declarations are Lean's own auxiliaries is asked
of the environment (`isAutoDeclOrPrivate_Internal`, the recursor, matcher,
instance and projection tables), never guessed from a name, so a
`theorem Ty.congr` with a `sorry` in it cannot leave the reports by being
called that.

`TRUST.md` is, for every theorem, the axioms `Lean.collectAxioms` reports for
its proof — so the `sorry` count is read from the axioms rather than from a
grep, and a `sorry` behind a macro would still show — plus the axioms the
package declares itself (none today) and the pinned toolchain.

**The float slice's assumptions live beside the axioms, not among them.**
§7 owes one lemma for floats — "totality of the float operations", discharged
"against the standard rather than against Rue" — and this package keeps that
honest by making the IEEE side an *interface* rather than an `axiom`.
`RueCore.FloatOps` (`RueCore/Float.lean`) is the operations §6.4 needs whose
result is `rnd_w` of a value that need not lie in `𝔽_w`: the four arithmetic
operators, `@sqrt`, a literal's conversion (`3.12:9`), `@int_to_float`, the
narrowing half of `@float_cast`, and `σ_NaN`. `RueCore.FloatModel` adds the
laws: closure in `𝔽_w` (`arith_wf`, `sqrt_wf`, `ofLit_wf`, `ofInt_wf`,
`narrow_wf`), and the behavioural clauses §6.4 quotes from `3.12:22`,
`3.12:19` and `3.12:9` (`arith_nan`, `narrow_nan`, `div_by_zero`,
`zero_div_zero`, `ofLit_zero`, `ofLit_one`). **Each law is one that is true of
IEEE 754 and of the compiler** — which is why the two NaN laws are the weak
ones: they say a NaN operand yields *a* NaN and leave its sign open, because
that is all the standard promises and because both of Rue's targets *propagate*
an operand's NaN rather than substituting `σ_NaN` (which `3.12:44` fixes for a
NaN an invalid operation *creates*). The propagation `Float.exactOps`
implements — the first NaN operand's sign — is a **model choice**, checked
against the compiler case by case, not a theorem. They are **structure
fields**, so a theorem that
rests on one carries it in its own statement — `#print axioms` on
`RueCore.soundness` still shows `propext`/`Quot.sound` and nothing more — and
`TRUST.md` names them in its own section rather than letting them hide inside
a proof.

Everything §6.4 does *not* round is a function of the module and a theorem
rather than an assumption: `neg` (`negate_wf`), the ordering compares,
`@total_cmp`, `@float_to_int` (whose `(D-Float-To-Int)`/
`(D-Float-To-Int-Trap)` partition is `floatToInt_partition`), the four exact
rounding intrinsics (`roundOp_wf`), and the widening half of `@float_cast`
(`widen_wf`). The executable instance `RueCore.Float.exactOps` is
constructive — `roundRat` rounds an exact rational into `𝔽_w` by integer
arithmetic — so nothing in the package touches Lean's `Float`, whose
definition over an `opaque` constant would otherwise put `Classical.choice` on
every theorem that so much as mentions a value. And `exactOps` satisfies the
laws, as a theorem rather than an assumption: `Float.exactModel`
(`RueCore/Float/Lemmas.lean`, RUE-2469) proves every one of them of it, so the
laws have a model and the theorems over `M : FloatModel` are not vacuous in
`M`: none holds for want of a model. The laws say nothing about which datum a
rounding returns; that `exactOps`'s roundings are IEEE 754's is checked
against the compiler by the corpus, not proved. `propext` and
`Quot.sound` are this project's policy; `Classical.choice` is kernel-checked
but outside it; `sorryAx` and the axiom `native_decide` adds (one per use
from Lean 4.29, `foo._native.native_decide.ax_1_1`; `Lean.ofReduceBool`
before) are holes. `decide +kernel` (or `decide (config := { kernel := true
})`) is allowed, and only where plain `decide` or `rfl` stops at the
elaborator's limits, with a comment saying so: it is kernel reduction of the
`Decidable` instance, the same trust as `rfl`, adds no axiom, and `leanchecker`
replays it; `ruecore-lint` lists each use beside the bounded options (today
two: `Float.ofLit_one` and `Nonvacuous.float`, whose roundings reach
`2^1076`). `native_decide`, `Lean.ofReduceBool` and `reduceBool` stay
forbidden. Four facts of Lean's core library reach `Classical.choice` on this
toolchain (`Nat.lt_of_mul_lt_mul_right`, `Nat.pow_lt_pow_right`,
`Nat.pow_le_pow_iff_right`, `Nat.sqrt_le`; RUE-2489): a numeric proof here
uses the constructive versions in `Float/Lemmas.lean` instead. The exe exits non-zero on anything outside the
policy, which fails the Buck build too — and unlike the target's `trust` list,
it covers *every* theorem, including ones no trusted theorem uses.

Both reports are read out of the compiled environment (`Lean.Environment`), so
neither can drift from the sources the way a hand-written summary can, and
both are committed. There is no drift gate on the committed copies: nothing in
CI runs the Lean build until ADR-0097's gate is met (RUE-2241), so a reviewer
regenerates both and diffs, which is what `GUIDE.md`'s "Validating this in
thirty minutes" asks for. `scripts/rue lean` prints `trust.md` from the Buck
build's own outputs, beside `digest.md`, `corpus.json`, `axioms.txt`, and the
`leanchecker` re-check of every module in the roots' import closure outside
the toolchain, which is what guarantees that the kernel checked every
declaration.

## How to read this, with no Lean

`GUIDE.md` is the full reader's guide: each Lean artifact in the calculus's
own terms, ten worked examples taken from Rue source through the checker and
the interpreter to the theorem that covers them, and how to run and trust
things.
`INDEX.md` (generated) maps every labeled rule and section of the calculus's
§5 and §6 to the declaration that mechanizes it, or says *not yet
mechanized*. The short version:

- **A judgment is an inductive type.** The calculus writes
  `Γ; Σ ⊢ e ⇒ T ⊣ Ω` (§5, with §5.3's outgoing result `Ω`: a normal state or
  §5.7's `⊥`, with the edge deliveries); `Statics.lean` writes
  `Typed P R Γ e T Ω`, with `Ω : Out`,
  `P` the top-level function environment (Call) §5.8 reads and `R` the
  enclosing function's declared return type (Return-Value) §5.7 checks
  against — both fixed for a derivation, as the calculus fixes them for a
  function body. Each
  constructor of `Typed` is one inference rule, its arguments are the rule's
  premises, and its doc-comment names the §5 rule and the prose paragraph it
  encodes. A program is well-typed when a value of `Typed [] e T Ω` exists.
- **The dynamics is a function.** `Dynamics.lean` defines `eval`, which runs
  an expression at a fuel bound and returns `.ok store value trace`,
  `.returned …` (a value an unwinding `return` handed past it, §6.9),
  `.panic kind trace` (a defined trap, §6.12, with the observable output that
  ran before it), `.stuck violation`, or `.outOfFuel`;
  `run P fuel` calls the program's entry point. A `Violation` is a named
  refusal.
  Four of them (`useAfterMove`, `useAfterDrop`, `unbound`, `typeConfusion`)
  are the states §6 leaves stuck, made explicit; the other four
  (`linearLeak`, `linearOverwrite`, `linearDiscard`, `ownedUnderCopy`) are
  monitors the machine adds for actions §6 would execute and §5 forbids, so
  a violation is a positive result rather than a silent drop or a
  duplicated owner. On a
  program `check` accepts, `eval` is a model of §6; on other input the two
  can differ, and `Dynamics.lean` and `Examples.lean` say exactly how. The
  trace lists every drop in order, and every aggregate value carries an
  identity minted at its introduction, so the trace says *which* value each
  drop was of (`no_double_free`, `Trace.lean`).
- **The theorem says stuck is unreachable.** `soundness` (`Soundness.lean`)
  states: if `Typed P R Γ e T Ω` holds and the frame agrees with `Γ`, then at
  every fuel `eval` never returns `.stuck`. The corollaries name one §7 bullet
  each, over a whole program (`run`) — with one carve-out, named in
  `no_violation`'s doc-comment: a by-value argument destroyed by a sibling
  argument's `return` is dropped by nobody and monitored by nobody, so the
  linear bullet has an edge these refusals do not reach (the calculus as
  written; RUE-2316). `FrameMatches` is the invariant the proof carries:
  `Matches` — "Σ faithfully tracks the store's initialization", with one
  deliberate asymmetry explained in its doc-comment — plus the σ invariant,
  that the frame's scope record read newest-first *is* its environment (which
  holds definitionally in this fragment, and becomes an obligation when
  `Frame.scope` is §6.1's stack). `Untouched` is the frame-locality property
  that carries a caller's agreement across a callee's run.
- **Run something.** Open `RueCore/Examples.lean`; each `#eval` line runs a
  program at a fuel bound, and the editor (or `lake build`'s log) shows its
  result. Change a program and watch the result change. Each
  `example : checkProgram ... = false := by rfl` is a kernel-checked
  rejection.
- **Fuel is not a loophole.** `eval` is fuel-indexed, because a recursive
  callee's body is not a subexpression of the call and a `loop` re-enters
  its body (each turn spends fuel), and the theorems quantify
  over every fuel — which `outOfFuel` would satisfy for free. `fuel_mono` and
  `no_masking` are why it is not free: raising the bound never changes an
  answer, and no bound turns a violation into exhaustion for a program some
  fuel completes.
- **Check what is trusted.** `#print axioms RueCore.soundness` must list only
  `propext` and `Quot.sound`. `scripts/rue lean` prints `TRUST.md`, which says
  the same for every theorem, and fails if anything else appears.

## Doc-comment convention (what the index reads)

`scripts/validate-lean-xref-index.py` generates `INDEX.md` from the
doc-comments in `RueCore/*.lean` and fails the premerge tier when the index
is stale or a declaration is uncited, so the index cannot rot silently. What
a slice author writes:

- Every top-level declaration in a rule-bearing module (every module except
  those marked below) has a `/-- ... -/` doc-comment that cites what it
  mechanizes, in one or more of three spellings the script recognizes:
  - a rule label exactly as the calculus writes it, in parentheses:
    `(Use-Move)`, `(D-Let)`, `(@Drop-Copy)`. The label must exist in
    `../01-core-calculus.md` §5 or §6 (the script inventories the labels at
    the ends of the rules' horizontal bars). A hyphenated label the calculus
    does not define (`(D-If)`, `(Use-Bar)`) is an error; a one-word label it
    does not define (`(Sequence)`) is ignored as prose, so one-word rule names
    have no rename protection, and a one-word rule name written in prose,
    `(Not)` or `(Panic)`, counts as a citation;
  - a calculus section: `§5.5`, `§6.7`. Write each section; a range such as
    `§5.1–§5.3` is read as its two endpoints only;
  - a prose-specification paragraph: `3.8:73`.

  Citations count only inside `/-- … -/` and `/-! … -/` comments, including
  inside code spans there; a `--` line comment is invisible to the index.
- A label is a claim: write `(Rule)` only where the declaration really is
  that rule's image, because the index's inverse table reads every label as
  "this rule is mechanized here". Where the fragment abstracts a rule away
  rather than modelling it — (Call) §5.8's by-reference clauses and §5.4's
  `Λ` have no instance without loans, which are Phase D's — name the form in
  prose with a section pointer and say what is not modelled, so the rule keeps
  reading *not yet mechanized*.
- A constructor of an inductive may carry its own doc-comment (the `Typed`
  rules do; each cites its rule). One without inherits its type's row.
- A declaration that mechanizes nothing on its own (an inversion lemma, a
  list lemma, a printing helper) says `(helper)` in its doc-comment and is
  listed under "Helpers" instead of failing the gate.
- A module whose declarations are programs and their plumbing rather than
  rules (`Examples.lean`, `Corpus.lean`) says `xref: examples` in its module
  docstring; its declarations are indexed when they cite something and never
  required to.
- The same script holds `SYNTAX_FORMS`, the table mapping each alternative of
  the calculus's §2 grammar to the `Expr`/`Ty` constructors that mechanize it,
  or to *not yet mechanized* with the reason. The gate fails on an alternative
  with no row, on a row for an alternative §2 no longer writes, and on a row
  naming a constructor the sources no longer declare — but only a human can
  say that a new constructor *is* a form's image, so a slice that gives a form
  its first core image updates its row in the same change.
- After editing doc-comments, run `scripts/validate-lean-xref-index.py
  --write` and commit the regenerated `INDEX.md`; after editing anything the
  statements or the proofs touch, regenerate `DIGEST.md` and `TRUST.md` too
  (`lake exe ruecore-digest`, `--trust`) and commit them.

## Layers (RUE-2456)

What a claim depends on is kept small and checked by a tool. Every module of
the package sits in one of five layers, and a module imports only modules of
its own layer or a lower one:

| Layer | Modules | What it holds |
| --- | --- | --- |
| **L0 syntax** | `Float`, `Syntax` | §2's syntax, types and float data |
| **L1 definitions** | `Statics`, `Dynamics`, `Step`, `Soundness/Defs`, `Checker/Defs`, `Trace/Defs`, `Adequacy/Defs` | the semantics (§5's judgment, `eval`, §6's `Step`), and every definition a headline statement is written in: value typing and `FrameMatches`, the checker algorithm, the trace projections, ledgers and configuration invariants, `Config.SafeAt` |
| **Spec statements** | `Spec`, `Spec.Safety`, `Spec.Checker`, `Spec.Trace`, `Spec.Step`, `Spec.Adequacy`, `Spec.Nonvacuous` | the headline statements, each a `def …_stmt : Prop` over L0 and L1 alone, with its English reading; the one list of them, `Spec.spine` ("The statement layer"); and the non-vacuity witnesses with their list, `Spec.witnesses` ("Non-vacuity witnesses") |
| **L2 proofs** | `Float.Lemmas`, `Statics.Lemmas`, `Dynamics.Lemmas`, `Step.Lemmas`, `Soundness`, `Checker`, `Trace`, `Adequacy`, `TraceExact`, `TraceOrder`, `Nonvacuous`, `Spine`, `Nonvacuous.Glue` | the theorems and their proofs, with the proof-internal relations (`Sim`, `Long`, the `*IH` motives); the `*.Lemmas` modules are the theorems about L0's and L1's definitions (`Float.Lemmas`: the `FloatModel` laws of `Float.exactOps`), `Nonvacuous` proves the witness statements, `Spine` checks each headline and witness proof against its Spec statement, and `Nonvacuous.Glue` applies each witness to the theorems it lists |
| **L3 tooling** | `Examples`, `Witnesses`, `Print`, `Corpus`, `Gen`, `Explain*`, `Digest`, `Layers`, `Lint`, the `*Main` executables, the root `RueCore` | example and corpus programs and the theorems about them, the printer, the generator, the explain and digest reports, the layer table and the lint |

L3 may import anything; nothing in L0–L2 or Spec imports L3, so no theorem of the
spine depends on the printer, the generator, the corpus or an example
program. Spec sits between L1 and L2 and keeps its own name, so the other
layers keep theirs: a statement may mention only syntax and definitions, and
the audit fails on a Spec module that imports a proof. L1 is definitions only: the 181 theorems its modules held are in
`Statics/Lemmas.lean`, `Dynamics/Lemmas.lean` and `Step/Lemmas.lean` (L2),
moved verbatim, and `Step.lean`'s 11 demo witnesses, with the two
`Adequacy.lean` theorems about their programs, are in `Witnesses.lean`
(RUE-2460). The `*/Defs` modules are the definitions moved verbatim out of the
proof modules (the `Defs` of a module holds what its headline statements
mention); `Witnesses.lean` is the theorems moved out of the proof modules
because they mention example or corpus programs.

**The audit.** `lake exe ruecore-layers`, after `lake build`, reads each
module's imports from its compiled `.olean` header, walking the whole import
closure of the library root and every executable's root, and checks them
against the one table in `RueCore/Layers.lean`. The walk stops at the
toolchain's own modules (`Init`, `Std`, `Lean`, `Lake`, when the `.olean` is
the one the toolchain ships), which are trusted as the toolchain is. It fails
on any other module in the closure that is not the package's (a library a
`[[lean_lib]]` line adds, say), on an import from a higher layer, on an
L0–L2 or Spec module importing anything outside the package but `Init`, on
one that is not a `module`, and on a module missing from the table, a
stale table entry, or a source file nothing imports. It prints the graph, one
line per module, and ends with `ruecore-layers: 47 modules, 118 package
imports, no upward import; import closure: 47 modules outside the toolchain,
all the package's, …`. `lake exe ruecore-layers --closure` prints that
closure, one module per line: the list the kernel re-check replays. The Buck target runs it as the `layers.txt`
report, so `./buck2 build root//:lean-ruecore` fails on an upward import;
RUE-2241 picks it up with the rest of the Lean build.

**The module system.** L0–L2 are `module`s (Lean 4.33.1 supports the
module system without an option): a `module` header, `public import`, and
`@[expose] public section`, so every definition's body stays visible to the
modules above it and `decide`, `rfl` and unfolding work across modules as
before. L3 stays ordinary files: `Examples.lean`'s `#eval`/`#guard` would
need a `meta import` of every module it runs, `Digest.lean` imports `Lean`
to walk the environment at run time, and a non-module file may import
modules, so nothing is lost. It also gives the layer rule a second guard: a
`module` cannot import a non-module file, so Lean itself refuses an L0–L2
import of an L3 file ("cannot import non-module … from module"), and the
audit covers what that leaves, an upward import within L0–L2.

Not adopted, and why: private `import` and non-exposed definitions would
hide definition bodies from the proofs and tools above them (a downstream
`decide` or `unfold` of a non-exposed definition fails), so the layer rule is
enforced by the audit instead. `leanchecker` and the digest's
`importModules` load every part of a module's `.olean` (the private part
holds proof bodies), so the kernel re-check and `#print axioms` still see
every proof. The re-check replays every module of the roots' import closure
outside the toolchain, as the audit walks it — the library, each executable's
root and the tooling it imports — and the audit makes that closure exactly the
package's modules ("The trusted-base lint").

The import graph, from the audit (an arrow points from a module to one that
imports it; the root `RueCore`, which imports every library module, is left
out):

```mermaid
flowchart BT
  subgraph L0["L0 syntax"]
    Float["Float"]
    Syntax["Syntax"]
  end
  subgraph L1["L1 definitions"]
    Adequacy_Defs["Adequacy.Defs"]
    Checker_Defs["Checker.Defs"]
    Dynamics["Dynamics"]
    Soundness_Defs["Soundness.Defs"]
    Statics["Statics"]
    Step["Step"]
    Trace_Defs["Trace.Defs"]
  end
  subgraph Spec["Spec statements"]
    Spec["Spec"]
    Spec_Adequacy["Spec.Adequacy"]
    Spec_Checker["Spec.Checker"]
    Spec_Safety["Spec.Safety"]
    Spec_Step["Spec.Step"]
    Spec_Trace["Spec.Trace"]
    Spec_Nonvacuous["Spec.Nonvacuous"]
  end
  subgraph L2["L2 proofs"]
    Adequacy["Adequacy"]
    Checker["Checker"]
    Dynamics_Lemmas["Dynamics.Lemmas"]
    Float_Lemmas["Float.Lemmas"]
    Nonvacuous["Nonvacuous"]
    Nonvacuous_Glue["Nonvacuous.Glue"]
    Soundness["Soundness"]
    Spine["Spine"]
    Statics_Lemmas["Statics.Lemmas"]
    Step_Lemmas["Step.Lemmas"]
    Trace["Trace"]
    TraceExact["TraceExact"]
    TraceOrder["TraceOrder"]
  end
  subgraph L3["L3 tooling"]
    root["RueCore (root)"]
    Corpus["Corpus"]
    CorpusMain["CorpusMain"]
    Digest["Digest"]
    DigestMain["DigestMain"]
    Examples["Examples"]
    Explain["Explain"]
    Explain_Html["Explain.Html"]
    Explain_Ledger["Explain.Ledger"]
    Explain_Text["Explain.Text"]
    ExplainMain["ExplainMain"]
    Gen["Gen"]
    Layers["Layers"]
    LayersMain["LayersMain"]
    Lint["Lint"]
    LintMain["LintMain"]
    Print["Print"]
    Witnesses["Witnesses"]
  end
  Float --> Syntax
  Step --> Adequacy_Defs
  Soundness_Defs --> Adequacy_Defs
  Statics --> Checker_Defs
  Statics --> Dynamics
  Dynamics --> Soundness_Defs
  Syntax --> Statics
  Dynamics --> Step
  Step --> Trace_Defs
  Soundness_Defs --> Trace_Defs
  Spec_Safety --> Spec
  Spec_Checker --> Spec
  Spec_Trace --> Spec
  Spec_Step --> Spec
  Spec_Adequacy --> Spec
  Spec_Nonvacuous --> Spec
  Float --> Spec_Nonvacuous
  Checker_Defs --> Spec_Nonvacuous
  Soundness_Defs --> Spec_Nonvacuous
  Trace_Defs --> Spec_Nonvacuous
  Adequacy_Defs --> Spec_Nonvacuous
  Float --> Float_Lemmas
  Float_Lemmas --> Nonvacuous
  Checker --> Nonvacuous
  Soundness --> Nonvacuous
  Trace --> Nonvacuous
  Adequacy --> Nonvacuous
  Nonvacuous --> Spine
  Spine --> Nonvacuous_Glue
  Adequacy_Defs --> Spec_Adequacy
  Checker_Defs --> Spec_Checker
  Soundness_Defs --> Spec_Safety
  Adequacy_Defs --> Spec_Step
  Trace_Defs --> Spec_Trace
  Step --> Adequacy
  Step_Lemmas --> Adequacy
  Soundness --> Adequacy
  Adequacy_Defs --> Adequacy
  Soundness --> Checker
  Checker_Defs --> Checker
  Dynamics --> Dynamics_Lemmas
  Statics_Lemmas --> Dynamics_Lemmas
  Dynamics --> Soundness
  Dynamics_Lemmas --> Soundness
  Soundness_Defs --> Soundness
  Spec --> Spine
  Soundness --> Spine
  Checker --> Spine
  Trace --> Spine
  TraceExact --> Spine
  TraceOrder --> Spine
  Adequacy --> Spine
  Statics --> Statics_Lemmas
  Step --> Step_Lemmas
  Dynamics_Lemmas --> Step_Lemmas
  Soundness --> Trace
  Checker --> Trace
  Step --> Trace
  Step_Lemmas --> Trace
  Trace_Defs --> Trace
  Trace --> TraceExact
  Adequacy --> TraceExact
  TraceExact --> TraceOrder
  Checker --> Corpus
  Examples --> Corpus
  Print --> Corpus
  Corpus --> CorpusMain
  Gen --> CorpusMain
  root --> Digest
  Lint --> DigestMain
  Checker --> Examples
  Corpus --> Explain
  Explain_Ledger --> Explain_Html
  Explain --> Explain_Ledger
  Trace --> Explain_Ledger
  Explain_Ledger --> Explain_Text
  Explain_Text --> ExplainMain
  Explain_Html --> ExplainMain
  Corpus --> Gen
  Layers --> LayersMain
  Digest --> Lint
  Layers --> Lint
  Lint --> LintMain
  Syntax --> Print
  Adequacy --> Witnesses
  TraceExact --> Witnesses
  TraceOrder --> Witnesses
  Corpus --> Witnesses
```

## The trusted-base lint (RUE-2457)

`TRUST.md` reports each theorem's axioms; the lint turns the trust bar into a
build failure and extends it to every declaration. `lake exe ruecore-lint`,
after `lake build` and the executables' builds, imports each root of the layer
table in turn (the library, then each executable, since each defines its own
`main`) and asks the compiled environment, for every constant declared in a
package module — about 10,000, generated ones included:

- **Axioms, by allow-list.** One memoized pass over every constant's type and
  value (after TauCeti's `scripts/Axioms.lean`, and walking the bodies itself
  rather than reading the axiom summaries Lean stores in each `.olean`). Any
  axiom but `propext` and `Quot.sound` fails, whatever it is called. That is
  what catches `native_decide`: since Lean 4.29 each use adds its own axiom,
  `foo._native.native_decide.ax_1_1 : decide p = true`, which mentions no
  compiler primitive and would pass a lint that matched `Lean.ofReduceBool`
  by name. One exception, listed rather than failed: an L3 *definition* that
  reaches `Classical.choice`, which Lean's own library brings (the proofs
  inside `String` operations, and a `partial def`'s `Nonempty` inhabitant).
  No statement depends on tooling code; every L3 *theorem*, and every
  declaration of L0–L2, is held to the allow-list.
- **Constructs, in L0–L2.** No `unsafe`, `partial def`, `@[implemented_by]`,
  `@[extern]`, `opaque`, or direct use of a compiler-evaluation primitive
  (`Lean.reduceBool`, `Lean.ofReduceBool`, `Lean.trustCompiler`, …). Each
  makes the code `#eval` runs differ from the definition the kernel reasons
  about, or hides a definition from the kernel, without leaving an axiom. L3
  may use them, and every use is listed. So are the four printers `deriving
  Repr` writes as `partial def`s for the nested types of L0–L1 (`Expr`,
  `Val`, `Contents`, `OwnSt`). They are recognized by where they came from,
  not by their shape: an `opaque` returning `Std.Format` under a `Repr`
  instance, where the printer and the instance were both declared inside the
  source range of the type's own declaration, which is where a `deriving`
  clause puts them. A hand-written `partial def instReprT.repr` is a command
  of its own, outside that range, and fails. The printers only render values
  for `#eval`, and none is in the trusted base. The exemption is a policy
  convenience, not a trust claim: one macro call that writes a type, a
  printer and its instance gives all three the call's range, so it passes
  too, and what keeps such a printer harmless is that it is an `opaque` of
  type `Std.Format` that no headline statement reaches. The `f._unsafe_rec` Lean compiles
  beside every recursive definition is the compiler's, and is not reported
  apart from `f`.
- **Options, a courtesy.** No `set_option debug.skipKernelTC` (it adds
  declarations the kernel never checks) and no `maxHeartbeats 0` in the
  sources or `lakefile.toml`; bounded overrides are listed. Options leave no
  trace in the environment, so this one check reads the sources, outside
  comments and string literals, as one stream of tokens: a line break after
  `set_option`, a `«quoted»` name part and `set_option … in` read as they do
  to Lean. A `set_option` whose name is not a literal identifier (a
  macro's `$o:ident`) fails, and so does any other name mentioning
  `skipKernelTC`. The scan is best-effort: it does not parse every lexical
  form (an interpolated string's `{…}`, a raw string, a TOML escape in
  `lakefile.toml`), so its summary line says what the scan found, not that
  there is no such option.

**The guarantee is the kernel re-check, not the scan.** A macro or an
elaborator can set an option without writing `set_option`, and L3 imports
`Lean`, so no scan of the sources can show that every declaration went
through the kernel. The toolchain's `leanchecker` does: it replays every
declaration of a module through the kernel, on top of that module's imports,
and a declaration the kernel rejects fails it, however the option was set.
`leanchecker` picks modules by name prefix, not by what imports what, so it
is given the roots' import closure by name: `lake env leanchecker $(lake exe
ruecore-layers --closure)`, every module the audit's walk from the `.olean`
headers reaches outside the toolchain. The toolchain's modules (`Init`,
`Std`, `Lean`, `Lake`) are not replayed; they are trusted as the toolchain
is. The audit fails on any other module in the closure, so what is replayed
is exactly the package's 47 modules. The Buck target runs it after the
executables are built; a local check should run it too (about 15 s).

It prints each table, ends with one summary line, and exits non-zero on a
violation, naming each on stderr. The Buck target runs it as the `lint.txt`
report, so `./buck2 build root//:lean-ruecore` fails on a violation. For
RUE-2241: when the Lean build enters CI, `lint.txt` is one of the reports it
gates on, beside `trust.md` and `layers.txt`, and nothing more needs wiring.

**The trusted base.** The same module computes what a reviewer must read to
know what the headline theorems say, beside the Spec layer: the package
definitions the Spec statements transitively unfold to (a statement's
constants, a definition's body, an inductive type's constructors, but no
proof). `TRUST.md` prints it in its "Trusted base" section. The headline
statements are the Spec layer's list, `RueCore.Spec.spine` ("The statement
layer"): 36 theorems, following the packet `../REDTEAM.md` asks for: the §7
claims the fragment states (`SPINE.md` opens with those it does not) and
their linking theorems (`checkProgram_sound`, `step_iff`,
`Config.stuck_iff`, `run_complete`, `run_ne_returned`). A lemma
`03-metatheory.md` cites as a step of a proof (the trace invariants behind
`no_double_free`, the drop-order lemmas, the float lemmas §7 owes) is not a
claim and is not on the list. Nor are the non-vacuity witnesses, which may
name definitions outside the trusted base (`Float.exactOps` and its
`roundRat`): a witness can only fail to witness, never widen a claim. Today
the headlines' trusted base is 292 definitions,
all in L0 and L1 (the package has 1092 theorems besides, 228 of them the
glue applications of `Nonvacuous/Glue.lean`, and the 49 `Spine`
restatements: 36 of the spine, 13 of the witnesses). A
definition counts as Lean's own, and is only counted, when Lean's own tables
record it as such (recursors and their auxiliaries, matchers, projections),
or when it is named as Lean names a by-product and has no source range of its
own. Anything else is listed: a hand-written `T.ndrec` or `RueCore._x`, a
`private` definition, one under an instance's name (`instDecidableEqTy.decEq`).

## The statement layer (RUE-2460)

A kernel-checked proof is worth what its statement says. `SPINE.md`
(generated) is the claim, statement by statement, and the first thing to
read; `DIGEST.md` stays the full index.

**Spec.** Every headline statement is written once, in the Spec layer
(`RueCore/Spec.lean` and `RueCore/Spec/*.lean`), as a `def <name>_stmt : Prop`
over the definitions of L0 and L1 alone, with a doc-comment giving its English
reading, the §7 paragraph of `../01-core-calculus.md` it realizes, and where
it is narrower than that paragraph. `RueCore.Spec.spine` lists the 36 of them,
each beside the theorem that proves it; `RueCore.Spec.witnesses` lists the 13
non-vacuity witnesses the same way ("Non-vacuity witnesses", RUE-2469), 49
statements in all. Every tool reads the two lists:

* **The kernel.** `RueCore/Spine.lean` (L2) restates each theorem as
  `theorem RueCore.Spine.<name> : RueCore.Spec.<name>_stmt := @RueCore.<name>`,
  so `lake build` fails unless every proof proves its Spec statement. The
  L2 theorems keep their own statements, names and call sites.
* **The lint.** `lake exe ruecore-lint` (`Lint.spineProblems`) checks that each
  L2 theorem's own statement is its `_stmt`'s body, the same term up to binder
  names — so the statement a reader meets in the proof module is word for word
  the one in Spec, not merely definitionally equal to it — that each
  `RueCore.Spine` theorem has exactly its `_stmt` as its type, and that no
  `_stmt` and no `Spine` theorem is outside the two lists. The lint's headline
  list and the trusted base are read from `Spec.spine` alone. It also holds the
  layers to their shapes (`Lint.layerShapeProblems`): an L1 module declares
  no authored theorem (a structure's proof field is part of its definition),
  and a Spec module declares nothing but the `_stmt`s the two lists name and
  the lists themselves, so no proof rides into the challenge and no helper
  definition enters the trusted base unstated. L0 keeps its few lemmas
  (`Syntax.lean`, `Float.lean`).
* **Lean Comparator** ([leanprover/comparator](https://github.com/leanprover/comparator)),
  configured in `comparator/`. Its challenge, `comparator/Challenge.lean`,
  imports the L0 and L1 modules the Spec layer imports and nothing else. It
  writes every Spec statement out in full, as its own
  `def RueCore.Spec.<name>_stmt : Prop` whose body is the Spec statement's
  elaborated body, pretty-printed, and then states each
  `RueCore.Spine.<name> : RueCore.Spec.<name>_stmt` with `sorry`. The solution
  is `RueCore.Spine`; `comparator/config.json` names its 49 theorems (36 of the
  spine, 13 witnesses) and allows
  the axioms `propext` and `Quot.sound`. Comparator checks that each solution
  theorem has the challenge's statement, with every constant the statements
  use identical in the two environments — each `_stmt` included, so the
  challenge's written-out copy must be the Spec layer's term exactly — that
  its proof uses no other axiom, and that the kernel accepts the whole
  exported solution, which it replays from a `lean4export` export rather than
  trusting an `.olean`.

  What Comparator adds is that independent replay and the axiom allow-list,
  sandboxed on Linux. It does not freeze the claim: the challenge is generated
  from the Spec layer, so a Spec statement weakened together with its proof
  and the challenge regenerated passes it. What makes such a change visible is
  that it cannot stay small: the regenerated challenge shows the new statement
  in its diff, and **`spine-fingerprints.txt`** (committed, one hash of each
  statement's elaborated body, `lake exe ruecore-digest --fingerprint`) no
  longer matches. `bin/chain.sh` fails on a statement added, removed or
  changed (`spine statement changed: <name> — regenerate and get review`)
  and never regenerates that file itself. The claim is the Spec layer as
  reviewed; the fingerprints and the challenge make a change to it a
  deliberate, reviewed diff.

`SPINE.md`, the challenge and the configuration are generated from
`Spec.spine`, so none of them is edited by hand:

```bash
lake exe ruecore-digest --spine > SPINE.md
lake exe ruecore-digest --challenge > comparator/Challenge.lean
lake exe ruecore-digest --comparator-config > comparator/config.json
lake exe ruecore-digest --fingerprint > spine-fingerprints.txt   # only once the Spec diff is reviewed
```

**Running Comparator.** `comparator/run.sh` builds Comparator (tag `v4.33.0`)
and `lean4export` (the revision that tag pins, `v4.33.0`) against this
package's toolchain in `.lake/comparator`, then runs Comparator on
`comparator/config.json`. The `Challenge` library in `lakefile.toml` is not a
default target and nothing imports it, so only Comparator builds it.
Comparator runs every build and export under
[`landrun`](https://github.com/Zouuup/landrun), which needs Linux Landlock.
It calls `landrun --best-effort`, which on a kernel without Landlock runs the
command **unsandboxed and says nothing**. So `comparator/run.sh` runs
Comparator only on positive evidence that the `landrun` it would use
sandboxes, called with Comparator's flags (`--best-effort --ldd --add-exec`)
and write access to one fresh directory:

1. unsandboxed, a shell can write a file in a second directory and read
   `run.sh` (the control);
2. under that `landrun`, the write to the granted directory succeeds;
3. under it, a write to the second directory leaves no file, and a read of
   `run.sh` fails.

It refuses to run when there is no `landrun`, or when any of the three does
not hold: a `landrun` that exits non-zero or is too old for those flags fails
2, and one that is a shim (Comparator's `fake-landrun.sh` put in
`COMPARATOR_LANDRUN`, or any that execs its command) or runs on a kernel
without Landlock fails 3.

* **On macOS**, `comparator/run.sh --unsandboxed` uses Comparator's
  `scripts/fake-landrun.sh`, says on stderr that it is running with no
  sandbox, and runs the same steps unsandboxed. Every
  check is made, and it passes (`Your solution is okay!`, about 12 s once the
  package is built). What the sandbox adds is protection against a solution
  written to tamper with the build, which our own proofs are not. So does
  Docker Desktop: its LinuxKit kernel (6.10) is built without
  `CONFIG_SECURITY_LANDLOCK`; there the script built Comparator, `lean4export`
  and the package from scratch on Linux (aarch64) and Comparator passed, but
  the probe shows `landrun` sandboxing nothing, and the script now refuses
  without `--unsandboxed`.
* **In RUE-2241's Linux lane**, on a kernel with Landlock active (Linux 5.13 or
  later with `landlock` in `/sys/kernel/security/lsm`, as on GitHub's Ubuntu
  runners): build `landrun` from its `main` branch (`GOBIN=$HOME/.local/bin go
  install github.com/zouuup/landrun/cmd/landrun@main`, Go 1.24) and put it in
  `PATH`; then, in this directory of a fresh checkout that has not built
  `RueCore.Spine` yet (Comparator's second assumption), run Comparator's own
  invocation, `systemd-run --property=RestrictAddressFamilies=~AF_UNIX --user
  --pty -E PATH="$PATH" --working-directory "$PWD" -- comparator/run.sh`, or,
  where no systemd user session exists, `comparator/run.sh` alone (the same
  sandbox without the `AF_UNIX` guard). The lane passes when the script exits
  0. Not yet run on a Landlock kernel: the sandboxed path, and the probe's
  passing case, are untested; the refusals (no `landrun`, one that always
  fails, a shim, the no-op one) are tested on macOS.

## Non-vacuity witnesses (RUE-2469)

A kernel-checked statement can still be empty: a checker that accepts nothing
is trivially sound, and a statement over every `M : FloatModel` holds
vacuously if no model satisfies the laws. So every spine statement has
**non-vacuity witnesses**: Spec statements (`RueCore/Spec/Nonvacuous.lean`)
saying that its hypotheses hold together of a non-trivial program, written out
in the statement, with the non-triviality in the statement too.

* **The float laws have a model.** `Nonvacuous.exact_model` is `∃ M :
  FloatModel, M.toFloatOps = Float.exactOps`: `RueCore/Float/Lemmas.lean`
  proves every law of the executable instance (closure of `rnd_w` through
  `roundRat_wf` and `@sqrt`'s `sqrt_core`, the NaN, division and literal laws
  by case analysis), constructively, with four core-library facts that reach
  `Classical.choice` reproved. It covers the 19 statements over a model.
* **Eight programs, one per construct class**: destructors, declared-linear
  values, a loop, an array, an enum with `match`, an early `return`, `@panic`
  and floats. Each statement says the checker accepts the program, it is
  `ProgramTyped` and `pendingSafe`, its body is typed by `check`, and its run
  on `Float.exactOps` returns (or panics) and is reached by `Step` from
  `Config.init`, with, say, two identities freed and two destructors run, or
  the value `7.5`. The destructor program also carries `DtorNotCopy`, a step
  from `Config.init`, never-stuck at every fuel, and a `Lead` minting an owned
  identity, the hypotheses the trace statements add.
* **A program that diverges** (`loop { () }`, `outOfFuel` at every fuel, both
  sides of `eval_diverges_iff`), **one that gets stuck** unchecked (a read after
  `@drop`: `run` refuses it with `useAfterMove` and `Step` reaches a stuck
  configuration), **the empty frame** (`FrameMatches` and `StoreCC` at the
  empty context, frame and store), and **an open term in a live frame**
  (`open_frame`: `@drop(s); 1` typed in the context `s : S0`, over a frame and
  store that hold an `S0`, whose evaluation runs that value's destructor), so
  `soundness`, `drop_exactly_once` and `rest_exactly_once` are shown to apply
  beyond the empty frame.

`RueCore.Spec.witnesses` lists each with its theorem (`RueCore/Nonvacuous.lean`,
L2) and the spine theorems it witnesses. It is read beside `Spec.spine`:
`Spine.lean` binds each proof, the lint holds each to a spine entry's checks
and fails on a spine theorem no witness names, and Comparator's challenge and
configuration, `spine-fingerprints.txt` and `SPINE.md` (each theorem's
"Non-vacuous" line, and a last section with the witnesses in full) include
them. The proofs run the checker and `eval` in the kernel: `rfl`, `decide`,
and `decide +kernel` for the float program, whose rounding reaches `2^1076`,
past the elaborator's evaluation threshold; never `native_decide`.

**Every listed pair is applied.** `RueCore/Nonvacuous/Glue.lean` (L2) has one
theorem per (witness, spine theorem) pair of `Spec.witnesses`,
`Glue.<witness>.<theorem>`, which takes the witness's facts (with `M` from
`exact_model` and the frame from `empty_frame` or `open_frame`) and applies
`RueCore.Spine.<theorem>` to them. It elaborates only if the witness supplies
that theorem's literal hypotheses, and the lint fails on a listed pair whose
glue theorem is missing or does not use both constants. Five spine statements
have no hypotheses (`freed_once`, `run_ne_returned`, `Config.trichotomy`,
`step_iff`, `Config.stuck_iff`); their witnesses only apply them at a
non-trivial program, and `SPINE.md` says so on their line.

A witness shows a hypothesis satisfiable, not needed; that a conclusion fails
once a hypothesis is dropped (sharpness) is RUE-2485's.

**The checker's acceptance profile.** `lake exe ruecore-corpus --profile
[--gen N --seed S]` counts how many corpus and generated programs
`checkProgram` accepts and rejects, and each side's outcomes under `run`. At
this commit: of the 174 seed cases it accepts 140 (116 return, 24 panic) and
rejects 34 (21 refused by a violation, 13 that run to a value, the checker's
conservatism: a leak on a path not taken, say); of 200 generated programs
(seed 7) it accepts 115 and rejects 85 (52 refused, 33 that return or panic).
No accepted program is refused, as `no_violation` says. `Witnesses.lean`'s
`errorClasses_rejected` checks in the kernel that it rejects a corpus program
of each of 21 error classes (`errorClassCases`): use after move, directly,
across a loop's back edge (two) and by a second `match` of a moved scrutinee;
a linear value leaked at a scope exit, a join (three shapes), an early
`return`, a `break`, one of a loop's exits, a parameter, a match arm or on
one path of an array element; discarded; overwritten; an assignment into an
array with a moved-out element; a use of a partially moved value; a move and
a destructure out of a destructor-bearing value; and a destructure with a
linear residue. Thirteen stand beside an accepted corpus case that differs
where the error is. The list is the classes the corpus exercises, not a
proof that the statics rule out no others. `typeErrors_rejected` adds six
type errors the corpus cannot hold (the compiler rejects them first): an
operand of the wrong type, a call of the wrong arity, an `if` on an integer,
a body of the wrong type, a field of an integer and a `match` on an integer.

## What is mechanized

| File | Contents | Calculus |
| --- | --- | --- |
| `RueCore/Float.lean` | §2's datum set `𝔽_w` with the operations §6.4 computes exactly, `3.12:40`–`3.12:42`'s shortest round-trip rendering, the `FloatOps`/`FloatModel` interface and its named IEEE laws, and the constructive instance `Float.exactOps` | §2, §6.4, §7's float lemma |
| `RueCore/Float/Lemmas.lean` | (layer L2) every `FloatModel` law proved of `Float.exactOps`, and `Float.exactModel` (RUE-2469): `roundRat_wf` (`rnd_w` lands in `𝔽_w`), `sqrt_core`/`sqrt_wf`, the NaN, division and literal laws | §2, §6.4, §7's float lemma |
| `RueCore/Syntax.lean` | multiplicity lattice and its join, §2's declaration environment `D` — struct declarations with their attribute, fields and destructor, and **enum** declarations with one payload tuple per variant — types including `[T; n]`, `class(T)` with §3's four-line array table (`3.8:74`), **places** (§5's `Path`, field steps and **constant** index steps) with the type a path reaches, §4.2's use plan `dl(Γ,p)` (`declaredPrefix`) and §5.1's residue test (`linearResidue`) over it, and §4.2's restrictions on which projections may be moved, expressions | §2, §3, §4.2 |
| `RueCore/Statics.lean` | §3's class assignment as a checked equation, for both layers, grounded by `3.0:5`'s joint acyclicity read through array nesting (`WfStructs`/`WfEnums`/`WfNames` over `Ty.declIds`, the unconditional `class_unique` and its two projections, `struct_carriesLinear_iff`/`enum_carriesLinear_iff`), the fused flow-sensitive `Γ;Σ` context with Σ **keyed by path** (`OwnSt`, `fullyOwned`, §5.6's recursive `residualLinear`, whose array clause reads the element type `n` times), the ownership-threading judgment `Typed` (parameterized by the program and the enclosing return type, and concluding at §5.3's outgoing result `Ω` with the `-Bottom` rules and the join over the arms that continue) — the ordinary place rules and the **declared-linear destructure** of §5.1 beside them — the §5.5 branch join over paths and its n-way fold at a `match` (proved commutative and, over states that are shapes of their declared types (`OwnSt.wf`), associative, so the fold is invariant under a permutation of the arms, `Ctx.joinAll_perm`, idempotent and absorbing its right arm, `Ctx.join_absorb`, and every derivation preserves that shape invariant, `Typed.wf`), §5.7's loop-head equation `LoopHead` with its re-entry lemma, (Fn) and whole-program well-formedness, skeleton preservation | §3, §4.2, §5.1–§5.3, §5.5–§5.8 |
| `RueCore/Dynamics.lean` | store/frame machine as a fuel-indexed definitional interpreter with observation traces (drops, destructors, `@dbg`); cell **contents as a tree with `⊘` at any node**, navigated by a path (§6.3's `H(ℓ)@π` and `H[ℓ@π ↦ ⊘]`, a constant index being a step like a field slot); §6.3's `split`/`destructure` for the declared-linear redex, with a residue monitor; §6.11's recursive drop (destructor, then fields in declaration order, an enum's active variant's payload, and an array's elements in ascending index order, every `⊘` skipped); frames with scope records and their unwinds, `return`'s and `break`'s; loops, each turn spending fuel; value identities minted at aggregate introduction and carried by values, cells and trace events; violations as named refusals, among them the copy-closure monitor; §6.4's operator rules, §6.5's bounds trap at a dynamic index, and every §6.12 trap the fragment reaches, each carrying the trace up to it | §6.1–§6.12 |
| `RueCore/Statics/Lemmas.lean`, `RueCore/Dynamics/Lemmas.lean`, `RueCore/Step/Lemmas.lean` | (layer L2) the theorems about the three definition modules' definitions, moved out of them verbatim (RUE-2460): the class and join lemmas `Statics.lean`'s row names, the machine's few, and `Step`'s below | as the module each is about |
| `RueCore/Step.lean` | §6's reduction relation `Step` over the §6.1 configuration, one constructor per rule, with §6.2's evaluation contexts as a stack of frames (enter and plug constructors per context production, (Panic-Lift) folded into every trap); `step`, the same relation as a function, and (in `Step/Lemmas.lean`) `step_iff`; determinism (`Step.det`), no step from a terminal configuration, the terminal/step/stuck trichotomy with every stuck state named by one of §6's own four violations, never a monitor (`step_stuck_isStuckState`); the monitor-free drops and the lemmas that a monitor only removes behaviour (`unwindLocs_plain`, `destructure_plain`). Adequacy to `eval`: soundness is `Adequacy.lean`, completeness proved in `RueCore/Adequacy.lean` (`eval_complete`, `never_stuck_iff`) | §6.1–§6.12 |
| `RueCore/Soundness/Defs.lean` | (layer L1) the definitions §7's statements are written in, moved out of `Soundness.lean`: value and contents typing (`HasTy`, `ContentsTy`), the per-frame agreement invariant `FrameMatches` with its per-cell and per-node parts, frame locality `Untouched`, and `soundness`'s promise `EvalOk` | §6.1, §7 |
| `RueCore/Soundness.lean` | lemmas about value typing, the per-frame agreement invariant `FrameMatches` and frame locality `Untouched` (all three defined in `Soundness/Defs.lean`), **the safety theorem** — with progress at a `match` resting on exhaustiveness, preservation on the folded join, and a loop's back edge and exits on the head equation (`LoopHead.enter`, `LoopHead.backEdge`, `loop_exit_ok`) — the fuel lemmas, and per-§7-bullet corollaries over a whole program | §7 |
| `RueCore/Trace/Defs.lean` | (layer L1) the definitions the trace theorems are stated over, moved out of `Trace.lean`, `TraceExact.lean` and `TraceOrder.lean`: owned identities and the trace's projections (`Contents.own`, `freedIds`, `dtorIds`), the ledgers `Cons`, `Exact`, `Lead` and `Tidy`, the carve-out `Program.pendingSafe`, the block grammar `Blocks`, and the configuration invariants `Config.Ordered`, `Config.Nested`, `NewestFirst` and `Lifo` | §6.7, §6.9–§6.11, §7 |
| `RueCore/Trace.lean` | theorems over the drop trace: owned value identities (`Contents.own`, defined in `Trace/Defs.lean`), copy closure, and the conservation law `eval_conserves`, proved by fuel induction over `eval`, from which `no_double_free` follows — no identity freed twice, no destructor run twice on one value, for every finished run of a checked program; and `dupProgram_step_double_free`, the ill-typed program §6's relation frees twice | §7 |
| `RueCore/TraceExact.lean` | the exact ledger `eval_exact`, the conservation law read as an equality over the identities an evaluation starts with, proved through `rest_step`, the ledger for the rest of every form; the frame-pop invariant `eval_tidy`, that every cell an evaluation allocates is retired by its end; `drop_exactly_once`, at every well-typed configuration of a checked program — every owned value it starts with ends exactly once, dropped, discarded or consumed on the normal or the unwind path, never both, and every cell it allocated is retired — and `rest_exactly_once`, the same for the values a form's leading operands produce — a loop's lead being its body breaking — which covers values minted inside an evaluation; with the `@panic` carve-out and the RUE-2316 one (`pendingSafe`, witnessed load-bearing by `pendingSafe_needed`), and `orphan_rejected`/`letDropDeleted_rejected`/`seqDropDeleted_rejected`/`breakLeak_rejected`, results the two statements reject at typed configurations of checked programs | §6.7, §6.9, §6.10, §7 |
| `RueCore/TraceOrder.lean` | **drop order**, `drop_order`, over §6's relation, in two halves. Within a value: every finished run's trace is in the block grammar `Blocks`, each drop marker followed by exactly §6.11's walk of what it names, so every destructor runs inside the drop that owns it, in §6.11's order, and nowhere else (`run_blocks` over `eval`, needing only `3.9:31`, carried to `Step` by `step_blocks`). Across cells: every scope record is in location order, which is registration order (`reachable_ordered`); the scopes nest, pending `endscope` markers being the tail of their record (`reachable_nested`); and every reachable step is last-in first-out on the registration stack, dropping only cells it deregistered, newest first, each newer than every cell still registered (`reachable_lifo`, `Lifo.newer`). Its witnesses on example programs (`fieldsSwapped_rejected`, `swappedMarkers_rejected`, `unorderedRecord_rejected`, `returnPastAffine_newestFirst`) and the order-witnessing corpus cases are in `Witnesses.lean` | §3.9, §6.7, §6.9, §6.10, §6.11, §7 |
| `RueCore/Adequacy.lean` | **`eval` is adequate to `Step`, both ways, and §7 over `Step`** (RUE-2289 parts 2–4, ADR-0097 decision 3). Soundness: the simulation relation `Sim` between an `eval` result and `→*` from the expression in focus under any context — a value reaches the hole's value in the same frame, a panic reaches `↯κ`, an unwinding `return` the nearest caller, an unwinding `break` the nearest loop's context — proved for every expression and fuel on every program (`eval_sim`, `run_sim`), and `eval_sound`, the statement over checked programs, where `no_violation` rules `.stuck` out. Completeness modulo fuel: exhausted fuel is a run of that many steps (`eval_steps_of_outOfFuel`); with determinism, `eval_complete` says that on a checked program every value or panic `→*` reaches is `run`'s answer at every fuel past the run's length; `never_stuck_iff` is "never `.stuck`" both ways, in §7's phrasing; `eval_diverges_iff` says exhaustion at every fuel is divergence. §7 over `Step` (part 4): `step_progress`, `step_preservation` (for the semantic configuration typing `Config.SafeAt`, whose fundamental lemma is `init_safeAt`), `step_value_typed` and `step_type_safety`; `Frame.empty`, `StepsN` and `Config.SafeAt` are defined in `Adequacy/Defs.lean` (layer L1) | §6.2, §6.9, §6.10, §6.12, §7 |
| `RueCore/Checker/Defs.lean` | (layer L1) the decidable checker as an algorithm, moved out of `Checker.lean`: `check`, `checkFn`, `checkDecls` and `checkProgram` | §3, §5 as an algorithm |
| `RueCore/Checker.lean` | decidable checker `check`/`checkProgram` (defined in `Checker/Defs.lean`) + `check_sound`/`checkProgram_sound` (every acceptance is a derivation), with §5.7's loop head found by a bounded iteration (`headIter`) and re-verified, and `checkDecls` — §3's two class equations plus `3.0:5`'s acyclicity, decided by peeling the declarations | §3, §5 as an algorithm |
| `RueCore/Spec/Nonvacuous.lean`, `RueCore/Nonvacuous.lean` | (layers Spec and L2) the non-vacuity witnesses (RUE-2469, "Non-vacuity witnesses"): thirteen statements, over written-out programs, that the spine's hypotheses hold together of non-trivial programs, and their proofs | §7's hypotheses, satisfied |
| `RueCore/Examples.lean` | `#eval` demos; kernel-checked acceptance/rejection of example programs | — |
| `RueCore/Witnesses.lean` | (layer L3) the theorems at work on example and corpus programs, moved out of the proof modules because they mention the tooling layer: `affineScopeDrop_both_ways` traces one corpus program both ways; `drop_order`'s rejections (`fieldsSwapped_rejected`, `swappedMarkers_rejected`, `unorderedRecord_rejected`) and `returnPastAffine_newestFirst`; fourteen order-witnessing corpus cases read through the trace theorems; every accepted seed case is `pendingSafe`; and, moved from `Step.lean` and `Adequacy.lean` (RUE-2460), eleven programs run through §6's relation by `stepN` and `run_sim`'s and `run_complete`'s witnesses on them (`letAddProgram_sound`, `dropMoved_refused`); and the checker's rejections, one corpus case per error class (`errorClasses_rejected`, `typeErrors_rejected`, RUE-2469) | §5, §6.7, §6.9, §6.11, §7 witnesses |
| `RueCore/Print.lean` | core syntax → Rue source, the program's struct and enum declarations included, and the observation channel (a `drop fn` per destructor-bearing declaration) | §2 elaboration inventory, 3.9 |
| `RueCore/Corpus.lean` | the bridge corpus: each case's checker verdict and interpreter outcome, exported as JSON (`lake exe ruecore-corpus`), and the checker's acceptance profile over it (`--profile`, RUE-2469) | §5, §6, §7 witnesses |
| `RueCore/Gen.lean` | a seeded, type-directed generator of fragment programs — struct **and enum** declarations, enum construction, `match` in §5.5's canonical form, **arrays**: `[T; n]` fields and binders, literal and repeat forms, constant-index reads, writes, element moves and `@drop`s, and dynamic-index reads, writes and `Copy` `@drop`s at and below the element, in and out of bounds — and **loops**: counted and once-through, nested, with `break` arms that may move a binder from outside the loop, every one terminating — appended to the corpus by `lake exe ruecore-corpus --gen N --seed S` | programs, not rules |
| `RueCore/Explain.lean` | instrumented mirrors of `check` and `eval` — derivation trees with the failing premise named, and step tables with stores and drop events — with the lemmas tying both to the proved definitions | §5, §6 as an explanation |
| `RueCore/Explain/Text.lean`, `RueCore/Explain/Html.lean`, `RueCore/Explain/Ledger.lean` | the terminal and self-contained-page renderings (`lake exe ruecore-explain`), each ending with the identity ledger: per owned identity, the step that minted it, the steps that ended it and the steps whose destructor ran on it, so "exactly once" and the order are visible; the checked-in text is in `explain/` | §7 |
| `RueCore/Digest.lean`, `RueCore/DigestMain.lean` | the statement digest and the trust report, walked out of the compiled environment (`lake exe ruecore-digest`) | the claim inventory and its trust boundary |
| `RueCore/Layers.lean`, `RueCore/LayersMain.lean` | the layer table and the layering audit over the compiled import graph (`lake exe ruecore-layers`, "Layers" above) | what the claims may depend on |
| `RueCore/Lint.lean`, `RueCore/LintMain.lean` | the headline statements, the trusted-base lint over every declaration of the package, and the trusted base `TRUST.md` prints (`lake exe ruecore-lint`, "The trusted-base lint" above) | what the claims may rest on |
| `RueCore/Spec.lean`, `RueCore/Spec/*.lean` | (layer Spec) the 36 headline statements, each a `def …_stmt : Prop` over L0 and L1 with its English reading, and `Spec.spine`, the one list of them ("The statement layer"); the thirteen non-vacuity witnesses and their list, `Spec.witnesses` ("Non-vacuity witnesses") | §7's claims, stated |
| `RueCore/Spine.lean` | (layer L2) each headline theorem restated with its Spec statement as its type, so the kernel checks the proof against it; Lean Comparator's solution | §7's claims, proved |
| `comparator/` | Lean Comparator's challenge and configuration (generated) and `run.sh`, which builds and runs Comparator | the statement/proof split, certified |
| `SPINE.md` | (generated) every Spec statement in Lean, its English reading, its §7 paragraph and the definitions it names — the first page a reviewer reads | §7's claims, stated |
| `DIGEST.md`, `TRUST.md` | (generated) every theorem's statement with the definitions it is written in terms of; every theorem's axioms, `sorry` count, and declared assumptions | §7's claims, stated |
| `GUIDE.md`, `INDEX.md` | the reader's guide, including the thirty-minute validation procedure, and the generated form ↔ rule ↔ declaration ↔ paragraph index (`scripts/validate-lean-xref-index.py`) | §2, §5, §6 coverage |

The fragment: integers at every width and signedness, `float(w)` at both
widths, `bool`, `unit`,
monomorphic struct types declared by the program, with §3's class as the join
of the field classes lifted by the declared attribute, monomorphic **enum**
types, with §3's class the payload join over every variant (`6.3:19`), and the
fixed-length array `[T; n]`, whose class is §3's lift of `class(T)` (`3.8:74`'s
zero-length array of a non-`Copy` element is `Affine`, not `Copy`); struct and
array literals ((Struct-Intro)/(Array-Intro) §5.8), enum construction and the
`match` that eliminates it in §5.5's canonical form — one arm per variant,
binding that variant's payload as locals that leave scope at the arm's end —
and §6.11's drop order (destructor, then fields in declaration order, an
enum's **active** variant's payload, and an array's elements in ascending
index order, every `⊘` skipped); **places** `p ::= x | p.f | p[c]`, so a use is
a copy or a move at a path — the partial move of `3.8:22`, and `3.8:68`'s
element move where that path's first step off the root is a constant index —
and a `@drop` and an assignment name one too; §5.6's leak check is the recursive
`residual-linear` read on the residue, and §5.5's join is taken path by path;
the **declared-linear destructure** of `3.8:33` — §4.2's `Declared(d, π_s)`
plan, §5.1's residue gate and §6.3's ordered `split`/`drop*` — so a projection
out of a declared-`linear` struct consumes the smallest enclosing one and
destroys its droppable residue at the access;
`let` scope exit, assignment with reinitialization and the `3.8:77`
linear-overwrite premise, sequence discard, `if` with the conservative branch
join, the whole §2 integer operator set (`+ - * / %`, `& | ^`, `<< >>`,
`< <= > >=`, `neg`, `not`, `bitnot`) with §6.4's traps and bit semantics, the
§2 float operator set (`+ - * /`, `neg`, `< <= > >=`, `@total_cmp`) with
§6.4's trap-free dynamics and the one-operand float intrinsics
(`@int_to_float`, `@float_to_int`, `@float_cast`, and the five of `3.12:34`),
`@intCast`, `@panic`, `@dbg`, the surface repeat form `[e; n]` at `7.1:38`'s
`Copy` element type, the dynamic-index read and `@drop` at a `Copy` element
type and the dynamic-index write at any element type §5.2's linear-overwrite
premise admits, all with §6.5's bounds trap, the **element-wise partial move** of `3.8:68` at
a constant-index path — with `rootIdxOnly` for §4.2's "element moves only at
the root", the `MovedOut` element state the move leaves, `3.8:73`'s
path-specific element drop, and `3.8:72`'s refusal to assign into an array that
has one — and top-level functions, by-value calls with frames and scope
records, `return` with its σ unwind, and `loop` with its nullary `break`,
typed at §5.7's loop-head state and unwound by §6.10's (D-Break). A dynamic index reaches below
itself: `a[i].x0`, `h.arr[i].x0`, `a[i][j]` and `a[i][0].x1` are read and
written as the compiler reads and writes them, and dropped when they are `Copy`, with `Place` still
constant-only, and a write evaluates its right-hand side before its indices
(`5.2:14`). No equality compare (it borrows its
operands, so `≈`'s float leaf has no instance here), no path into an enum's
payload (§5.6 tracks none) and none of the `match` shapes §5.5 makes
elaboration obligations (wildcard, repeated or guarded patterns, a bool or
integer scrutinee, a zero-arm `match`), no
borrows, no `inout`/`borrow` parameters, no accessor calls, no `continue`
(see the outline doc for the milestone ladder that adds them).

### Loops

`loop e` and the nullary `break` (§5.7, §6.10, RUE-2369) type the body once,
at the **loop-head state**: the entry state joined with the state the body
leaves at its back edge (`LoopHead`). The head is on both sides of its own
definition, so the rules take it as a premise and `check` finds the least one
by iterating from the entry state (`headIter`), then checks the equation.
`soundness` re-enters the loop at its head with the same body derivation
(`LoopHead.reenter`, resting on `Ctx.join_absorb`), which is the back-edge
proof. A `break` delivers the whole context where it fires; the loop drops the
bindings its body opened (`Ctx.loopLocals`, §6.10's unwind) and joins the rest
over every exit (`Ctx.outsideLoop`, `3.8:80`). A `break`-less loop is
`never`-typed and checks the `⟨diverge, Σ_h⟩` edge frame-wide
(`03-metatheory.md` records the reading). Eleven corpus cases cover the
shapes (`Examples.lean`, "Loops and `break`"), and the generator draws
counted, once-through and nested loops (`Gen.lean`, "Loops"). The guide's
example 11 traces RUE-1615's and RUE-1614's shapes through the rules.

## The main theorem

```
theorem soundness (hwf : WfProgram P) :
  ∀ fuel, Typed P R Γ e T Ω → FrameMatches P.decls Γ φ H →
    EvalOk P.decls T R Ω.norm Ω.brk φ H (eval fuel P H φ e)
```

`EvalOk` is a predicate on the result: a well-typed value with the agreement
restored at `Ω`'s normal outgoing state and the frame's neighbours untouched
(and no value at all when `Ω` is §5.7's `⊥`); or a value
an unwinding `return` handed back; or an unwinding `break` that fired at one
of `Ω`'s delivered states; or a *defined* panic; or `outOfFuel`. It is
`False` on `.stuck`, which is the whole point — no `Violation`
(`useAfterMove`, `useAfterDrop`, `linearLeak`, `linearOverwrite`,
`linearDiscard`, …) is reachable. The interpreter is total, so this is
progress and preservation in one statement; `run_safe` and the named §7
corollaries restate it over a whole program. `Adequacy.lean` carries it to
§6's `Step`: `step_progress` (no reachable configuration is stuck) and
`step_preservation` (every reachable configuration is typed at the entry
type, for a semantic configuration typing).

`FrameMatches` is the §7 preservation invariant, in two halves. `Matches` —
"Σ faithfully tracks the store's initialization" — whose `CellMatches` clause
is the **recursive** `ContentsMatches`, relating the binding's ownership tree
to the tree stored in its cell path by path and encoding the deliberate
asymmetry of the §5.5 join (a statically `MovedOut` path may dynamically still
hold live *non-linear* content, which the machine then drops path-specifically,
`3.8:60`; a live linear value is never statically lost). And the σ invariant: the frame's scope record, read
newest-first, **is** its environment, which is what makes `let`'s double
bookkeeping (RUE-1277) consistent and what keeps an unwind off a retired
cell.

Because a callee's body is not a subexpression of its call, `eval` is indexed
by fuel and the theorem quantifies over every bound. `fuel_mono` and
`no_masking` say that quantification is not vacuous: a bound that answered
gives that same answer at every larger bound, and no bound turns a violation
into exhaustion for a program some fuel completes.

The dynamics deliberately mirror `crates/rue-oracle`: an interpreter
producing a result plus a drop trace. `eval` runs under `#eval`, so every
semantic question ("what does this program drop, in what order?") is
answerable by execution — and the Lean model already seeds a
differential-testing harness against the Rust oracle: differential testing
against an executable model, Cedar-style, though only `--gen` mode is random,
and at a far smaller scale than Cedar's DRT (see the outline doc, and
`FIELD.md`, "Where the bridge sits").
