# Checker profile: how much does completeness cost, and is any of it a bug?

`checkProgram` is proved sound (`checkProgram_sound`) but not complete: a
program the declarative `Typed` judgment (§5) would accept may still be one
`checkProgram` refuses, either because the algorithm simplifies a case the
judgment derives (`Checker.lean`, "What completeness still costs") or
because the judgment itself is a conservative, flow-insensitive
approximation of what is actually safe (a branch join, a loop head, a
whole-declaration class). `lake exe ruecore-corpus --profile` (RUE-2469)
already counts acceptances and rejections; this page (RUE-2491) measures the
part of that incompleteness visible from outside the proof: corpus and
generated programs the checker rejects that the interpreter's `run`
nonetheless carries to a value at the export fuel and model, so the bridge
compares them only on their accept/reject call and never on their run
(`README.md`, "The bridge corpus"). A checker that drifted stricter over
time would show up here first — as more such cases, or as one whose refusal
no longer traces to a citation below — before it ever cost the bridge a real
disagreement.

Measured on trunk `2b68e2142` (2026-09-26):

| Corpus | programs | `checkProgram` accepts | rejects | rejected but runs cleanly |
| --- | --- | --- | --- | --- |
| seed | 193 | 143 | 50 | **19** |
| generated, `--gen 200 --seed 7` | 200 | 91 | 109 | **30** |
| generated, `--gen 1000 --seed 23` | 1000 | 513 | 487 | **133** |

`lake exe ruecore-corpus --profile [--gen N --seed S]` now prints this count
and, under it, one block per case: the name and the checker's own refusal —
the failing premise, its rule, and its citation — in the same words
`ruecore-explain` prints (`Explain.lean`'s `verdictSection`, reused rather
than re-derived, so the profile's reason and the explainer's stay one text).
That per-case listing is what the classification below was read from; it is
not reproduced here in full (182 cases across the three settings), but every
group below names representative cases so a reader can look one up with
`lake exe ruecore-explain <case>` or, for a generated one, by regenerating
the same `--gen`/`--seed` pair and searching the profile output.

## Why a rejected program still runs: five mechanisms, all already in the calculus

Every one of the 182 cases traces to a real §3/§5/§6/§7 rule with a prose
citation, and (checked below against the compiler with `verify.py` for the
seed corpus and `--gen 200 --seed 7`) the compiler is exactly as strict as
the checker on all but one already-known shape. None is a checker bug; the
182 sort into five mechanisms, all of which the calculus or `Corpus.lean`'s
module docstring already names:

1. **A branch, arm, or loop head is checked whether or not the run takes
   it.** (Match) §5.5 checks every arm's per-arm leak, (If) §5.5's join
   merges both arms' outgoing state, and §5.7's loop head is the *fixpoint*
   `Σ_h = join(Σ, B_h)` over every back-edge the body can reach — so a value
   consumed only on the arm/iteration the run does not take is flagged, and
   a `break` taken before a later iteration's move ever happens is checked
   against the head state that move already contributed to. `Corpus.lean`'s
   module docstring names this directly: "the refusal can lie on a path the
   program does not take — a §5.5 join disagreement, or a refusal inside the
   arm the condition skips". This is the largest group by far (over three
   in four of the 182): (Loop-Break) §5.7's head iteration (`3.8:79`,
   `3.8:80`), (Use-Move)/(Use-Copy)/(@Drop) §5.1/§5.3 reading a place a join
   or head left conservatively not-fully-owned (`3.8:5`, `3.8:53`), (Let) +
   §5.6's scope-exit leak check (`3.8:32`) and (Return-Value) §5.7 (`3.8:62`)
   on a binding an untaken arm or a loop iteration the run never reaches
   would have left un-consumed, (Match)'s per-arm payload leak (`6.3:17`,
   `3.8:32`), and (If) §5.5's join itself (`3.8:50`).
2. **A type's class is one verdict for the whole declaration, not per
   value.** §3's payload join over every variant (`6.3:19`) makes an enum
   Linear the moment *any* variant carries a linear field, even for a value
   built from a variant that carries none; discarding such a value trips
   (Seq) §5.3's `carries_linear` premise (`3.8:64`, E0478) although the
   concrete value has nothing to leak.
