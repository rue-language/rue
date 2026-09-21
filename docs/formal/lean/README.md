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
never filtered (about half are rejected). The generator is a pure function
of its seed, and a case named `gen_<seed>_<i>` is the same in every run with
that seed and more than `i` cases. Its bias toward moves in one arm of an
`if`, linear values reaching scope exit, and reassignment after a move is
documented in the module.

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
`i64_min_times_neg1` is `min_T * -1` at `i64`, which §6.4's (D-Arith-Trap),
`3.1:6` and `8.1:3` all make an overflow trap and which the model traps on.
The compiler's constant folder wraps it instead and the program exits 0 —
only at `i64`, only for `*`, and only when both operands are literals; every
non-constant spelling of the same multiplication traps. That is a compiler
defect, RUE-2318, and the case stays seeded until it is fixed, the way
`cond_drop_affine` stayed after the ICE it found (RUE-2290) was. A red case
is what the bridge is for; the model is not softened to match the compiler.

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
the trap (`overflow`, `divZero`, `remZero`, `castOverflow` or `user`; the
compiler's runtime reports these as `error: integer overflow`,
`error: division by zero`, `error: integer cast overflow` and
`panic: <message>`, each with exit status 101, and the value never prints);
or, for a rejected program, `stuck` with the refusal the machine would
reach, which the bridge cannot observe because the compiler rejects the
program first (the compiler's diagnostics for the seed cases: E0406 linear
leak, E0205 use after move, E0443 join, E0493 linear overwrite, E0478
linear discard). A rejected program whose executed path never reaches the
refusal, because it lies on a path the program does not take (a join
disagreement, or a refusal inside the arm the condition skips), carries
that path's `ok` or `panic` outcome instead, and its header says so, so a
compiler that accepts it unsoundly is compared against what the machine
does; generated programs (below) have such cases, the seed corpus does not.

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
(`3.9:44`). The third is the fragment's own: `Expr.consume` reads a struct's
first field and destroys the value *without running its drop glue*, so it is
defined only on a declaration with no destructor — one there would be an
event §6.11 owes and the interpreter never emits, while the printed consumer
lets its parameter drop and so *would* print it. (`3.9:34` is not that
reason: it forbids moving a field out and permits borrowing one, and the
compiler accepts `fn consume_S(s: S) -> i64 { s.x0 }` on a destructor-bearing
`S`.) `@drop` prints as `@drop(x)` at every class — the identity
elaboration. Six images are *not* the identity elaboration: the four typed
blocks below `Print.lean`'s "Integer typing" heading, the generated
`consume_S<s>` helper a `consume` prints as a call to, and the invented
`drop fn` body, which the core declaration does not carry and which is the
whole observation channel. One limit, accepted at fragment scope: every line
is a bare integer or `true`/`false`, so a destructor line `n` swapped with a
`@dbg` line `n` or a value line `n` would not be told apart. Every printed
program opens with a comment naming
its case, the rules it exercises, and its expected outcome in words, so
`corpus.json` doubles as a readable example set.

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
package declares itself (none today; when the project's obligation interfaces
arrive they appear there with their doc-comments, which is where an
assumption's source belongs) and the pinned toolchain. `propext` and
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
  rather than modelling it — `Expr.consume` stands in for the projection the
  calculus eliminates a struct through, which is a place and so RUE-2231's —
  name the form in prose with a section pointer and say what is not modelled,
  so the rule keeps reading *not yet mechanized*.
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
| `RueCore/Syntax.lean` | multiplicity lattice and its join, struct declarations with their attribute, fields and destructor, types, `class(T)`, expressions | §2, §3 |
| `RueCore/Statics.lean` | §3's class assignment as a checked equation (`WfStructs`, `struct_class_unique`, `struct_carriesLinear_iff`), the fused flow-sensitive `Γ;Σ` context, the ownership-threading judgment `Typed` (parameterized by the program and the enclosing return type), the §5.5 branch join, (Fn) and whole-program well-formedness, skeleton preservation | §3, §4.2, §5.1–§5.3, §5.5–§5.8 |
| `RueCore/Dynamics.lean` | store/frame machine as a fuel-indexed definitional interpreter with observation traces (drops, destructors, `@dbg`); struct values and §6.11's recursive drop (destructor, then fields in declaration order); frames with scope records and their unwinds; violations as named refusals; §6.4's operator rules and every §6.12 trap the fragment reaches, each carrying the trace up to it | §6.1–§6.12 |
| `RueCore/Soundness.lean` | value typing, the per-frame agreement invariant `FrameMatches`, frame locality `Untouched`, **the safety theorem**, the fuel lemmas, and per-§7-bullet corollaries over a whole program | §7 |
| `RueCore/Checker.lean` | decidable checker `check`/`checkProgram` + `check_sound`/`checkProgram_sound` (every acceptance is a derivation) | §5 as an algorithm |
| `RueCore/Examples.lean` | `#eval` demos; kernel-checked acceptance/rejection of example programs | — |
| `RueCore/Print.lean` | core syntax → Rue source, the program's struct declarations included, and the observation channel (a `drop fn` per destructor-bearing declaration) | §2 elaboration inventory, 3.9 |
| `RueCore/Corpus.lean` | the bridge corpus: each case's checker verdict and interpreter outcome, exported as JSON (`lake exe ruecore-corpus`) | §5, §6, §7 witnesses |
| `RueCore/Gen.lean` | a seeded, type-directed generator of fragment programs, appended to the corpus by `lake exe ruecore-corpus --gen N --seed S` | programs, not rules |
| `RueCore/Explain.lean` | instrumented mirrors of `check` and `eval` — derivation trees with the failing premise named, and step tables with stores and drop events — with the lemmas tying both to the proved definitions | §5, §6 as an explanation |
| `RueCore/Explain/Text.lean`, `RueCore/Explain/Html.lean` | the terminal and self-contained-page renderings (`lake exe ruecore-explain`); the checked-in text is in `explain/` | — |
| `RueCore/Digest.lean`, `RueCore/DigestMain.lean` | the statement digest and the trust report, walked out of the compiled environment (`lake exe ruecore-digest`) | the claim inventory and its trust boundary |
| `DIGEST.md`, `TRUST.md` | (generated) every theorem's statement with the definitions it is written in terms of; every theorem's axioms, `sorry` count, and declared assumptions | §7's claims, stated |
| `GUIDE.md`, `INDEX.md` | the reader's guide, including the thirty-minute validation procedure, and the generated form ↔ rule ↔ declaration ↔ paragraph index (`scripts/validate-lean-xref-index.py`) | §2, §5, §6 coverage |

The fragment: integers at every width and signedness, `bool`, `unit`, and
monomorphic struct types declared by the program, with §3's class as the join
of the field classes lifted by the declared attribute; struct literals
((Struct-Intro) §5.8) and §6.11's drop order (destructor, then fields in
declaration order); use (copy/move), `@drop`,
`let` scope exit with the residual-linear leak check, assignment with
reinitialization and the `3.8:77` linear-overwrite premise, sequence discard,
`if` with the conservative branch join, the whole §2 integer operator set
(`+ - * / %`, `& | ^`, `<< >>`, `< <= > >=`, `neg`, `not`, `bitnot`) with
§6.4's traps and bit semantics, `@intCast`, `@panic`, `@dbg`, and
top-level functions, by-value calls with frames and scope records, and
`return` with its σ unwind. Whole bindings only — no projections/partial
moves (so the fragment's whole-value struct elimination stands in for one,
RUE-2231), no floats, no equality compare (it borrows its operands), no
enums, no arrays, no borrows, no `inout`/`borrow` parameters,
no accessor calls, no loops (see the outline doc for the milestone ladder
that adds them).

## The main theorem

```
theorem soundness (hwf : WfProgram P) :
  ∀ fuel, Typed P R Γ e T Γ' → FrameMatches P.structs Γ φ H →
    EvalOk P.structs T R Γ' φ H (eval fuel P H φ e)
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
encodes the deliberate asymmetry of the §5.5 join (a statically `MovedOut`
entry may dynamically still hold a live *non-linear* value, which the machine
then drops path-specifically, `3.8:73`; a live linear value is never
statically lost). And the σ invariant: the frame's scope record, read
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
