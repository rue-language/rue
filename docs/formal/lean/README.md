# RueCore — Lean 4 mechanization spike

A machine-checked mechanization of a fragment of the Rue core calculus
(`../01-core-calculus.md`), proving the fragment's slice of the §7
memory-safety theorems in Lean 4. This is the spike for making mechanized
proofs part of the formal core; the findings and project outline live in
`../../notes/lean-mechanization-spike.md`.

**Status: zero `sorry`, axioms `propext`/`Quot.sound` only**
(no `Classical.choice`, no `native_decide`; `TRUST.md` is the generated
evidence, and `DIGEST.md` is every statement it is evidence for). Adopted as
the fourth view of the language by ADR-0097
(`docs/designs/0097-mechanized-formal-core.md`), which
fixes the theorem shape, the authority rule, and the non-blocking posture the
project "Formal core mechanization" grows this seed under.

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
definitions — printed as a complete Rue module, the verified checker's
verdict, and the interpreter's outcome at one fixed fuel bound (a case that
bound does not complete is left out rather than given an outcome).
`crates/rue-oracle-diff` consumes it (RUE-2228) and runs the compiler, the
oracle, and the native binary on each source. Any pairwise disagreement is a
defect in one of the four views (RUE-305).

Running the consumer needs neither `lake` nor `elan`: the corpus it reads is
the Buck target's own `corpus.json`.

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
and about three in four do (154 of 200 at `--gen 200 --seed 7`, 771 of 1,000
at `--gen 1000 --seed 23`); such a program also draws enum construction and
`match` — one arm per variant in declaration order, each arm a block over that
variant's payload locals, which it may move, `@drop`, read or leave. A little
under half the cases contain a `match` (98 of 200 and 465 of 1,000 at those two
settings), and a `match` whose scrutinee is a **place** rather than a temporary
is the majority of them (127 of 211 sites and 552 of 909), because a drawn
`match` half the time binds its scrutinee to a `let` first where the scope
holds no enum place.

A use or `@drop` is drawn through a struct declared `linear` exactly as through
any other (RUE-2339), so the checker, not the draw, picks §4.2's declared-linear
destructure: 18 of the 200 programs and 67 of the 1,000 contain one, the
checker accepts 0 and 13 of those, and the destructure's own linear-residue
premise (E0474) is the deepest refusal of 0 and 2. RUE-2335's shape — a
`@drop` of a declared-`linear` place after a destructure under it, which the
compiler rejects and the model accepts — is not drawn around. None of those
1,200 cases has it, but the draw reaches it at about one accepted case in
100,000 programs (first: `gen_1_773`, `--gen 774 --seed 1`), and such a case is
a bridge disagreement to attribute to RUE-2335 by hand. One shape is deliberately absent and the
module says why: a `return` or `@panic` **inside an arm**, which `check` is
incomplete on exactly as it is inside an `if` arm.

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

**The seed corpus is red on four cases, and that is the bridge working.**
`i64_min_times_neg1` is `min_T * -1` at `i64`, which §6.4's (D-Arith-Trap),
`3.1:6` and `8.1:3` all make an overflow trap and which the model traps on.
The compiler's constant folder wraps it instead and the program exits 0 —
only at `i64`, only for `*`, and only when both operands are literals; every
non-constant spelling of the same multiplication traps. That is a compiler
defect, RUE-2318, and the case stays seeded until it is fixed, the way
`cond_drop_affine` stayed after the ICE it found (RUE-2290) was.
`destructure_ancestor_dropped` is the second: after `y.x0.x0` destructures the
inner declared-`linear` place, §5.3's (@Drop) discharges the declared-`linear`
**ancestor** `y` — `Σ(y) = Owned`, no still-owned linear sub-place remains
below it — and the model runs the program, while the compiler reports E0406.
Which of the two is right is a spec decision, RUE-2335.
`array_write_after_destructure_via_field` is the third: a declared-`linear`
destructure at `h.arr[0].x0` holes an array reached through a field, and a
write through that element follows. `3.8:72` refuses it and so does
`assignArrayOk`. The compiler accepts it, because its check fires only when the
root binding is an array, then runs the moved-out element's destructor twice
and leaks the written value. That is RUE-2341.
`array_dyn_write_after_destructure_via_field` is the fourth, the same shape
with the write below a dynamic index (`h.arr[i].x0 = …`): the model refuses it
(E0480, `fully-owned` at `h.arr` fails) and the compiler accepts it with the
same double drop and leak, RUE-2341 again.
`array_dyn_write_after_field_move` was a fifth until RUE-2344 was fixed: the
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
verified checker's verdict, one §5 derivation per function body as a tree
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