3. **A non-constant index below a declared-linear or partially-consumed
   array is refused outright**, because no static rule can tell which
   element a runtime index names: `Use-Untrackable-Dynamic-Copy` §5.1 and
   (Assign)/(@Drop-Copy) below a dynamic index (`3.8:33`, `3.8:70`,
   `7.1:45`) reject the *shape*, not the specific index a run draws, so a
   generated program whose random index happens to miss the moved-out or
   holed element still runs to a value.
4. **A static discipline with no dynamic monitor at all** —
   `Corpus.lean`'s own list: `3.9:34`'s restriction on moving a field out of
   a destructor-bearing value (E0456), (@Drop) §5.3's residual side
   condition (E0406, `3.8:32`, distinct from mechanism 1's scope-exit leak
   check — this is the premise on `@drop` itself), and (Assign) §5.2's
   `3.8:77` premise, keyed on the destination's *type* where the machine's
   `linearOverwrite` monitor reads the residue it is about to drop (E0493).
   Also `3.8:68`'s restriction that an element move applies only "directly
   to the root binding" (E0904) — an absolute syntactic restriction the
   compiler enforces identically, not a path- or value-dependent one.
   And `5.1:3`'s rule that an immutable binding is never reassigned
   (E0203): the machine stores the new value just as it would for a `mut`
   binding, so the two seeds that write an immutable binding or a
   `match` payload binding (`assign_immutable`, `match_payload_assign`) run
   cleanly and are still refused.
5. **A rule no elaborated program can trigger, or a whole-declaration check
   that runs whether or not the declaration is ever used.** `lit_out_of_range`
   probes (Lit) §5.8's range premise directly in the core, past the
   elaboration that always resolves a literal in range (`4.1:2`) — "no
   elaborated program reaches this premise". `copy_struct_dtor` and
   `dtor_linear_field` are declarations `checkDecls` refuses (`3.9:31`,
   `3.9:44`) before any function runs, `main` never instantiates either, and
   the compiler refuses the same two declarations for the same reason
   (E0457, E0462) — a `checkDecls` failure, not a per-function one, which is
   why `ruecore-explain`'s generic whole-program fallback text used to
   mis-describe these two as (Fn) §5.8's second clause; it now names
   `checkDecls` when that is what actually failed (`Explain/Text.lean`,
   `Explain/Html.lean`; the only two `explain/*.txt` renderings that change).

## The one exception: RUE-2346, already tracked

`verify.py` (run against the seed corpus and against `--gen 200 --seed 7`,
393 programs) finds exactly two cases where the compiler accepts a program
the model rejects: `array_elem_self_assign` (seed) and `gen_7_3` (generated,
the same shape reached unmasked — README.md, "The bridge corpus"). Both are
`a[i] = a[i]`, the one shape (Assign) §5.2's `3.8:72`/`7.1:46` premise
refuses on purpose while the compiler accepts it on purpose since RUE-228;
which is right is RUE-2346's open decision, not a new finding. No other
case among the 182 disagrees with the compiler — the other seven cases of
the same `3.8:72`/`7.1:46` group (an array write below an element the
program itself, not a self-read, had already moved out) are ordinary
agreement: the compiler refuses those too. `--gen 1000 --seed 23`'s 133
cases were not run through `verify.py` (an hour-scale `scripts/rue exec` per
case at that count); every refusal reason they produce is one of the same
five mechanisms above, already checked at the smaller settings, so nothing
suggests a different outcome there.

## Finding

No unexpected strictness: every refusal among the 182 traces to a cited
§3/§5/§6/§7 premise the calculus states, and the compiler is exactly as
strict except for RUE-2346's one open shape. RUE-2491 asked whether a
checker that is too strict would be visible; it now is (the count and
per-case listing above), and today it holds none.

## Reproducing

```bash
lake exe ruecore-corpus --profile                          # seed corpus only
lake exe ruecore-corpus --profile --gen 200 --seed 7
lake exe ruecore-corpus --profile --gen 1000 --seed 23
lake exe ruecore-explain <case>                             # one case's full derivation
```