Two generated reports for a reader who knows type systems or proof assistants
and wants to judge the mechanization without trusting whoever wrote it:

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
every theorem that so much as mentions a value. What the package does **not**
prove is that `exactOps` satisfies the laws; that is the residual assumption,
and it is checked rather than proved, by running every float corpus case
against the compiler. `propext` and
`Quot.sound` are this project's policy; `Classical.choice` is kernel-checked
but outside it; `sorryAx` and `Lean.ofReduceBool`/`ofReduceNat`
(`native_decide`) are holes. The exe exits non-zero on anything outside the
policy, which fails the Buck build too — and unlike the target's `trust` list,
it covers *every* theorem, including ones no trusted theorem uses.

Both reports are read out of the compiled environment (`Lean.Environment`), so
neither can drift from the sources the way a hand-written summary can, and
both are committed. There is no drift gate on the committed copies: nothing in
CI runs the Lean build until ADR-0097's gate is met (RUE-2241), so a reviewer
regenerates both and diffs, which is what `GUIDE.md`'s "Validating this in
thirty minutes" asks for. `scripts/rue lean` prints `trust.md` from the Buck
build's own outputs, beside `digest.md`, `corpus.json`, `axioms.txt`, and the
`leanchecker` re-check.

## How to read this, with no Lean

`GUIDE.md` is the full reader's guide: each Lean artifact in the calculus's
own terms, one program worked from Rue source through the checker and the
interpreter to the theorem that covers it, and how to run and trust things.
`INDEX.md` (generated) maps every labeled rule and section of the calculus's
§5 and §6 to the declaration that mechanizes it, or says *not yet
mechanized*. The short version:

- **A judgment is an inductive type.** The calculus writes
  `Γ; Σ ⊢ e ⇒ T ⊣ Σ'` (§5); `Statics.lean` writes `Typed P R Γ e T Γ'`, with
  `P` the top-level function environment (Call) §5.8 reads and `R` the
  enclosing function's declared return type (Return-Value) §5.7 checks
  against — both fixed for a derivation, as the calculus fixes them for a
  function body. Each
  constructor of `Typed` is one inference rule, its arguments are the rule's
  premises, and its doc-comment names the §5 rule and the prose paragraph it
  encodes. A program is well-typed when a value of `Typed [] e T Γ'` exists.
- **The dynamics is a function.** `Dynamics.lean` defines `eval`, which runs
  an expression at a fuel bound and returns `.ok store value trace`,
  `.returned …` (a value an unwinding `return` handed past it, §6.9),
  `.panic kind trace` (a defined trap, §6.12, with the observable output that
  ran before it), `.stuck violation`, or `.outOfFuel`;
  `run P fuel` calls the program's entry point. A `Violation` is a named
  refusal.
  Four of them (`useAfterMove`, `useAfterDrop`, `unbound`, `typeConfusion`)
  are the states §6 leaves stuck, made explicit; the other three
  (`linearLeak`, `linearOverwrite`, `linearDiscard`) are monitors the
  machine adds for linear actions §6 would execute and §5 forbids, so a
  linear violation is a positive result rather than a silent drop. On a
  program `check` accepts, `eval` is a model of §6; on other input the two
  can differ, and `Dynamics.lean` and `Examples.lean` say exactly how. The
  trace lists every drop in order.
- **The theorem says stuck is unreachable.** `soundness` (`Soundness.lean`)
  states: if `Typed P R Γ e T Γ'` holds and the frame agrees with `Γ`, then at
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
  callee's body is not a subexpression of the call, and the theorems quantify
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

## What is mechanized

| File | Contents | Calculus |
| --- | --- | --- |
| `RueCore/Float.lean` | §2's datum set `𝔽_w` with the operations §6.4 computes exactly, `3.12:40`–`3.12:42`'s shortest round-trip rendering, the `FloatOps`/`FloatModel` interface and its named IEEE laws, and the constructive instance `Float.exactOps` | §2, §6.4, §7's float lemma |
| `RueCore/Syntax.lean` | multiplicity lattice and its join, §2's declaration environment `D` — struct declarations with their attribute, fields and destructor, and **enum** declarations with one payload tuple per variant — types including `[T; n]`, `class(T)` with §3's four-line array table (`3.8:74`), **places** (§5's `Path`, field steps and **constant** index steps) with the type a path reaches, §4.2's use plan `dl(Γ,p)` (`declaredPrefix`) and §5.1's residue test (`linearResidue`) over it, and §4.2's restrictions on which projections may be moved, expressions | §2, §3, §4.2 |
| `RueCore/Statics.lean` | §3's class assignment as a checked equation, for both layers, grounded by `3.0:5`'s joint acyclicity read through array nesting (`WfStructs`/`WfEnums`/`WfNames` over `Ty.declIds`, the unconditional `class_unique` and its two projections, `struct_carriesLinear_iff`/`enum_carriesLinear_iff`), the fused flow-sensitive `Γ;Σ` context with Σ **keyed by path** (`OwnSt`, `fullyOwned`, §5.6's recursive `residualLinear`, whose array clause reads the element type `n` times), the ownership-threading judgment `Typed` (parameterized by the program and the enclosing return type) — the ordinary place rules and the **declared-linear destructure** of §5.1 beside them — the §5.5 branch join over paths and its n-way fold at a `match` (proved commutative and, over states that are shapes of their declared types (`OwnSt.wf`), associative, so the fold is invariant under a permutation of the arms, `Ctx.joinAll_perm`), (Fn) and whole-program well-formedness, skeleton preservation | §3, §4.2, §5.1–§5.3, §5.5–§5.8 |
| `RueCore/Dynamics.lean` | store/frame machine as a fuel-indexed definitional interpreter with observation traces (drops, destructors, `@dbg`); cell **contents as a tree with `⊘` at any node**, navigated by a path (§6.3's `H(ℓ)@π` and `H[ℓ@π ↦ ⊘]`, a constant index being a step like a field slot); §6.3's `split`/`destructure` for the declared-linear redex, with a residue monitor; §6.11's recursive drop (destructor, then fields in declaration order, an enum's active variant's payload, and an array's elements in ascending index order, every `⊘` skipped); frames with scope records and their unwinds; violations as named refusals; §6.4's operator rules, §6.5's bounds trap at a dynamic index, and every §6.12 trap the fragment reaches, each carrying the trace up to it | §6.1–§6.12 |
| `RueCore/Soundness.lean` | value typing, the per-frame agreement invariant `FrameMatches`, frame locality `Untouched`, **the safety theorem** — with progress at a `match` resting on exhaustiveness and preservation on the folded join — the fuel lemmas, and per-§7-bullet corollaries over a whole program | §7 |
| `RueCore/Checker.lean` | decidable checker `check`/`checkProgram` + `check_sound`/`checkProgram_sound` (every acceptance is a derivation), and `checkDecls` — §3's two class equations plus `3.0:5`'s acyclicity, decided by peeling the declarations | §3, §5 as an algorithm |
| `RueCore/Examples.lean` | `#eval` demos; kernel-checked acceptance/rejection of example programs | — |
| `RueCore/Print.lean` | core syntax → Rue source, the program's struct and enum declarations included, and the observation channel (a `drop fn` per destructor-bearing declaration) | §2 elaboration inventory, 3.9 |
| `RueCore/Corpus.lean` | the bridge corpus: each case's checker verdict and interpreter outcome, exported as JSON (`lake exe ruecore-corpus`) | §5, §6, §7 witnesses |
| `RueCore/Gen.lean` | a seeded, type-directed generator of fragment programs — struct **and enum** declarations, enum construction, and `match` in §5.5's canonical form — appended to the corpus by `lake exe ruecore-corpus --gen N --seed S` | programs, not rules |
| `RueCore/Explain.lean` | instrumented mirrors of `check` and `eval` — derivation trees with the failing premise named, and step tables with stores and drop events — with the lemmas tying both to the proved definitions | §5, §6 as an explanation |
| `RueCore/Explain/Text.lean`, `RueCore/Explain/Html.lean` | the terminal and self-contained-page renderings (`lake exe ruecore-explain`); the checked-in text is in `explain/` | — |
| `RueCore/Digest.lean`, `RueCore/DigestMain.lean` | the statement digest and the trust report, walked out of the compiled environment (`lake exe ruecore-digest`) | the claim inventory and its trust boundary |
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
records, and `return` with its σ unwind. A dynamic index reaches below
itself: `a[i].x0`, `h.arr[i].x0`, `a[i][j]` and `a[i][0].x1` are read and
written as the compiler reads and writes them, and dropped when they are `Copy`, with `Place` still
constant-only, and a write evaluates its right-hand side before its indices
(`5.2:14`). No equality compare (it borrows its
operands, so `≈`'s float leaf has no instance here), no path into an enum's
payload (§5.6 tracks none) and none of the `match` shapes §5.5 makes
elaboration obligations (wildcard, repeated or guarded patterns, a bool or
integer scrutinee, a zero-arm `match`), no
borrows, no `inout`/`borrow` parameters, no accessor calls, no loops (see the
outline doc for the milestone ladder that adds them).

## The main theorem

```
theorem soundness (hwf : WfProgram P) :
  ∀ fuel, Typed P R Γ e T Γ' → FrameMatches P.decls Γ φ H →
    EvalOk P.decls T R Γ' φ H (eval fuel P H φ e)
```

`EvalOk` is a predicate on the result: a well-typed value with the outgoing
context's agreement restored and the frame's neighbours untouched; or a value
an unwinding `return` handed back; or a *defined* panic; or `outOfFuel`. It is
`False` on `.stuck`, which is the whole point — no `Violation`
(`useAfterMove`, `useAfterDrop`, `linearLeak`, `linearOverwrite`,
`linearDiscard`, …) is reachable. The interpreter is total, so this is
progress and preservation in one statement; `run_safe` and the named §7
corollaries restate it over a whole program.

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
answerable by execution — and the Lean model can seed a differential
harness against the Rust oracle (the Cedar pattern; see the outline doc).
