# Field map: the literature the formal core belongs to

This file grounds the formal core's terminology in the programming-languages
literature. The calculus ([01-core-calculus.md](01-core-calculus.md)), the
metatheory ([03-metatheory.md](03-metatheory.md)) and the mechanization
([lean/](lean/README.md)) draw on eight subfields, §§1–8, on some
general mathematics, §9, and on the vocabulary of program logics, data
abstraction and language implementation, §10. For each one, this file records:

- the canonical sources;
- the accepted terms, with the source that defines or uses each;
- the conventional symbols and how authors vary them;
- the standard theorem forms;
- the terms we currently use that differ from the accepted ones.

The glossary (RUE-2461) builds on this map, and so does the terminology audit.
This file is a reference, not a style guide. It says what the field calls
things; whether to rename anything of ours is those issues' decision.

**How it was built.** The term tables were drafted from the sources alone,
before any of our documents were read, so our vocabulary could not leak into
the "accepted" columns. Every source was fetched: a DOI record, an arXiv page,
an author or publisher page, or the document itself. A source that could not
be fetched is not cited. Where the only thing fetched was the bibliographic
record (the publisher blocked the full text), the Sources table marks the
entry **(record)**, and no term in this file is attributed to that source's
content. Section and theorem numbers are given only where they were seen.
Some numbering comes from a preprint (Wright & Felleisen, Felleisen & Hieb,
the Chen et al. survey), and those cases are flagged.

**Confidence**, in the "differs" tables:

- **clear**: our term names the same thing as the accepted term.
- **partial**: same idea, but a different scope, a different object, or a
  second meaning.
- **none**: the field has no counterpart for our term, or none we could verify.

---

## 1. Structural operational semantics and evaluation contexts

**Sources:** Plotkin 1981/2004 and Plotkin 2004b; Felleisen & Hieb 1992;
Pierce 2002 (TAPL); Pierce et al., *Software Foundations* vol. 2 (standing in
for TAPL's notation); Harper 2016 (PFPL).

### Accepted terms

| Term | Meaning | Source |
|---|---|---|
| transition system; configuration | A set of configurations with a step relation | Plotkin 1981/2004 §1.2 |
| terminal transition system | A transition system with a set T of final configurations, none of which can step | Plotkin 1981/2004 §1.2, Def. 2 (the author's 2004 edition) |
| small-step / big-step | Steps one atomic transition at a time / relates a term directly to its final result. Kahn and his coworkers called big-step "natural semantics" | Plotkin 2004b; SF *Smallstep* |
| structural dynamics | Harper's name for SOS-style transition rules | PFPL §5.2 |
| search rules / instruction rules | Rules that locate the next step / rules that perform it | PFPL §5.2 |
| contextual dynamics | An instruction step plus an evaluation context. It defines the same relation as the structural dynamics (PFPL Thm 5.4) | PFPL §5.3 |
| evaluation context | A context with one hole, placed where the next redex is | Felleisen & Hieb §2; PFPL §5.3 |
| hole | The empty position in a context, `[ ]` | Felleisen & Hieb; PFPL (written `∘`) |
| redex; notion of reduction | The reducible term in the hole; the base reduction relation | Felleisen & Hieb |
| standard reduction | A reduction step taken only inside an evaluation context | Felleisen & Hieb, Def. 2.3 (preprint numbering) |
| reduction semantics (usage) | A semantics given as a rewriting relation on terms, the style of Wright & Felleisen's syntactic approach | Amin & Rompf §2 |
| value; `e val` | A finished computation | PFPL §5.2; SF |
| normal form | A term that cannot step | SF *Smallstep* |
| stuck | Not a value (not final) and no step applies | Plotkin 1981/2004 §3.1, Def. 11; PFPL ch. 6 introduction; Wright & Felleisen Def. 4.8; SF |
| determinacy / deterministic | Every state has at most one successor | PFPL Lemma 5.3; SF |
| checked error, `e err` | A defined run-time failure, such as division by zero, as opposed to being stuck | PFPL §6.3 |
| statics / dynamics | Harper's words for the type system and the operational semantics | PFPL chs. 4–5 |
| evaluation dynamics | Harper's name for big-step evaluation `e ⇓ v` | PFPL ch. 7 (its title; the abbreviated edition omits the chapter's text, and `e ⇓ v` is from its cross-reference in ch. 28) |
| control stack; evaluation state `k ▷ e`, return state `k ◁ e` | A machine that makes the evaluation context explicit as a stack of frames | PFPL ch. 28, §28.1 |

### Symbols

| Symbol | Reading | Variants |
|---|---|---|
| `e ↦ e′` | "e steps to e′" | SF `t --> t'`; Plotkin `γ → γ′`. Felleisen & Hieb and Wright & Felleisen use `↦` for a step in an evaluation context. Their `→` differs: the compatible closure of the notion of reduction (Felleisen & Hieb), the notion of reduction itself (Wright & Felleisen) |
| `↦*`, `→*` | Reflexive-transitive closure: many steps | PFPL also writes `↦ᵏ` for exactly k steps |
| `E[e]` | E with its hole filled by e | PFPL `E{e}`. Felleisen & Hieb and Wright & Felleisen write the hole `[ ]` |
| `C[e]` | An arbitrary program context, not only an evaluation context | Felleisen & Hieb; Wright & Felleisen |
| `e ⇓ v` | "e evaluates to v" (big-step) | SF `t ==> n` |
| `e val`, `e err` | The value and checked-error judgments | PFPL |

**Follow:** `↦`/`→` for one step and `↦*`/`→*` for many; `E[e]` with square
brackets; "value" and "checked error" as PFPL defines them. For "stuck", use
PFPL's ch. 6 definition (not a value, and no step applies), which is also
Plotkin's Def. 11. PFPL §5.1 uses the word more widely: there every final
state is stuck too.

### Standard theorem forms

| Name | Form | Source |
|---|---|---|
| Finality of values | `¬(e val ∧ e ↦ e′)` | PFPL Lemma 5.2 |
| Determinacy | `e ↦ e′ ∧ e ↦ e″ ⇒ e′ =α e″` | PFPL Lemma 5.3 |
| Structural = contextual | `e ↦str e′ ⇔ e ↦ctx e′` | PFPL Thm 5.4 |
| Big-step = small-step | `e ⇓ v ⇔ e ↦* v ∧ v val` | PFPL §7.2 "Relating Structural and Evaluation Dynamics" (title seen; the abbreviated edition omits the text, so the statement's exact form is unchecked) |
| Church–Rosser; standardization | Reduction is confluent; `e →* e′` iff a standard reduction sequence exists | Felleisen & Hieb Thms 2.2, 2.5 (credited to Plotkin; preprint numbering) |

"Unique decomposition" (every non-value is `E[r]` for exactly one `E` and
redex `r`) is folklore. No fetched source names it as a theorem.

### Terms we currently use that differ from this

| Our term | Accepted term | Confidence |
|---|---|---|
| "reduction relation `C → C'`", `Step`, `Steps` (§6, `Step.lean`) | small-step transition relation and its closure | clear |
| "evaluation contexts `E`", `[·]`, "redex" (§6.2) | evaluation context, hole, redex | clear |
| "search" in §6.2's title | search rules (PFPL) | clear |
| `Statics.lean` / `Dynamics.lean` | statics / dynamics (PFPL) | clear |
| trap `↯κ` (§6.1, §6.12), "defined panic" (§7) | checked error `e err` (PFPL §6.3) | partial: the same role, but a different name and symbol |
| `EvalRes.refused w`, "refused" (formerly "stuck", which also covered the interpreter's four monitors; GUIDE §2) | stuck = no rule applies. On statically invalid input, a monitored case is a step §6 does take | clear since RUE-2466: "stuck" now means only §6's stuckness, and `eval` refuses, four of its refusals being monitor refusals |
| "moved-out" for the `⊘` cell content (`Contents.movedOut`; formerly "hole") | the accepted words for a moved-out cell are in §5 below; "hole" is the empty position of a context | clear: since RUE-2466 "hole" means only a context's empty position (or, in proof talk, a proof gap) |
| control stack `K`, `Kont`, `Focus` (§6.1, `Step.lean`) | control stack (PFPL ch. 28). `Focus.eval` / `Focus.ret` play the roles of PFPL's evaluation state `k ▷ e` and return state `k ◁ e` | clear: the same machine shape; the frames are language-specific |

---

## 2. Type safety, syntactic and semantic

**Sources:** Milner 1978; Wright & Felleisen 1994 (numbering below is from the
TR91-160 preprint); TAPL §8.3 "Safety = Progress + Preservation" (seen in the
table of contents); PFPL ch. 6; Timany, Krebbers, Dreyer & Birkedal 2024;
Dreyer et al. 2019 (SIGPLAN blog); Appel & McAllester 2001.

### Accepted terms

| Term | Meaning | Source |
|---|---|---|
| type safety / type soundness | Well-typed programs have well-defined behavior. PFPL calls it coherence between the statics and the dynamics | PFPL ch. 6; Timany §1 |
| "going wrong", `wrong` | Milner's denotation for a program that goes wrong | Milner 1978 |
| subject reduction | Reduction preserves typing. The older name, from combinatory logic (Curry and Feys) | Wright & Felleisen §3.1, Main Lemma 4.3 |
| preservation | The PFPL/TAPL/Timany name for subject reduction | PFPL Thm 6.2; TAPL §8.3 |
| progress | A well-typed closed term is a value, or it can step | PFPL Thm 6.4 |
| canonical forms | A value of type τ has τ's introduction form | PFPL Lemma 6.3 |
| weak / strong soundness | The program never yields `wrong` / its answer lies in the type's value set | Wright & Felleisen §§1–2 |
| faulty expression; uniform evaluation | Wright & Felleisen's syntactic approximation of stuck, later replaced by progress | Wright & Felleisen Def. 4.9, Lemma 4.10; PFPL §6.4 |
| progressive state; `safe(e)` | Every thread is a value or can reduce; every state reachable from e is progressive | Timany §2.4 |
| syntactic type soundness | Soundness established through "a syntactic connection between answers and types" (Wright & Felleisen), not a model of types. Wright & Felleisen prove it by subject reduction plus uniform evaluation (faulty expressions are untypable); the now-standard proof uses progress and preservation instead | Wright & Felleisen Thm 4.12; Timany §2.5; PFPL §6.4 |
| semantic type soundness | Soundness through a semantic typing judgment `⊨`, which says what a term does rather than how it is built | Timany §4; Dreyer et al. 2019 |
| logical relation | A type-indexed interpretation of values and expressions that defines `⊨` | Timany §5 |
| compatibility lemma | The semantic counterpart of one typing rule | Timany §4.1, §8.4 |
| fundamental theorem | Syntactic typing implies semantic typing | Timany Thm 6.5 |
| adequacy (of the semantic model) | Semantic typing implies `safe` | Timany §4.1, §6.10 (Thm 6.6); RustBelt Thm 7.2 (for a closed function of type `fn() → ()`) |
| step-indexing | Indexing the logical relation by a step count, which makes recursive types well founded | Timany §4.2. The idea is Appel & McAllester's, who call their construction an "indexed model" (`v :k τ`) |
| store typing | Typing extended to the heap, so that preservation can be stated | TAPL §13.4 (section title seen) |

### Symbols

| Symbol | Reading | Variants |
|---|---|---|
| `Γ ⊢ e : τ` | "under Γ, e has type τ" | Wright & Felleisen `Γ ▷ e : τ`; SF `Gamma \|-- t ∈ T`; PFPL writes `e : τ` for closed terms |
| `Γ ⊨ e : τ` | "e is semantically well typed at τ" | Timany |
| `safe(e)` | "no state reachable from e is stuck" | Timany |

**Follow:** "preservation", noting "(subject reduction)" once. Qualify
"syntactic soundness" and "semantic soundness" every time, because Milner uses
both words in other senses (algorithm W's agreement with the type system; a
denotational model).

### Standard theorem forms

| Name | Form | Source |
|---|---|---|
| Preservation | `Γ ⊢ e : τ ∧ e ↦ e′ ⇒ Γ ⊢ e′ : τ` | PFPL Thm 6.2; Wright & Felleisen Lemma 4.3 (stated for the notion of reduction `→`; Cor. 4.7 lifts it to `↦`) |
| Progress | `⊢ e : τ ⇒ e val ∨ ∃e′. e ↦ e′` | PFPL Thm 6.4 |
| Progress with checked errors | `⊢ e : τ ⇒ e err ∨ e val ∨ ∃e′. e ↦ e′` | PFPL Thm 6.5 |
| Type safety | Preservation ∧ progress | PFPL Thm 6.1; TAPL §8.3 |
| Syntactic soundness (Wright & Felleisen) | `⊢ e : τ ⇒ e⇑ ∨ (e ↦* v ∧ ⊢ v : τ)` | Wright & Felleisen Thm 4.12 (preprint numbering) |
| Type soundness, safety form | `∅ ⊢ e : τ ⇒ safe(e)` | Timany §2.4 |
| Fundamental theorem | `Γ ⊢ e : τ ⇒ Γ ⊨ e : τ` | Timany Thm 6.5 |
| Adequacy | `∅ ⊨ e : τ ⇒ safe(e)` | Timany §4.1; Thm 6.6 |
| Syntactic type soundness | `∅ ⊢ e : τ ⇒ safe(e)`, proved by progress and preservation | Timany Cor. 2.3 |
| Semantic type soundness | The same statement, `∅ ⊢ e : τ ⇒ safe(e)`, obtained instead from the fundamental theorem and adequacy | Timany Cor. 6.7 |

Timany et al. argue (§3) that progress and preservation are too weak rather
than false. They cover only syntactically well-typed code, say nothing about
data abstraction, and put `unsafe` code behind a safe API out of scope.

### Terms we currently use that differ from this

| Our term | Accepted term | Confidence |
|---|---|---|
| "type safety (progress + preservation)" (§7, metatheory) | type safety = progress ∧ preservation | clear |
| `step_progress`: every configuration reachable from a checked program's start steps or has halted | progress is stated per well-typed term. The reachability form is Timany's `safe(e)` | partial |
| `step_safeAt` over `Config.SafeAt` | Preservation is syntactic (`⊢ e′ : τ`). A statement over "nothing reachable is stuck, and every halted value is typed" is a semantic statement in Timany's safety form. Our own docs already say this: the `Config.SafeAt` docstring ("SafeAt invariance, not syntactic preservation", `Equivalence.lean`), GUIDE §2 and the metatheory's type-safety section call the configuration typing semantic and claim no syntactic `⊢ C : T` | partial: the old name, `step_preservation`, clashed with the accepted syntactic meaning; `step_safeAt` (RUE-2466) names the semantic invariant instead, our docs call it "SafeAt invariance", and they disclose the difference |
| `Config.SafeAt` | `safe(e)` (Timany §2.4) plus value typing. Timany's adequacy (Thm 6.6) concludes `safe(e)` only; our second conjunct is the syntactic value typing `HasTy` | partial |
| "syntactic type soundness" `init_safeAt` (metatheory; formerly "the fundamental lemma"): a checked program's initial configuration is `SafeAt` | Timany's fundamental theorem is `⊢ ⇒ ⊨`; `⊢ ⇒ safe` is their Cor. 6.7, which they call semantic type soundness, and also their Cor. 2.3, "Syntactic Type Soundness", proved by progress and preservation. Our proof has no `⊨` and no logical relation, so Cor. 2.3 is the closer counterpart: it is syntactic soundness of `eval` (a `StoreTyping` invariant) followed by the `eval`/`Step` agreement of §3. "The fundamental theorem composed with adequacy" is only an analogy | partial |
| `ActivationTyping` / `StoreTyping` ("Σ faithfully tracks the store's initialization") | the invariant a syntactic proof carries: store typing / a well-typed machine state. Ours checks each cell against both its ownership state and its type (`ContentsOwnTyping`: "`owned` holds a value"; `Soundness.lean`: "a fully-owned node holds a moved-out-free contents"), and `Soundness/Defs.lean` calls `StoreTyping` "the §7 preservation invariant" | partial: store typing extended with ownership state |
| Σ for the ownership state (§5) | Σ is store typing in TAPL and the global environment in Oxide | partial: symbol clash |
| `Refusal` (`useAfterMove`, `useAfterDrop`, …) | `wrong` (Milner), "going wrong" (CompCert), stuck (PFPL) | partial: ours is a named refusal, and four of its eight constructors are the monitors of §6 below, which are not stuck states of §6's `Step` |
| `soundness`, `run_safe` over `eval` | type soundness via a definitional interpreter (§3 below) | clear |

---

## 3. Big-step, small-step and definitional interpreters; fuel and clocks

**Sources:** Plotkin 1977; Reynolds 1972 (HOSC reprint 1998); Leroy & Grall
2009; Owens, Myreen, Kumar & Tan 2016; Amin & Rompf 2017; Siek 2013;
Nipkow & Klein, *Concrete Semantics*, with its Isabelle theory `Small_Step`;
*Software Foundations* (`Smallstep`, `ImpCEvalFun`); Niu, Sterling & Harper
2024 (as a secondary source for Plotkin 1977); Charguéraud 2013; Wadler 1992
("Monads for functional programming"). Plotkin 1977 and Reynolds 1972 are
cited for existence only.

### Accepted terms

| Term | Meaning | Source |
|---|---|---|
| natural semantics | The name Kahn and his coworkers gave big-step operational semantics | Plotkin 2004b |
| big-step / small-step semantics | An evaluation relation to a final result / a one-step relation and its closure | Leroy & Grall §§3–4; Concrete Semantics §§7.2–7.3 |
| definitional interpreter | An interpreter that serves as the definition of a language | Amin & Rompf §2.1 ("in the style of Reynolds") |
| environment | What an environment-passing interpreter looks variables up in; a closure pairs a term with one | Amin & Rompf §§2.1–2.2 |
| store; store location | The run-time map the evaluator is threaded with; reading or writing a location accesses the store | Amin & Rompf §4.1 (mutable references) |
| bind (monadic); the sequencing operation `m ⋆ λa. n` | Perform computation `m`, bind its result to `a`, then perform computation `n`; satisfies the left-unit, right-unit and associative monad laws | Wadler 1992 §2.5 (introduces the operation on the exception/state/output evaluators), §3 (the three laws) |
| functional big-step semantics | Big-step semantics written as a total recursive function with a clock | Owens et al. |
| clock | A counter in the interpreter state, decremented on recursive calls whose termination is not obvious | Owens et al. |
| fuel; step index | A bound n on how much work the interpreter may do | Amin & Rompf |
| gas | *Software Foundations*' name for the step bound | SF `ImpCEvalFun` |
| timeout | The result when the clock or fuel runs out, kept distinct from an error | Owens (`Rtimeout`); Amin & Rompf; Siek (`TimeOut`) |
| stuck / error / "goes wrong" | Evaluation reaches an undefined state. Soundness rules it out | Amin & Rompf; Owens (`Rfail`); Leroy & Grall |
| divergence (clock-based) | The program times out at every clock value | Owens et al.; Amin & Rompf |
| coinductive big-step; divergence `⇒∞` | Divergence as a coinductive big-step relation | Leroy & Grall |
| semantic equivalence (theorem) | The interpreter agrees with the small-step relation | Amin & Rompf Thm 2 |
| computational adequacy | A denotational semantics agrees with an operational one on ground-type programs | Niu, Sterling & Harper §1 (who credit Plotkin 1977 with the definition) |
| adequacy; "adequate with respect to" | A formal semantics faithfully models an intended one. Amin & Rompf use it for a reduction semantics against the language it models; Charguéraud for one operational semantics against another | Amin & Rompf §§1–2; Charguéraud 2013 §2.4 |

### Symbols

| Symbol | Reading | Variants |
|---|---|---|
| `e ⇓ v`, `a ⇒ v` | "evaluates to" | `(c,s) ⇒ t` (Concrete Semantics); `t ==> n` (SF); `(t,s) ⇓ r` (Owens) |
| `a ⇒∞` | "a diverges" (big-step) | `(t,s) ⇑t` (Owens); `e⇑` (Wright & Felleisen) |
| `eval n ρ e` | an interpreter with fuel n | `sem_t` with `s.clock` (Owens); `ceval_step st c i` (SF) |
| `Timeout ∣ Done (Error ∣ Val v)` | the interpreter's result type | `Rval/Rfail/Rtimeout` (Owens); `Result/Stuck/TimeOut` (Siek); `option` (SF) |

**Follow:** "fuel" is accepted (Amin & Rompf), and so is "clock" (the CakeML
line). The out-of-fuel result is a *timeout*. The sources that have both a
bound and an error outcome (Owens, Amin & Rompf, Siek) keep the two apart. SF's
`ceval_step` has no error outcome (`None` means out of gas), and Leroy & Grall
and *Concrete Semantics* have no bound.

### Standard theorem forms

| Name | Form | Source |
|---|---|---|
| Big-step/small-step equivalence (terminating runs) | `a ⇒ v ⇔ a →* v ∧ v value` | Leroy & Grall Thm 9 (§4, "Relation with small-step semantics") |
| Big-step/small-step, diverging runs (unnamed in the source) | `a ⇒∞ ⇔ a →∞` | Leroy & Grall Thm 11 (§4) |
| `big_iff_small` | `cs ⇒ t ⇔ cs →* (SKIP, t)` | Isabelle HOL-IMP `Small_Step` |
| Semantic equivalence | `e →* e′ ⇒ ∃n. eval n e ∼ e′`, and `eval n e = r ⇒ ∃e′. e →* e′ ∧ r ∼ e′` | Amin & Rompf Thm 2 (§3.1). `∼` relates a timeout to any term, so the first direction holds trivially at n = 0; the substance is in their Coq lemmas `big_to_small` / `small_to_big` |
| Functional ⇔ relational big-step | `(t,s) ⇓ r ⇔ ∃c. sem_t (s with clock c) t = r ≠ Rtimeout` (simplified); always timing out ⇔ `⇑` | Owens §3.4 ("Comparison with proof in relational semantics") |
| Equivalence with small-step | the section title for relating the clocked interpreter to a small-step semantics. The theorem stated there goes one way: every result of the clocked interpreter has a matching small-step trace, long enough that divergence carries over | Owens §7 |
| Clock lemma (unnamed; Owens call it "an analogue of determinism") | Not timed out at clock c ⇒ the same result at every c + k | Owens §3.4; SF `ceval_step_more` |
| Soundness via a definitional interpreter | `⊢ e : T ∧ eval n e = r ≠ Timeout ⇒ r = Val v ∧ v : T` | Amin & Rompf Lemma 3 (their Thm 1 states it for the partial `evalp`); Owens §5 (where the result may also be an exception); Siek |

**The name of the eval ⇔ small-step theorem.** The dominant name is
**equivalence of big-step and small-step semantics**. Owens §7 and
*Concrete Semantics* §7.3.1 use it in a title, Leroy & Grall use the word in
the prose of §4, and Isabelle's theorem is `big_iff_small`. For a fuelled
interpreter against a small-step relation, Amin & Rompf's **semantic
equivalence** is the closest verified form. That is the recommended name,
because it is the one the sources use for exactly this pair of relations.
"Adequacy" is not wrong for it, but it is less specific, because the word has
at least three senses in the sources:

- computational adequacy: a denotational semantics agrees with an operational
  one (Plotkin 1977, via Niu, Sterling & Harper);
- adequacy of a semantic model: `⊨` implies safe (Timany; RustBelt Thm 7.2);
- adequacy of a formal semantics to an intended one: Amin & Rompf §§1–2 use it
  for a reduction semantics against the language it models, and Charguéraud
  2013 calls one operational semantics "adequate with respect to" another,
  which is the sense closest to ours.

### Terms we currently use that differ from this

| Our term | Accepted term | Confidence |
|---|---|---|
| "semantic equivalence", "the semantic-equivalence lemma", `Equivalence.lean` (`eval` ⇔ `Step*`; formerly "adequacy", "the adequacy lemma") | equivalence of big-step (definitional-interpreter) and small-step semantics; "semantic equivalence" (Amin & Rompf). Charguéraud's "adequate with respect to" is the same kind of relation | clear since RUE-2466, which adopted Amin & Rompf's name ("adequacy" was used in this sense by Charguéraud, but more often for the two relations in the list above). Ours also holds only on checked programs: on other input only the one-directional `run_sim` holds (`Equivalence.lean`), whereas Leroy & Grall's and Amin & Rompf's theorems are unconditional |
| `eval_big_to_small` / `eval_small_to_big` | the two directions of the equivalence (`big_to_small` / `small_to_big` in Isabelle HOL-IMP; SF's `eval__multistep` / `multistep__eval` relate a *relational* big-step semantics to multistep) | clear: the names follow HOL-IMP (RUE-2466). Before, `eval_sound` / `eval_complete` used "sound" for a simulation direction, next to the type-soundness theorem `soundness` in the same package |
| "definitional interpreter" `eval` (ADR-0097) | definitional interpreter (Amin & Rompf §2.1) | clear |
| fuel, `.outOfFuel` | fuel (Amin & Rompf), clock (Owens); timeout | clear |
| `fuel_mono` | no accepted name. The same statement is Owens' unnamed §3.4 lemma ("an analogue of determinism") and SF's `ceval_step_more` | clear (same statement) |
| `no_masking` (no fuel hides a violation) | follows from monotonicity. No separate name was found | none |
| `eval_diverges_iff` (out of fuel at every bound ⇔ §6 never halts) | the diverging-runs half of the equivalence (Leroy & Grall Thm 11; Owens §3.4). Our small-step side is "runs of every length" rather than a coinductive `→∞`; the two agree for a deterministic relation | clear |

---

## 4. Substructural types

**Sources:** Walker 2005 (ATTAPL ch. 1); Tov & Pucella 2011; McBride 2016;
Atkey 2018; Bernardy et al. 2018 (Linear Haskell).

### Accepted terms

| Term | Meaning | Source |
|---|---|---|
| structural rules | Exchange, weakening and contraction on the typing context | Walker §1.1 |
| exchange / weakening / contraction | Assumptions may be reordered / may go unused / may be used twice | Walker §1.1 |
| unrestricted | All three rules: any number of uses | Walker (`un`). Tov & Pucella's `U` is "unlimited" |
| affine | Exchange and weakening: at most one use | Walker; Tov & Pucella (`A`) |
| relevant | Exchange and contraction: at least one use | Walker |
| linear | Exchange only: exactly one use | Walker |
| ordered | No structural rules: exactly one use, in order | Walker §1.4 |
| qualifier | The annotation that puts a type in one of these classes | Walker §1.2 (`lin`, `un`); Tov & Pucella ("usage qualifier") |
| containment rules | "Unrestricted data structures may not contain linear data structures. More generally, data structures with less restrictive type may not contain data structures with more restrictive type": the predicate `q(T)` says which types a `q`-qualified structure may hold | Walker §1.2 |
| input context / output context | In the algorithmic judgment `Γin ⊢ t : T; Γout`, the context checking starts from, "some portion of which will be consumed", and the context left over, "synthesized alongside the type T" | Walker §1.2, "Algorithmic Linear Type Checking"; Oxide App. B.4 ("producing output context") |
| context split | `Γ = Γ₁ ∘ Γ₂`, which distributes linear assumptions between subterms | Walker Fig. 1-4 |
| dereliction subtyping | An unlimited-use function may be used where a one-use function is expected, after linear logic's dereliction rule | Tov & Pucella |
| multiplicity | An arrow or binder annotation: 1, ω, a variable, or a sum or product of these. Multiplicities form a semiring without a zero | Linear Haskell §3.2 |
| usage; semiring | QTT's annotations, which form a semiring; {0, 1, ω} is one example | Atkey §2.1.1; McBride ("rig"; his ω is relevant, unbounded use unless weakening is added) |

### Symbols

| Symbol | Reading | Variants |
|---|---|---|
| `q ∈ {lin, un}` | qualifier | `{U, A}` (Tov & Pucella); `π ∈ {1, ω}` (Linear Haskell); ρ in a rig (McBride, Atkey) |
| `q₁ ⊑ q₂` | Walker: "q₁ is more restrictive" (defined over systems in §1.1, Fig. 1-2, and used for qualifiers in §1.2); `lin ⊑ un` | **Direction varies.** In Tov & Pucella, U is the bottom and A the top: more restrictive is higher |
| `Γ₁ ∘ Γ₂` | context split | `Γ₁ + Γ₂` (Atkey, Linear Haskell); McBride writes contexts `Δ` |
| `πΓ` | a context scaled by a usage | McBride, Atkey |
| `A ⊸ B`, `A →π B` | linear / multiplicity-annotated arrow | Linear Haskell |

**Follow:** unrestricted / affine / linear, with "qualifier" for the
annotation. State the order's direction every time it is used.

### Standard theorem forms

| Name | Form | Source |
|---|---|---|
| Walker's classification (a definition, not a theorem) | ordered ∅; linear {E}; affine {E, W}; relevant {E, C}; unrestricted {E, W, C} | Walker §1.1 |
| Preservation, progress (linear λ) | the standard forms | Walker Thms 1.2.11, 1.2.12 |
| Algorithmic soundness | `Γ₁ ⊢ t : T; Γ₂ ∧ L(Γ₂) = ∅ ⇒ Γ₁ ⊢ t : T` | Walker 1.2.9 |
| Weakening, admissible at usage 0 | variables of usage 0 may be added | Atkey Lemma 2.4 |
| Resource-scaled substitution | substituting consumes `πΔ` | Atkey Lemma 2.5 |

### Terms we currently use that differ from this

| Our term | Accepted term | Confidence |
|---|---|---|
| "qualifier" `qual(T) ∈ {Copy, Affine, Linear}` (§3; `Qual`; formerly "class", `class(T)`) | qualifier (Walker), usage qualifier (Tov & Pucella) | clear: the same role and, since RUE-2466, the same word |
| `Copy` | unrestricted (`un`, Walker), unlimited (`U`, Tov & Pucella). "Copy" is Rust's trait name | partial |
| `Affine`, `Linear` with §3's rule glosses | affine, linear; §3 names contraction and weakening correctly | clear |
| "multiplicity lattice" (§3), `Qual` | a qualifier order (Walker, Tov & Pucella). "Multiplicity" (Linear Haskell) means an arrow annotation in a semiring generated by 1 and ω | partial: the word comes from a neighboring framework with a different structure |
| `Copy ⊑ Affine ⊑ Linear` ("more restrictive is higher") | Tov & Pucella's direction (`U ⊑ A`). The reverse of Walker's (`lin ⊑ un`) | clear, once the direction is stated |
| "infectious" (a struct takes its fields' join) | Walker's containment rules (a container is at least as restrictive as what it contains: an `un` pair cannot hold a `lin` component); Tov & Pucella give a product the least upper bound `⊔` of its components' kinds | partial: the same constraint, with no single-word name |
| `Typed … Ω`, the "input context" and "output context" (formerly "incoming" and "outgoing state") | Walker's algorithmic `Γin ⊢ t : T; Γout` (input / output context, §1.2); Oxide's output context `⇒ Γ′` | clear for the words; partial for `Ω`, which also carries the abrupt-completion contexts `Δ` |
| `check_sound` | algorithmic soundness (Walker 1.2.9) | clear |
| "Copy containment", "copy-contained" (the metatheory; `Contents.copyContained`; formerly "Copy closure", "copy-closed") | Walker's containment rules (§1.2) | clear: nothing owned sits under a `Copy` node, the containment rule for the least restrictive qualifier |

---

## 5. Ownership, borrowing, moves and drop

**Sources:** RustBelt (Jung et al. 2018); Oxide (Weiss et al. 2019/2021); the
Rust Reference (*Destructors*, *Expressions*, *Glossary*, *Patterns*); the
Rustonomicon (*Drop Flags*, *Destructors*); the rustc-dev-guide (*Move paths*,
*Drop elaboration*); the Polonius book (*Atoms*); the Rust Book §4.1; Swift
SE-0176 (*Enforce Exclusive Access to Memory*); the CWE entries 416, 415 and
401; Hicks 2014; ISO C N1570 §6.8.

### Accepted terms

| Term | Meaning | Source |
|---|---|---|
| place expression / value expression | Denotes a memory location / denotes a value (formerly lvalue / rvalue) | Ref. *Expressions* |
| place | Oxide: a place expression with no dereference | Oxide |
| move path | A location that can be initialized or moved. Move paths form a tree | rustc-dev-guide *Move paths* |
| moved from; moved out of; deinitialized | After a move out of a place, the place is deinitialized: "After moving out of a place expression that evaluates to a local variable, the location is deinitialized" | Ref. *Expressions* (`[expr.move.deinitialization]`); Ref. *Glossary* |
| initialized / uninitialized | Assigned and not moved from since / otherwise | Ref. *Glossary* |
| maybe-initialized / maybe-uninitialized | The two dataflow analyses over move paths | rustc-dev-guide |
| partial move; partially initialized | Some fields have been moved out. Only the initialized fields are dropped | Ref. *Patterns*; Ref. *Destructors* |
| dead type `τ†`; maybe-dead type | Oxide's type for a moved-out place / for an aggregate with some parts moved | Oxide |
| owner | Every value has one, and the value is dropped when its owner goes out of scope | Rust Book §4.1 |
| borrow; loan | A loan records a borrow: a path plus a mutability | Polonius *Atoms*; Oxide |
| lifetime / region / origin | κ (RustBelt); regions as provenances (Oxide); origin = a set of loans (Polonius) | as listed |
| unique / shared reference | RustBelt's `mut`/`shr`; Oxide's `uniq`/`shrd` | RustBelt §3.3; Oxide |
| destructor; dropped | What runs when an initialized variable or temporary leaves scope | Ref. *Destructors* |
| drop scope; drop order | Variables are dropped in reverse order of declaration, temporaries in reverse order of creation; struct fields in declaration order; arrays from first element to last | Ref. *Destructors* |
| drop glue | Calls `Drop::drop` if implemented, then the drop glue of every field | rustc-dev-guide *Drop elaboration* |
| drop obligation | When a local variable becomes initialized, it establishes "a set of structural paths (e.g. a local `a`, or a path to a field `b.f.y`) that need to be dropped"; moving from a path releases the obligations for it and its descendants | rustc-dev-guide *Drop elaboration*, quoting RFC 320 |
| drop flag | A per-variable runtime flag recording whether a drop is still owed | Rustonomicon *Drop Flags* |
| drop elaboration; static / dead / conditional / open drop | Rewriting drops into code guarded by flags. The target is always initialized / always uninitialized / either wholly initialized or wholly uninitialized / possibly partly initialized. "Dynamic drops" is the guide's heading for the flag-based scheme (RFC 320), not a kind | rustc-dev-guide *Drop elaboration* |
| substructural (context) | RustBelt's typing context is substructural | RustBelt §2, §3.3 |
| law of exclusivity | A modification of a variable must be exclusive with any other access to it | Swift SE-0176, which takes the name from the Swift Ownership Manifesto |
| ownership / sharing predicate | `⟦τ⟧.own`, `⟦τ⟧.shr` | RustBelt §4 |
| use after free | The program "reuses or references memory after it has been freed". Alternate terms: dangling pointer, UAF | CWE-416 |
| double free | The program "calls free() twice on the same memory address" | CWE-415 |
| memory leak | Allocated memory is not released after its last use, so it cannot be reused. CWE's title is "Missing Release of Memory after Effective Lifetime", and it discourages "memory leak" because the phrase also names disclosure of memory contents | CWE-401 |
| memory safety | No agreed formal definition. One common way to define it: an execution is memory safe when no memory access error occurs (buffer overflow, null pointer dereference, use after free, use of uninitialized memory, illegal free). Hicks's own: no access to undefined (unallocated or freed) memory, with pointers as capabilities. Hicks counts leaks as outside memory safety | Hicks 2014 (the post, and his reply in its comments) |
| full expression | An expression that is not part of another expression or of a declarator. There is a sequence point between one full expression and the next | C11 N1570 §6.8¶4 |

Rust's own documentation does not call Rust "affine": the word occurs 0 times
in the Rust Reference, the Book, the Rustonomicon and the rustc-dev-guide
(their single-page `print.html` editions, searched).

### Symbols

| Symbol | Reading | Variants |
|---|---|---|
| `Σ; Δ; Γ; Θ ⊢ e : τ ⇒ Γ′` | Oxide's typing: with global environment Σ, type environment Δ, stack typing Γ and temporary typing Θ, e has type τ and the stack typing becomes Γ′ | Oxide v1 (2019) writes `Σ; Δ; Γ ⊢ e : τ ⇒ Γ′` |
| `τ†`, `τ^sx` | dead type; maybe-dead type | Oxide |
| `ℓ = ω p`, `ω ∈ {uniq, shrd}` | a loan of place p | Oxide |
| `own_n τ`; `&^κ_mut τ`, `&^κ_shr τ` | owned pointer; unique / shared reference with lifetime κ | RustBelt |
| `⊨` | semantic typing | RustBelt §7 |

### Standard theorem forms

| Name | Form | Source |
|---|---|---|
| Fundamental theorem | Replacing every `⊢` in a typing rule with `⊨` gives a valid Iris theorem | RustBelt Thm 7.1 |
| Adequacy | A closed function `f` semantically well typed at `fn() → ()`, run with the default continuation, never reaches a stuck state | RustBelt Thm 7.2 |
| Syntactic type safety | Progress, preservation and type safety for Oxide | Oxide Lemma 3.1 (Progress), Lemma 3.3 (Preservation), Thm E.70 (Type Safety) |

### Terms we currently use that differ from this

| Our term | Accepted term | Confidence |
|---|---|---|
| "moved-out" (formerly "hole"), `⊘`, `Contents.movedOut` (an uninitialised or moved-out cell, §6.1) | moved out of / moved from / deinitialized / uninitialized (Rust Reference); dead (Oxide) | clear: the Rust Reference's word, for a cell's contents rather than a place |
| `MovedOut` (`OwnSt.movedOut`) | moved from; `τ†` (Oxide) | clear |
| `OwnSt.fields` ("a partially moved value") | partial move / partially initialized (Ref.); maybe-dead type (Oxide); the move-path tree (rustc) | clear |
| "residue": first (§4) `residue(T, π)`, the places a declared-linear destructure leaves unselected, which are dropped at once; also (Dynamics, GUIDE) what remains of a partially moved cell | the initialized fields of a partially moved value. There is no accepted noun | partial |
| "place", "path" (§4, `Place.path`) | place (Ref., Oxide); path (Polonius); move path (rustc) | clear |
| drop scope `s`, dropped "newest-first" (§6.1; formerly "scope record") | drop scope; reverse order of declaration | clear |
| §6.11's drop order (a value's destructor first, then its contents by kind: a struct's fields in declaration order, an array's elements in ascending order, an enum's active payload only) | drop glue; drop order (Ref. *Destructors*) | clear |
| the dynamic `⊘` skip during a drop walk | the job drop flags do: conditional and open drops. GUIDE already calls it a per-element drop flag | partial: the same job, but the state lives in the cell rather than in a flag |
| `Owned` / `MovedOut` (Σ's two states, §5); the `Borrowed` place-use mode; `inout`/`borrow` parameters | owner; moved from; borrow / loan; unique / shared reference | partial |
| "law of exclusivity" (§5.4) | law of exclusivity (Swift SE-0176); in Rust terms, unique (`mut`/`uniq`) vs shared (`shr`/`shrd`) references | clear for the Swift term |
| "memory safety", "the memory-safety-without-GC claim" (README; 01 §7: "These seven are the memory-safety-without-GC claim, decomposed") | memory safety (Hicks 2014): no memory access error | partial: the field has no single definition. Ours is a list of theorems, and two of them go past Hicks's reading: leak-freedom, which he puts outside memory safety, and linear values consumed exactly once |
| "No use-after-free" (01 §7; the metatheory's heading, not yet mechanized): no reduction applies a §6.13.1 machine operation to a dead buffer allocation | use after free (CWE-416) | clear, for buffers: it is stated only over §6.13's block store |
| "No double-free", `no_double_free` (01 §7; the metatheory): no destructor runs twice, and no identity is freed twice in a trace | double free (CWE-415) | partial: CWE's is one `free()` of the same address twice; ours counts destructor runs and `drop`/`dropTemp` events per object identity |
| "No use-after-drop", `no_use_after_drop` (01 §7; the metatheory): no evaluation reads a tombstoned (`†`) cell | use after free (CWE-416), for a dropped binding's cell | partial: the same kind of error. "Drop" is Rust's word (Ref. *Destructors*) for what CWE calls freeing |
| "no leak of drops" (01 §7; the metatheory; `drop_exactly_once`, `Tidy`): every owned, droppable, non-moved place is dropped exactly once, at the end of its scope | memory leak (CWE-401) is the failure of the "at least once" half | partial: ours also has the "at most once" half, and it counts drops (destructor runs), not memory released |
| "full expression" (01 §5.8, RUE-1279: the extent of an accessor loan) | full expression (C11 N1570 §6.8¶4) | partial: the same syntactic notion, an expression not inside another, used here for a loan's extent; C uses it for sequence points |
| "drop obligation", "has a drop obligation" (01 §6.7, §6.9; formerly "owes `x` a drop") | drop obligation (RFC 320; rustc-dev-guide *Drop elaboration*) | clear: a binding the scope will drop at its exit; RFC 320's obligations are per structural path, ours per cell |

---

## 6. Trace properties and runtime monitors

**Sources:** Lamport 1977; Alpern & Schneider 1985; Schneider 2000; Leucker &
Schallhart 2009; Clarkson & Schneider 2010; ISO C N1570 §5.1.2.3; the
Confluent Kafka documentation (delivery semantics).

### Accepted terms

| Term | Meaning | Source |
|---|---|---|
| execution; partial execution | An infinite sequence of states (a terminating execution repeats its final state forever); an element of `S*` | Alpern & Schneider |
| run; trace | A possibly infinite sequence of states or events; an execution is a finite prefix of a run, a finite trace | Leucker & Schallhart |
| trace property | A set of infinite traces | Clarkson & Schneider §2.1 |
| safety property | Nothing bad happens: every violation has a finite prefix that no continuation can repair. The term is Lamport's | Lamport 1977 (per Lamport's own bibliography and Alpern & Schneider p. 181); Alpern & Schneider §2 |
| liveness property | Something good eventually happens: every finite prefix can be extended to satisfy it | Alpern & Schneider §3 |
| bad prefix / good prefix | A finite prefix that decides violation / satisfaction | Leucker & Schallhart (after Kupferman & Vardi) |
| monitor | A device that reads a finite trace and yields a verdict | Leucker & Schallhart |
| verdict | A monitor's output: true, false, or (LTL₃) inconclusive | Leucker & Schallhart |
| runtime verification | Checking whether a run satisfies or violates a property | Leucker & Schallhart |
| online / offline monitoring | Checking the execution while it runs / checking recorded traces | Leucker & Schallhart |
| monitorable | A monitor can still reach a verdict. This class is strictly larger than safety ∪ co-safety | Leucker & Schallhart (the definition is Pnueli & Zaks's; the strictness result is Bauer, Leucker & Schallhart's) |
| EM (Execution Monitoring); security automaton | EM is the class of enforcement mechanisms that monitor a target's execution and terminate it before it violates the policy; a security automaton is the recognizer such a mechanism runs | Schneider 2000 §§1, 3 |
| hyperproperty; hypersafety | A set of trace sets; safety lifted to that level | Clarkson & Schneider |
| at most / at least / exactly once | Message-delivery guarantees ("exactly once" = delivered once and only once) | Confluent Kafka docs |
| exactly / at most / at least once (use) | linear / affine / relevant use of a variable | Walker (§4 above) |
| observable behavior | Volatile accesses, final file contents and interactive I/O | C11 N1570 §5.1.2.3¶6 |

### Symbols

| Symbol | Reading | Variants |
|---|---|---|
| `S^ω`, `S*` | infinite / finite sequences of states | `Σ^ω`, `Σ*`; Clarkson & Schneider write `Ψ_inf`, `Ψ_fin` |
| `σ ⊨ P` | "σ satisfies P", i.e. σ ∈ P | `T ⊨ P` iff `T ⊆ P` for trace sets |
| `σᵢ` / `σ[..i]` | the prefix of length i | Alpern & Schneider / Schneider 2000. Clarkson & Schneider's `t[..i]` is `s₀…sᵢ`, one state longer |
| `⊤, ⊥, ?` | the verdicts true, false and inconclusive | Leucker & Schallhart |

### Standard theorem forms

| Name | Form | Source |
|---|---|---|
| Safety (formal) | `∀σ ∈ S^ω. σ ⊭ P ⇒ ∃i. ∀β ∈ S^ω. σᵢβ ⊭ P` | Alpern & Schneider §2 |
| Liveness (formal) | `∀α ∈ S*. ∃β ∈ S^ω. αβ ⊨ P` | Alpern & Schneider §3 |
| Decomposition | Every property is the intersection of a safety property and a liveness property | Alpern & Schneider Thm 1 |
| EM enforces only safety | A policy an EM mechanism enforces is a safety property; the converse fails | Schneider 2000 §2; the converse's failure, §4 |
| Hyperproperty decomposition | Every hyperproperty is the intersection of a safety and a liveness hyperproperty | Clarkson & Schneider Thm 5 |

The sources disagree on what an execution is. Alpern & Schneider's execution and
Clarkson & Schneider's trace are infinite, and a terminating run is padded by
repeating (stuttering) its final state. Leucker & Schallhart's execution is a
finite prefix of a run. Schneider 2000's safety quantifies over finite and
infinite executions and is credited to Lamport 1985, so it differs from Alpern
& Schneider's. Our properties over *terminating* traces (`Blocks`, `no_double_free`)
become ordinary trace properties in the infinite-trace sense by the same
stuttering padding. `step_no_double_free` states the at-most-once bound in the
safety form itself: over the trace of every configuration a run reaches, a
finite prefix, whether or not the run terminates (RUE-2477).

### Terms we currently use that differ from this

| Our term | Accepted term | Confidence |
|---|---|---|
| "monitor" (`linearLeak`, `linearOverwrite`, `linearDiscard`, `ownedUnderCopy`): a check the interpreter `eval` adds, which refuses the step (`.refused w`); `Step` has none | an EM mechanism (Schneider 2000): it terminates the target before a violating step, and Schneider counts a virtual machine whose instruction cycle is augmented this way as EM. In runtime verification a monitor typically only returns a verdict and does not change the execution (Leucker & Schallhart) | partial. Mechanically ours match Schneider's enforcement sense. But they are part of the definitional interpreter rather than isolated from a target, they exist so the safety proof can go through, they make `eval` stricter than §6, and they never fire on checked programs |
| drop trace, `Event`, `tr` | trace of events | clear |
| `no_double_free` (each identity is freed at most once, and each destructor runs at most once, in every run's trace) | an at-most-once **safety** property over traces | clear once stated this way |
| `drop_exactly_once` ("consumed exactly once") | "exactly once" (delivery) = at most once ∧ at least once; linear use = exactly once. Ours is per value, per terminating evaluation | partial |
| `Blocks`, `run_blocks` (every terminating run's trace is in a grammar) | a trace property over terminating traces. No accepted name for this shape | none |
| `Tidy` / `eval_tidy` (every cell an evaluation allocates is tombstoned by its end) | no verified counterpart | none |
| "identity ledger" (trace renderings; formerly "explain renderings") | no verified counterpart | none |
| "observable outcome" (`@dbg` output, exit code; GUIDE §2) | observable behavior (C11; CompCert) | clear |

---

## 7. Testing against an executable semantics

**Sources:** McKeeman 1998; Yang, Chen, Eide & Regehr 2011 (Csmith);
Regehr et al. 2012 (C-Reduce); Claessen & Hughes 2000 (QuickCheck); Barr et
al. 2015; Chen et al. 2020 (survey; numbering below is from its 2019
preprint); Disselkoen et al. 2024 (Cedar); Leroy
2009 (CACM) and 2009 (JAR); Kumar et al. 2014 (CakeML); Pnueli, Siegel &
Singerman 1998; Necula 2000; the libFuzzer documentation; DeMillo, Lipton &
Sayward 1978; Jia & Harman 2011; Beer, Ben-David, Eisner & Rodeh 2001;
RFC 2119.

### Accepted terms

| Term | Meaning | Source |
|---|---|---|
| differential testing | Run the same tests on comparable systems. A difference, hang or crash marks a candidate bug | McKeeman 1998 (Csmith §5 credits him with the term) |
| randomized differential testing | Differential testing on random inputs. No result oracle is needed | Csmith §2.1 (citing McKeeman). The survey's §7 uses "randomized differential testing (RDT)" more narrowly, for comparing different compilers |
| differential random testing (DRT) | Cedar's check that the Lean model and the production code agree on millions of inputs generated with the cargo-fuzz framework | Cedar §4 |
| verification-guided development | Write an executable model and prove properties of it; check the production code against it with DRT; and use property-based testing | Cedar §1 |
| executable model / specification | Cedar: the models serve as the specification | Cedar §3 |
| test oracle; oracle problem | A procedure that decides whether a test's behavior is correct; the difficulty of obtaining one | Barr Def. 2.4; the oracle problem is named in the abstract |
| specified / derived / implicit oracle | An oracle from a formal specification / from another artifact / from obvious failures such as crashes | Barr §3 |
| pseudo-oracle | An independently written version of the program, used as the oracle. Barr present it as a kind of derived oracle | Barr §5.1 |
| metamorphic testing; EMI | Relations between inputs and outputs; variants equivalent on the given inputs | Barr §5.2; survey §4.2 |
| randomized test-case generator | Csmith's description of itself | Csmith §1 |
| wrong-code error / bug | A run-time failure of the compiled program: a wrong result, a crash, or wrong termination behavior. A *silent* wrong-code error is one the compiler produced without any warning | Csmith §3.1 |
| test-case reduction | Heuristic reduction of a failing input, deliberately not called minimization | C-Reduce §3.1 |
| property; generator (`Gen`, `Arbitrary`) | QuickCheck's vocabulary. "Shrinking" does **not** appear in the 2000 paper | QuickCheck §§2–3 |
| seed | Several senses: the initial inputs a fuzzer's corpus is seeded with (libFuzzer: "initial seeds", "seed" sample inputs); a random-number-generator seed (libFuzzer's `-seed=N`; Csmith §3.4); "a skeletal program called seed" (survey §3.3.2); the failure-inducing input `i_seed` that reduction starts from (C-Reduce §3.1) | as listed |
| corpus | The saved set of test inputs | libFuzzer; Cedar §4 |
| semantic preservation | The compiled code behaves as the source semantics prescribes | Leroy CACM §2.1 |
| verified compiler | A compiler with a proof that `Comp(S) = OK(C) ⇒ S ≈ C` | Leroy CACM §2.2 |
| translation validation; validator | A per-run check that establishes `S ≈ C` for one compilation, for all its executions | Pnueli et al.; Necula §1; Leroy CACM §2.2 |
| verified validator | A validator with a proof that `Validate(S,C) = true ⇒ S ≈ C` | Leroy CACM §2.2 |
| certifying compiler | A compiler that emits a proof alongside its code; only the client-side checker is trusted | Leroy CACM §2.2 (Necula §1 mentions one, the Touchstone compiler) |
| forward / backward simulation | Source behaviors are preserved / target behaviors are allowed by the source. Backward simulation is also called refinement | Leroy JAR §2.1 |
| trusted computing base | What must be trusted for a guarantee to hold | CakeML §1 |
| program mutation; mutation; mutant | Judge how adequate a test set is by running it on *mutations* of the program: copies that differ from it only by simple errors (for example `.LE.` replaced by `.EQ.`) | DeMillo et al. 1978, p. 36 |
| dead / live mutant; kill | A mutant is *dead* when the test data gives it a result different from the program's, and *live* otherwise; testers try to *kill* the live ones | DeMillo et al. 1978, pp. 36–37 |
| killed / survived | A mutant is killed when its result differs from the original's on some test, and has survived otherwise | Jia & Harman §II.B |
| equivalent mutant | A mutant that always produces the same output as the original program, so no test can kill it. DeMillo et al. describe the case ("live mutants that are equivalent to P") without the compound noun | Jia & Harman §II.B; DeMillo et al. 1978, p. 36 |
| mutation score | The number of killed mutants divided by the number of non-equivalent mutants | Jia & Harman §II.B |
| coupling effect | Test data that distinguishes all programs differing from a correct one by simple errors also distinguishes more complex errors. An empirical principle, not a theorem | DeMillo et al. 1978, p. 35; Jia & Harman §II.A |
| mutation testing; mutation analysis | Jia & Harman's names for the method. In mutation analysis, mutants are generated from a program by small syntactic changes, the tests are run on them, and the goal is to raise the mutation score | Jia & Harman §§I, II.B |
| vacuity; vacuous | A formula is vacuous in a model when one of its sub-formulas does not affect its truth there. Antecedent failure, an implication that holds because its premise never does, is the simplest case | Beer et al. 2001, §3 Def. 2; abstract |
| interesting witness | For a formula that holds non-vacuously, a non-trivial example of it holding: "it proves non-vacuity, while a counter-example proves non-validity" | Beer et al. 2001, §4.3, Def. 20 |
| MUST, MUST NOT (key words) | An absolute requirement / an absolute prohibition of a specification | RFC 2119 §§1–2 |

### Symbols

| Symbol | Reading | Variants |
|---|---|---|
| `S ⇓ B` | "S has observable behavior B" | CakeML `REPLs l i o` |
| `S ≈ C` | "C preserves the semantics of S" | — |
| `Safe(S)` | S cannot go wrong | Leroy JAR; CACM writes "S safe" |
| `∼` | a simulation invariant | Necula writes Σ for a simulation relation |

`Beh(C) ⊆ Beh(S)` and `S ⊑ C` are common restatements of backward simulation,
but neither appears in the CompCert sources fetched here. Quote
`∀B, C ⇓ B ⇒ S ⇓ B`.

### Standard theorem forms

| Name | Form | Source |
|---|---|---|
| Backward simulation | `∀B, C ⇓ B ⇒ S ⇓ B` | Leroy JAR Def. 2 |
| Safe backward simulation (CompCert) | `Safe(S) ⇒ ∀B, C ⇓ B ⇒ S ⇓ B` | JAR Def. 3; CACM eq. 2 |
| Forward simulation | `∀B, S ⇓ B ⇒ C ⇓ B`. It implies backward simulation (Def. 2) when C is deterministic | JAR Def. 4 |
| Safe forward simulation | `∀B ∉ Wrong, S ⇓ B ⇒ C ⇓ B`. It implies safe backward simulation (Def. 3) when C is deterministic | JAR Def. 5; CACM eq. 3 |
| Verified compiler / validator | `Comp(S) = OK(C) ⇒ S ≈ C` / `Validate(S,C) = true ⇒ S ≈ C` | CACM eqs. 6, 7 |
| QuickCheck property | `∀x. P x`, checked on random x | QuickCheck §2.1 |

**Where differential testing sits.** Our differential testing compares the model with the
implementation twice: the compiler's accept/reject against the Lean checker's
verdict, and the observations of the oracle and the native binary against the
Lean interpreter's outcome (ADR-0097; [lean/README.md](lean/README.md)). That
is **differential testing against an executable model** (McKeeman's
differential testing), with the model as a specified test oracle (Barr). It is
Cedar-style: Cedar also compares production code with a Lean model. But the
default corpus is about 170 hand-written cases, and random generation
(`Gen.lean`, `--gen N --seed S`) runs only on request, so only the `--gen` mode
is random testing in Cedar's DRT sense. Cedar's DRT generates millions of
inputs with the cargo-fuzz framework and sends each to both the Lean model and
the Rust implementation; ours prints each core program as a Rue module first. It
is **not translation validation**, which establishes `S ≈ C` for every
execution of one compilation and must itself be proved sound. It is not
verified compilation either. For one closed, deterministic, terminating
program, a run checks the single behavior that program has, which is one
instance of backward simulation. The check is not proof-producing, and it
trusts the runner, the printer and the model.

### Terms we currently use that differ from this

| Our term | Accepted term | Confidence |
|---|---|---|
| "differential testing", "differential corpus" (ADR-0097; [lean/README.md](lean/README.md); formerly "the bridge", "bridge corpus") | differential testing against an executable model; in `--gen` mode, randomized differential testing (Csmith) / DRT (Cedar) | clear since RUE-2466 for the activity; "differential corpus" is our name for its test set. The tool names (`lean-bridge`, BRIDGE-SENSITIVITY.md) keep the old word |
| "differential-tested", `rue-oracle-diff` | differential testing | clear |
| "oracle" (`rue-oracle`, the executable reference interpreter) | test oracle, specifically a pseudo-oracle, which Barr class as a derived oracle. The accepted term names the pass/fail judge, not the interpreter itself | partial |
| "executable reference interpreter" / "the executable semantics" (§6) | executable model / specification (Cedar) | clear |
| "seed cases", "seed corpus" (the hand-written corpus cases) | the initial seeds of a fuzzing corpus (libFuzzer) | partial: fuzzing seeds are the initial inputs that mutation starts from |
| `--seed N`, `gen_<seed>_<i>` | random seed (libFuzzer `-seed=N`; Csmith §3.4) | clear |
| generated programs (`Gen.lean`) | random program generation | clear |
| "verdict" (the checker's accept or reject on a case) | no counterpart. In runtime verification a verdict is a monitor's output (§6), a different object | none |
| "model gap", "gap registry" (01 §6.13.6; these belong to `rue-oracle`, not to the Lean model) | no verified counterpart | none |
| "red case", "disagreement" | a candidate for a bug-exposing test (McKeeman: the results differ, or one system hangs or crashes) | partial |
| "real-fault mutation run" (a re-introduced historical compiler bug that some differential-testing case kills; REDTEAM.md "caught or not", pending sync) | mutation testing (Jia & Harman; DeMillo et al.). The re-introduced bug plays the role of a mutant of the compiler, and "killed" (Jia & Harman) is now our word too | partial: the mutant is a real past bug, not the output of a mutation operator, and the test set is the differential corpus |
| "mutation" of the definitions (REDTEAM.md: "drop a premise or weaken a rule and see whether any theorem or corpus case notices"; RUE-2465) | program mutation (DeMillo et al.), applied to the model's definitions; the mutants that survive are the live ones | clear |
| "non-vacuity witness" (REDTEAM.md: a program meeting every hypothesis, on which the conclusion is non-trivial) | interesting witness (Beer et al.), which shows that a formula holds non-vacuously | partial: the same role for a theorem's hypotheses, but Beer et al. define it for temporal formulas checked against a model, and their witness is a model or a path, not a program |
| "sharpness counter-example" (REDTEAM.md: a program just outside a hypothesis, showing the hypothesis is not slack) | no verified counterpart | none |

---

## 8. Lean proof vocabulary

**Sources:** *The Lean Language Reference* (fetched at 4.35.0-rc3); Avigad,
de Moura, Kong & Ullrich, *Theorem Proving in Lean 4* (TPIL); the Lean API
documentation (`Lean.ReducibilityAttrs`,
`Init.Tactics`); the Lean 4.29.0 release notes; Barendregt & Wiedijk 2005.

### Accepted terms

| Term | Meaning | Source |
|---|---|---|
| elaborator | Translates surface syntax into the core type theory, which keeps the kernel small | Reference §2 |
| kernel | A small type checker for the core type theory | Reference §2 |
| trusted code base | What a proof's validity depends on. Native evaluation extends it to the compiler | Reference, "Validating a Lean Proof" |
| de Bruijn criterion | A proof can be checked by a small independent program | Barendregt & Wiedijk §3 |
| lean4checker; comparator | Replays declarations through the kernel; checks a proof against a trusted challenge statement, replaying it with the kernel and/or an external checker and checking that the proved statements match the challenge file | Reference, "Validating a Lean Proof" |
| axiom | A postulated constant. Axioms do not reduce, so a closed term that uses one can get stuck | Reference §8, §8.3 |
| standard axioms | `propext`, `Classical.choice`, `Quot.sound` | Reference, "Validating a Lean Proof"; `Init.Tactics`. Reference §8.4 counts four standard axioms, the fourth being `sorryAx` |
| `funext` | A theorem, proved from `Quot.sound` | TPIL "Axioms and Computation" |
| `sorry` / `sorryAx` | A placeholder that closes any goal, implemented by the axiom `sorryAx` | Reference §8.4 |
| `#print axioms` | Lists every axiom a declaration depends on, transitively | Reference §8.5 |
| definitional equality | Equality up to computation (β, δ, ι, ζ, η, proof irrelevance, quotients), which the kernel checks | Reference §4 |
| propositional equality | The type `a = b`. `rfl` proves terms with a common reduct equal | TPIL "Quantifiers and Equality" |
| reducible / semireducible / irreducible | How eagerly a definition unfolds. Semireducible is the default. The API now lists five statuses (also `instanceReducible`, `implicitReducible`) | `Lean.ReducibilityAttrs` |
| `theorem` vs `def` | A theorem's type is a `Prop`, and theorems are irreducible by default | Reference §7.4 |
| structural / well-founded recursion | Recursive calls on strict subterms / on a decreasing measure (`termination_by`, `decreasing_by`) | Reference §7.6 |
| `partial` | Opaque to the kernel: never unfolded, so its body cannot be reasoned about | Reference §7.6 |
| inductively defined proposition | An inductive type in `Prop` | TPIL §7.3 |
| `structure` | An inductive type with exactly one constructor and no indices; Lean generates a projection function for each field | Reference §4.4.2 |
| `mutual` block; mutual recursion | Definitions that may mention one another are declared together in `mutual … end` | Reference §7.6.1 |
| module; `@[expose]` | A source file whose header begins with `module` opts in to separating public from private information; even a public definition usually keeps its body private unless exposed, and `@[expose] public section` exposes the rest of a file | Reference §5.3 ("If a source file's header begins with `module` …"), §5.4 "Modules and Visibility", §5.6.1 (the porting recipe) |
| `decide` / `native_decide` | Evaluates a `Decidable` instance in the kernel / in compiled code, which adds an axiom. Up to 4.28 every use showed up as the one axiom `Lean.trustCompiler`; from 4.29 each use gets its own auto-generated axiom | `Init.Tactics`; Reference, "Validating a Lean Proof"; 4.29.0 release notes |
| `noncomputable` | Required on definitions whose non-proof (data-producing) code depends on axioms such as `Classical.choice`; proofs may use axioms freely | Reference §8.3; TPIL |

**"Definitional unfolding"** does not appear in the pages fetched. The accepted
phrasing is **definitional equality**, with unfolding (the manual's
**δ-reduction**) controlled by reducibility. Lean has no official symbol for definitional
equality; write "definitionally equal".

### Standard theorem forms

| Name | Form | Source |
|---|---|---|
| `propext` | `(a ↔ b) → a = b` | TPIL |
| `Quot.sound` | `r a b → Quot.mk r a = Quot.mk r b` | TPIL |
| Checking a proof's axioms (the source gives it no name) | `#print axioms T` ⊆ {`propext`, `Classical.choice`, `Quot.sound`}; any `sorryAx` means the proof is incomplete | Reference, "Validating a Lean Proof" ("Printing Axioms") |

### Terms we currently use that differ from this

| Our term | Accepted term | Confidence |
|---|---|---|
| "kernel-checked", "zero `sorry`", "only `propext` and `Quot.sound`" | kernel; `sorry`; standard axioms; `#print axioms` | clear |
| "trust report" (`TRUST.md`) | a `#print axioms` check plus a statement of the trusted code base | partial |
| "definitional unfolding" (RUE-2459's own wording; not in our docs) | definitional equality; δ-reduction; reducibility | partial |
| "stuck" (our dynamics) | Lean also says "stuck" for a term whose reduction an axiom blocks | partial: two meanings of one word in one project |
| judgments as inductive types (GUIDE §1) | inductively defined propositions (TPIL) | clear |
| `eval` as a total function made terminating by fuel | a terminating definition (structural or well-founded recursion), as opposed to `partial` | clear |

## 9. General mathematics

**Sources:** Tarski 1955 §1; Tov & Pucella 2011 (§4 above); Harper 2016
(PFPL); Hutton 1999; Leijen 2001, which credits Boute 1992 **(record)** with
the Euclidean definition. No fetched source states the algebraic laws
(associative, commutative, idempotent), so the glossary classes them as ours,
pending audit.

### Accepted terms

| Term | Meaning | Source |
|---|---|---|
| lattice; complete lattice | A non-empty set with a partial order in which any two elements have a least upper bound (join) and a greatest lower bound (meet); complete when every subset has both. Tov & Pucella's qualifiers `U` and `A` are "the bottom and top of the two-element lattice" | Tarski 1955 §1; Tov & Pucella §4.1 |
| join; least upper bound | The least upper bound of two elements, which Tarski calls their join. Tov & Pucella write it `⊔`: the kind of a type "is the least upper bound of the kinds of the types that occur directly in its representation" | Tarski 1955 §1; Tov & Pucella §2 |
| fixpoint; least fixpoint | "an element x of the domain of f such that f(x) = x". For an increasing function on a complete lattice, the fixpoints form a complete lattice, so a least one exists | Tarski 1955 §1, Theorem 1 |
| α-equivalence; α-variant | Two terms are α-equivalent when they are identical up to the choice of bound variable names; PFPL identifies terms up to it | PFPL §1.2 |
| ill-formed / well-formed | A syntax tree that breaks a formation rule is ill-formed (§1.2); the statics derives that "an expression is well-formed of a certain type" (ch. 4) | PFPL §1.2; ch. 4 introduction |
| judgment; inference rule; derivation; rule induction | A derivation is "a finite composition of rules, starting with axioms and ending with that judgment", a tree whose nodes are rules | PFPL §§2.1–2.4 (the quote is §2.3) |
| fold; left fold (`foldl`) | `fold` replaces a list's nil by a value and each cons by a function (a right fold); `foldl` processes the elements left to right, from a starting value | Hutton §2, §5.1 |
| truncated division (T-division) | The quotient is rounded toward zero, so the remainder has the dividend's sign. C99's `/` and `%` | Leijen §1.1 |
| floored division (F-division) | The quotient is rounded toward negative infinity, so the remainder has the divisor's sign | Leijen §1.1 |
| Euclidean division (E-definition) | The unique `q`, `r` with `q` an integer, `D = d·q + r` and `0 ≤ r < \|d\|` | Leijen §1.2, crediting Boute 1992 |

### Symbols

| Symbol | Reading | Variants |
|---|---|---|
| `a ⊔ b` | "a join b" | Tov & Pucella write `⊔` for the least upper bound of kinds |
| `a =α b` | "a and b are α-equivalent" | PFPL |

### Terms we currently use that differ from this

| Our term | Accepted term | Confidence |
|---|---|---|
| "multiplicity lattice" (§3): `Copy ⊑ Affine ⊑ Linear` | a lattice: a three-element chain is one | clear for "lattice"; "multiplicity" is §4's partial term |
| `⊔` (§3), and §5.5's `join(Σ1, …, Σn)` (`OwnSt.join`, `Ctx.join`) | join (least upper bound) | clear for §3's order. §5.5's join acts on ownership states; `join_comm`, `join_assoc` and `join_idem` prove it commutative, associative and idempotent on well-formed states |
| "left fold" (`Ctx.joinAll`; the metatheory) | left fold (`foldl`, Hutton §5.1) | clear |
| the loop head as the least solution of §5.7's equation (`headIter`) | least fixpoint | clear |
| "α-renames", "α-renaming" (§2, §6.7) | α-equivalence; α-variants (PFPL §1.2) | clear |
| "well-formed", "ill-formed" (§5: a program no rule types is ill-formed) | well-formed / ill-formed (PFPL) | clear |
| "derivation" (a typing derivation; GUIDE §1) | derivation (PFPL §2.3) | clear |
| "the **Euclidean** remainder" (01 §6.4: a shift amount is reduced `mod w` to `0 ≤ k < w`; `Int.emod` in `Dynamics.lean`) | the remainder of Euclidean division (Leijen §1.2) | clear |

---

## 10. Program logics, data abstraction and implementation

**Sources:** O'Hearn, Reynolds & Yang 2001; Raza & Gardner 2009; Wirth
1996/2017; Appel 1998 (table of contents only); the Java Language
Specification, Java SE 21, ch. 14; cppreference (*Throwing exceptions*;
`std::vector::push_back`); Maranget 2008; Liskov & Zilles 1974 **(record)**;
MIT 6.005 Readings 12 and 13; Guttag & Horning 1978 **(record)**; Gaudel & Le Gall
2008; Yang & O'Hearn 2002 **(record)**; Leroy & Blazy 2008; Pearce, Kelly & Hankin 2004; Wikipedia's
*Tombstone (programming)*; Lomet 1985 **(record; abstract)**; the Python
Language Reference; the OODBMS Manifesto (Atkinson et al. 1990); Khoshafian &
Copeland 1986 **(record)**; the Swift performance guide; Rust's
`std::borrow::Cow`; the WHATWG Encoding Standard.

### Accepted terms

| Term | Meaning | Source |
|---|---|---|
| frame rule; frame property | The frame rule `{P} C {Q} ⇒ {P ∗ R} C {Q ∗ R}`, provided `C` modifies no variable free in `R`, codifies local behaviour: cells beyond those `P` describes "will remain unaltered". The frame property is the semantic condition behind it: "if the command is safe on some heap, then in any outcome of applying the command on a larger heap, the additional heap portion will remain unchanged by the command" | O'Hearn, Reynolds & Yang §3 (the rule); Raza & Gardner §2 (the property, which they credit to Yang & O'Hearn 2002, their ref. [21], fetched only as a record) |
| activation record; activation frame | The block of storage a procedure call allocates, on a stack, for its local variables (and its return address), released when the procedure terminates | Wirth §12.1 ("The storage blocks are called procedure activation records or activation frames"); Appel ch. 6 "Activation Records" (title seen) |
| normal / abrupt completion; reason | A statement completes normally when all its steps are carried out. `break`, `yield`, `continue`, `return` and `throw` cause a transfer of control that may make it complete abruptly instead, and "an abrupt completion always has an associated reason" (the statement, with its label or value) | JLS §14.1 |
| can complete normally | The static, rule-by-rule judgment that a statement can complete normally, which decides reachability | JLS §14.22 |
| stack unwinding | After a throw, "the control flow works backwards (up the call stack)" to a matching handler; on the way, destructors run for the automatic objects constructed since the `try` block was entered, in reverse order of construction | cppreference, *Throwing exceptions*, "Stack unwinding" |
| match compilation; match compiler | Compiling ML pattern matching to simple tests: a decision tree whose switch nodes test the constructor of one value, with distinct constructors of one signature and a default case only when they do not cover it | Maranget §§1, 3 |
| abstract data type (ADT) | A type known by its operations ("a type is characterized by the operations you can perform on it"), whose representation is hidden, so that the representation can change without changing client code (representation independence) | MIT 6.005 Reading 12 (introduction; "Representation Independence"); Liskov & Zilles 1974 (record; attribution only) |
| representation invariant (rep invariant) | A predicate `RI : R → boolean` on representation values that "tells us whether a given rep value is well-formed"; "invariants must be established by creators and producers, and preserved by observers and mutators" | MIT 6.005 Reading 13 |
| algebraic (axiomatic) specification | Defining an abstract data type "by giving the properties (axioms) of their operations", with "no explicit definition of each operation (no pre- and post-condition, no algorithm)"; the idea dates from the late 1970s | Gaudel & Le Gall §2, crediting Goguen, Thatcher & Wagner and Guttag & Horning 1978 (record) (their refs. [34, 36]) |
| memory block; block reference | CompCert views a memory state "as a collection of separated blocks, identified by block references b"; allocation creates a fresh block, and a location is a block reference and an offset | Leroy & Blazy §3 |
| field-sensitive / field-insensitive / field-based | Of an analysis over aggregates: each instance of a field is modelled separately / the aggregate is one variable / each field name is one variable | Pearce, Kelly & Hankin §2 |
| iterator and reference invalidation | After an operation reallocates a container's storage, "all iterators (including the end() iterator) and all references to the elements are invalidated" | cppreference, `std::vector::push_back` |
| copy-on-write (COW); clone-on-write | A shared value is copied only when it is mutated (Swift's containers); Rust's `Cow` encloses borrowed data and clones it "lazily when mutation or ownership is required" | Swift, *Writing High-Performance Swift Code* ("Use inplace mutation instead of object-reassignment", with its footnote); Rust `std::borrow::Cow` |
| UTF-8 decoder | The algorithm that turns UTF-8 bytes into code points, returning *error* on an invalid sequence, which the "replacement" error mode turns into U+FFFD (in "fatal" mode decoding fails instead) | WHATWG Encoding §8.1.1 (decoder); §4.1 (error modes) |
| tombstone | An intermediary a pointer refers to; when the data is deallocated, the tombstone is set to a value no valid pointer has, so a later use through the stale pointer is detected. More generally, a marker for "this data is no longer here" | Lomet 1985 (record; its abstract names "tombstones for invalidating dangling references"); Wikipedia, *Tombstone (programming)*, which cites Scott, *Programming Language Pragmatics*, p. 392 for the pointer sense and Shaffer, *Data Structures and Algorithm Analysis in C++*, p. 344 for the general sense (neither fetched) |
| object identity | "Every object has an identity, a type and a value. An object's identity never changes once it has been created" | Python Language Reference §3.1; the OODBMS Manifesto, "Object identity" ("an object has an existence which is independent of its value"), which cites Khoshafian & Copeland 1986 (record) |

### Terms we currently use that differ from this

| Our term | Accepted term | Confidence |
|---|---|---|
| "abstract data type" (01 §6.13: `ArrayBuf(T)`, `StrBuf`; formerly "library container") | abstract data type (Liskov & Zilles; MIT 6.005) | clear: the core sees the type only through its methods' equations |
| "trusted code", "trusted-code obligations", "trusted-code bug" (01 §6.13.5; formerly "library obligations", "library bug") | trusted code base (§8 above; Lean Reference, "Validating a Lean Proof"); trusted computing base (§7 above; CakeML §1, §12.1) | clear: the theorems hold conditionally on this code meeting (O1)–(O4) |
| "specification equation" (01 §6.13, the intrinsics of §6.9; formerly "defining equation") | the axioms of an algebraic specification (Guttag & Horning, via Gaudel & Le Gall) | partial: ours give one method's effect on the store, as a function would; algebraic axioms relate the operations to one another |
| "(O2) Representation invariant" (01 §6.13.5; formerly "Boundary invariant") | representation invariant (MIT 6.005) | clear |
| "growth invalidates references" (01 §6.13.3; formerly "growth is identity death") | iterator and reference invalidation (cppreference) | clear: a reallocating growth leaves every view into the old buffer dead |
| "copy-on-write" (01 §6.13.4: a literal-backed `StrBuf` copies its cells into a fresh buffer on its first mutation; formerly "literal promotion") | copy-on-write (Swift); clone-on-write (Rust `Cow`) | clear: Rust's `Cow` is the same shape, borrowed data made owned on the first mutation |
| "UTF-8 decoder methods" (01 §6.13.4; formerly "decode family") | UTF-8 decoder (WHATWG Encoding) | clear |
| "memory block", "block store", `BlockId` (01 §6.1, §6.13; formerly "abstract allocation", "allocation store", `AllocId`) | memory block, block reference (CompCert, Leroy & Blazy) | clear: an identity with cells, not an address, never reused |
| "match-compilation form" (01 §5.5: the core `match` has one arm per variant, in order, with plain bindings; formerly "canonical form only") | match compilation; the switch node of a decision tree (Maranget §3) | partial: ours is one switch on one enum scrutinee, the form elaboration must compile every surface `match` into, and it has no default case |
| "field-sensitive obligations" (the metatheory's leak check; formerly "paths and per-field obligations") | field-sensitive (Pearce, Kelly & Hankin) | partial: the word is from pointer analysis; the idea, per-path obligations, is also RFC 320's drop obligations (§5 above) |
| "unwinding helpers" (01 §6.1; formerly "scope helpers") | stack unwinding (cppreference) | partial: ours close drop scopes on every scope exit, not only on an exception; the `return`/`break` exits are the unwinding case |
| "object identity" (the metatheory, `Dynamics.lean`: the tag an aggregate value carries from its creation; formerly "value identity") | object identity (Python Language Reference; the OODBMS Manifesto, after Khoshafian & Copeland) | clear for aggregates; scalars carry none |
| "frame property", `FrameProperty` (the metatheory, `Soundness/Defs.lean`: an evaluation grows the store and changes no cell outside its activation's own; formerly "locality", `Untouched`) | frame property (Yang & O'Hearn, via Raza & Gardner) | partial: the same guarantee, that state outside what the code may touch is unchanged, but ours is a relation between one evaluation's stores, not a condition on a command run on every larger heap |
| "abrupt-completion context", "completes abruptly with" (01 §5.3: `Ω`'s `Δ`, each exit kind with its ownership state; formerly "edge delivery") | abrupt completion and its reason (JLS §14.1) | partial: ours is static, records the ownership state with each reason, and counts a loop that never exits among them |
| "can complete normally" (01 §5.3, §5.5; formerly "continuing"); "can complete abruptly", `Expr.canCompleteAbruptly` (formerly "unwinds", `Expr.unwinds`) | can complete normally (JLS §14.22); abrupt completion (JLS §14.1) | clear for "can complete normally"; the abrupt predicate has no JLS name of its own |
| "activation record" (01 §6.1's `φ = ⟨ρ ; σ⟩`, `Activation`; formerly "frame") | activation record (Wirth §12.1; Appel ch. 6) | clear: one call's environment and locals; ours also carries its drop scopes. "Frame" now means only a control-stack or evaluation-context frame (§1), as in PFPL |
| "tombstone", "tombstoned", `†` (01 §6.1, §6.13; `Tombstoned`; formerly "retire", "retired") | tombstone (Lomet 1985; Wikipedia) | partial: the sources' tombstone is a separate cell a pointer goes through; ours marks the cell itself (Wikipedia's general sense). The cell stays in the store marked `†` and is never reused, so a later use is refused. `drop-retire` / `dropRetire` keep the old verb (the D179 override), so "dropRetire tombstones the cell" mixes the two |

---

## Sources

Fetched means the page title was seen and recorded. **(record)** means the DOI
or Crossref record was fetched but the full text was not; such a source is
cited for its existence only, and any content attributed to it comes through
the secondary source named alongside.

| Short cite | Citation | Link | Fetched |
|---|---|---|---|
| Alpern & Schneider 1985 | B. Alpern, F. B. Schneider. Defining Liveness. *Inf. Process. Lett.* 21(4):181–185, 1985 | https://doi.org/10.1016/0020-0190(85)90056-0 | "Defining liveness" (Crossref); Cornell PDF "DEFINING LIVENESS" |
| Amin & Rompf 2017 | N. Amin, T. Rompf. Type Soundness Proofs with Definitional Interpreters. POPL 2017 | https://doi.org/10.1145/3009837.3009866 | Semantic Scholar record; the paper PDF |
| Appel & McAllester 2001 | A. W. Appel, D. McAllester. An indexed model of recursive types for foundational proof-carrying code. *TOPLAS* 23(5):657–683, 2001 | https://doi.org/10.1145/504709.504712 | OpenAlex record; full text (cs.princeton.edu/~appel/papers/indexed.pdf) |
| Appel 1998 | A. W. Appel. *Modern Compiler Implementation in ML*. Cambridge University Press, 1998 | https://www.cs.princeton.edu/~appel/modern/toc.html | "Modern Compiler Implementation, Table of Contents" (chapter titles only; text not read) |
| Atkey 2018 | R. Atkey. Syntax and Semantics of Quantitative Type Theory. LICS 2018, 56–65 | https://doi.org/10.1145/3209108.3209189 | Crossref; PDF at bentnib.org |
| Atkinson et al. 1990 | M. Atkinson, F. Bancilhon, D. DeWitt, K. Dittrich, D. Maier, S. Zdonik. The Object-Oriented Database System Manifesto. In *Deductive and Object-Oriented Databases* (DOOD '89), Elsevier, 1990, 223–240 | https://doi.org/10.1016/B978-0-444-88433-6.50020-4 | Crossref; the CMU HTML edition (www.cs.cmu.edu/~clamen/OODBMS/Manifesto/htManifesto/node4.html), "Object identity" |
| Barendregt & Wiedijk 2005 | H. Barendregt, F. Wiedijk. The Challenge of Computer Mathematics. *Phil. Trans. R. Soc. A* 363(1835):2351–2375, 2005 | https://doi.org/10.1098/rsta.2005.1650 | Crossref; the authors' preprint in the Radboud repository (https://hdl.handle.net/2066/32307) |
| Barr et al. 2015 | E. T. Barr, M. Harman, P. McMinn, M. Shahbaz, S. Yoo. The Oracle Problem in Software Testing: A Survey. *IEEE TSE* 41(5):507–525, 2015 | https://doi.org/10.1109/TSE.2014.2372785 | Crossref; PDF of the same title |
| Beer et al. 2001 | I. Beer, S. Ben-David, C. Eisner, Y. Rodeh. Efficient Detection of Vacuity in Temporal Model Checking. *Formal Methods in System Design* 18(2):141–163, 2001 | https://doi.org/10.1023/A:1008779610539 | Crossref; the article PDF (cs.toronto.edu/~chechik/courses05/csc2108/beer01.pdf) |
| Bernardy et al. 2018 | J.-P. Bernardy, M. Boespflug, R. R. Newton, S. Peyton Jones, A. Spiwack. Linear Haskell: practical linearity in a higher-order polymorphic language. *PACMPL* 2(POPL), 2018 | https://arxiv.org/abs/1710.09756 | "Linear Haskell: practical linearity in a higher-order polymorphic language" |
| Boute 1992 | R. T. Boute. The Euclidean definition of the functions div and mod. *ACM TOPLAS* 14(2):127–144, 1992 **(record)** | https://doi.org/10.1145/128861.128862 | Crossref record (the ACM PDF returned 403); content cited through Leijen 2001 |
| Cedar 2024 | C. Disselkoen et al. How We Built Cedar: A Verification-Guided Approach. FSE Companion '24, 351–357 (pages per Crossref), 2024 (doi:10.1145/3663529.3663854) | https://arxiv.org/abs/2407.01688 | "How We Built Cedar: A Verification-Guided Approach"; the arXiv PDF's first page gives the FSE Companion venue and DOI |
| Charguéraud 2013 | A. Charguéraud. Pretty-Big-Step Semantics. ESOP 2013, *Programming Languages and Systems*, LNCS, 41–60 | https://doi.org/10.1007/978-3-642-37036-6_3 | Crossref; author PDF (chargueraud.org/research/2012/pretty/pretty.pdf) |
| Chen et al. 2020 | J. Chen, J. Patra, M. Pradel, Y. Xiong, H. Zhang, D. Hao, L. Zhang. A Survey of Compiler Testing. *ACM Comput. Surv.* 53(1), Art. 4, 2020 | https://doi.org/10.1145/3363562 | Crossref; 2019 preprint PDF of the same title (section numbers here are the preprint's) |
| Claessen & Hughes 2000 | K. Claessen, J. Hughes. QuickCheck: A Lightweight Tool for Random Testing of Haskell Programs. ICFP 2000, 268–279 | https://doi.org/10.1145/351240.351266 | Crossref; PDF of the same title |
| Clarkson & Schneider 2010 | M. R. Clarkson, F. B. Schneider. Hyperproperties. *J. Comput. Secur.* 18(6):1157–1210, 2010 | https://doi.org/10.3233/JCS-2009-0393 | "Hyperproperties" (Crossref; Cornell PDF) |
| Confluent | Kafka Message Delivery Guarantees (Confluent documentation) | https://docs.confluent.io/kafka/design/delivery-semantics.html | "Kafka Message Delivery Guarantees" |
| C11 | ISO/IEC 9899:201x, draft N1570, §5.1.2.3, §6.8 | https://port70.net/~nsz/c/c11/n1570.html | "N1570 … ISO/IEC 9899:201x" |
| CakeML 2014 | R. Kumar, M. O. Myreen, M. Norrish, S. Owens. CakeML: A Verified Implementation of ML. POPL 2014, 179–191 | https://doi.org/10.1145/2535838.2535841 | Crossref; cakeml.org/popl14.pdf |
| C-Reduce 2012 | J. Regehr, Y. Chen, P. Cuoq, E. Eide, C. Ellison, X. Yang. Test-Case Reduction for C Compiler Bugs. PLDI 2012, 335–346 | https://doi.org/10.1145/2254064.2254104 | Crossref; preprint of the same title |
| cppreference | cppreference.com: *Throwing exceptions* ("Stack unwinding"); `std::vector<T,Allocator>::push_back` | https://en.cppreference.com/w/cpp/language/throw; https://en.cppreference.com/w/cpp/container/vector/push_back | "Throwing exceptions - cppreference.com"; "std::vector<T,Allocator>::push_back - cppreference.com" |
| Csmith 2011 | X. Yang, Y. Chen, E. Eide, J. Regehr. Finding and Understanding Bugs in C Compilers. PLDI 2011, 283–294 | https://doi.org/10.1145/1993498.1993532 | Crossref; preprint of the same title |
| CWE | MITRE. Common Weakness Enumeration 4.20: CWE-416 Use After Free; CWE-415 Double Free; CWE-401 Missing Release of Memory after Effective Lifetime | https://cwe.mitre.org/data/definitions/416.html (and 415.html, 401.html) | "CWE-416: Use After Free (4.20)"; "CWE-415: Double Free (4.20)"; "CWE-401: Missing Release of Memory after Effective Lifetime (4.20)" |
| DeMillo et al. 1978 | R. A. DeMillo, R. J. Lipton, F. G. Sayward. Hints on Test Data Selection: Help for the Practicing Programmer. *Computer* 11(4):34–41, 1978 | https://doi.org/10.1109/C-M.1978.218136 | Crossref; a scan of the IEEE reprint (st.cs.uni-saarland.de/edu/recommendation-systems/papers/Hints_on_Test_Data_Selection-1.pdf), read as page images |
| Dreyer et al. 2019 | D. Dreyer, A. Timany, R. Krebbers, L. Birkedal, R. Jung. What Type Soundness Theorem Do You Really Want to Prove? SIGPLAN Blog, 2019-10-17 | https://blog.sigplan.org/2019/10/17/what-type-soundness-theorem-do-you-really-want-to-prove/ | same title |
| Felleisen & Hieb 1992 | M. Felleisen, R. Hieb. The revised report on the syntactic theories of sequential control and state. *Theor. Comput. Sci.* 103(2):235–271, 1992 | https://doi.org/10.1016/0304-3975(92)90014-7 | OpenAlex record; the Rice TR 100-89 preprint of the same title (numbering here is the preprint's) |
| Gaudel & Le Gall 2008 | M.-C. Gaudel, P. Le Gall. Testing Data Types Implementations from Algebraic Specifications. In *Formal Methods and Testing*, Springer, 2008, 209–239 (per arXiv) | https://arxiv.org/abs/0804.0970 | "Testing Data Types Implementations from Algebraic Specifications" (arXiv PDF) |
| Guttag & Horning 1978 | J. V. Guttag, J. J. Horning. The algebraic specification of abstract data types. *Acta Informatica* 10(1):27–52, 1978 **(record)** | https://doi.org/10.1007/BF00260922 | Crossref record (the Springer PDF is paywalled); content cited through Gaudel & Le Gall 2008, which credits it together with Goguen, Thatcher & Wagner |
| Harper 2016 (PFPL) | R. Harper. *Practical Foundations for Programming Languages*, 2nd ed. Cambridge University Press, 2016 | https://doi.org/10.1017/CBO9781316576892 | CUP page; cs.cmu.edu/~rwh/pfpl and its abbreviated PDF |
| Hicks 2014 | M. Hicks. What is memory safety? *The PL Enthusiast* (blog), 2014-07-21 | http://www.pl-enthusiast.net/2014/07/21/memory-safety/ (the site is gone; fetched from https://web.archive.org/web/20260831075253/http://www.pl-enthusiast.net/2014/07/21/memory-safety/) | "What is memory safety? - The PL Enthusiast" (Wayback snapshot) |
| Hutton 1999 | G. Hutton. A tutorial on the universality and expressiveness of fold. *J. Funct. Program.* 9(4):355–372, 1999 | https://doi.org/10.1017/S0956796899003500 | Crossref; people.cs.nott.ac.uk/pszgmh/fold.pdf |
| Jia & Harman 2011 | Y. Jia, M. Harman. An Analysis and Survey of the Development of Mutation Testing. *IEEE TSE* 37(5):649–678, 2011 | https://doi.org/10.1109/TSE.2010.62 | Crossref; author preprint of the same title (www0.cs.ucl.ac.uk/staff/mharman/tse-mutation-survey.pdf) |
| JLS | J. Gosling et al. *The Java Language Specification*, Java SE 21 Edition, ch. 14 "Blocks, Statements, and Patterns" | https://docs.oracle.com/javase/specs/jls/se21/html/jls-14.html | "Chapter 14. Blocks, Statements, and Patterns" |
| Khoshafian & Copeland 1986 | S. N. Khoshafian, G. P. Copeland. Object identity. OOPSLA '86, 406–416 **(record)** | https://doi.org/10.1145/28697.28739 | Crossref record (the ACM page returned 403); the term is cited through the Python Language Reference and the OODBMS Manifesto |
| Lamport 1977 | L. Lamport. Proving the Correctness of Multiprocess Programs. *IEEE TSE* SE-3(2):125–143, 1977 | https://doi.org/10.1109/TSE.1977.229904 | Crossref; "The Writings of Leslie Lamport" (which says this paper introduced "safety" and "liveness") |
| Lean Reference | *The Lean Language Reference* | https://lean-lang.org/doc/reference/latest/ | "The Lean Language Reference" and its chapters 2, 4 (with §4.4 "Inductive Types"), 5 "Source Files and Modules", 7.4, 7.6 "Recursive Definitions", 8, "Validating a Lean Proof" |
| Lean 4.29.0 | Lean 4.29.0 release notes | https://lean-lang.org/doc/reference/latest/releases/v4.29.0/ | "Lean 4.29.0 (2026-03-27)" |
| Lean API | `Lean.ReducibilityAttrs`, `Init.Tactics` | https://lean-lang.org/doc/api/Lean/ReducibilityAttrs.html | "Lean.ReducibilityAttrs"; "Init.Tactics" |
| Leijen 2001 | D. Leijen. Division and Modulus for Computer Scientists. University of Utrecht, 2001-12-03 | https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/divmodnote-letter.pdf | "Division and Modulus for Computer Scientists" |
| Leroy & Blazy 2008 | X. Leroy, S. Blazy. Formal verification of a C-like memory model and its uses for verifying program transformations. *J. Autom. Reasoning* 41(1):1–31, 2008 | https://doi.org/10.1007/s10817-008-9099-0 | Crossref; xavierleroy.org/publi/memory-model-journal.pdf |
| Leroy 2009a (CACM) | X. Leroy. Formal verification of a realistic compiler. *CACM* 52(7):107–115, 2009 | https://doi.org/10.1145/1538788.1538814 | Crossref; xavierleroy.org PDF |
| Leroy 2009b (JAR) | X. Leroy. A formally verified compiler back-end. *J. Autom. Reasoning* 43(4):363–446, 2009 | https://arxiv.org/abs/0902.2137 | "A formally verified compiler back-end" |
| Leroy & Grall 2009 | X. Leroy, H. Grall. Coinductive big-step operational semantics. *Inf. Comput.* 207(2):284–304, 2009 | https://doi.org/10.1016/j.ic.2007.12.004 (arXiv:0808.0586) | "Coinductive big-step operational semantics" (arXiv) |
| Leucker & Schallhart 2009 | M. Leucker, C. Schallhart. A brief account of runtime verification. *J. Log. Algebr. Program.* 78(5):293–303, 2009 | https://doi.org/10.1016/j.jlap.2008.08.004 | Crossref; Lübeck PDF "A Brief Account of Runtime Verification" |
| libFuzzer | libFuzzer documentation | https://llvm.org/docs/LibFuzzer.html | "libFuzzer – a library for coverage-guided fuzz testing" |
| Liskov & Zilles 1974 | B. Liskov, S. Zilles. Programming with abstract data types. *ACM SIGPLAN Notices* 9(4):50–59, 1974 **(record)** | https://doi.org/10.1145/942572.807045 | Crossref record; attribution only, content cited through MIT 6.005 Reading 12 |
| Lomet 1985 | D. B. Lomet. Making Pointers Safe in System Programming Languages. *IEEE Trans. Softw. Eng.* SE-11(1):87–96, 1985 **(record; abstract)** | https://doi.org/10.1109/TSE.1985.231846 | Crossref record; the abstract via OpenAlex (the IEEE PDF is paywalled) |
| Maranget 2008 | L. Maranget. Compiling pattern matching to good decision trees. ML Workshop 2008, 35–46 | https://doi.org/10.1145/1411304.1411311 | Crossref; the author's PDF (pauillac.inria.fr/~maranget/papers/ml05e-maranget.pdf) |
| McBride 2016 | C. McBride. I Got Plenty o' Nuttin'. In *A List of Successes That Can Change the World*, LNCS 9600, 207–233, 2016 | https://doi.org/10.1007/978-3-319-30936-1_12 | "I got plenty o' nuttin'" (Strathclyde); preprint |
| McKeeman 1998 | W. M. McKeeman. Differential Testing for Software. *Digital Technical Journal* 10(1):100–107, 1998 | https://www.cs.tufts.edu/~nr/cs257/archive/bill-mckeeman/DifferentailTesting.pdf (no DOI found) | "Differential Testing for Software" |
| Milner 1978 | R. Milner. A theory of type polymorphism in programming. *J. Comput. Syst. Sci.* 17(3):348–375, 1978 | https://doi.org/10.1016/0022-0000(78)90014-4 | Edinburgh Research Explorer abstract page; OpenAlex; full text (homepages.inf.ed.ac.uk/wadler/papers/papers-we-love/milner-type-polymorphism.pdf) |
| MIT 6.005 | MIT 6.005 *Software Construction*, Fall 2015, Reading 12: Abstract Data Types; Reading 13: Abstraction Functions & Rep Invariants | https://web.mit.edu/6.005/www/fa15/classes/12-abstract-data-types/; https://web.mit.edu/6.005/www/fa15/classes/13-abstraction-functions-rep-invariants/ | "Reading 12: Abstract Data Types"; "Reading 13: Abstraction Functions & Rep Invariants" |
| Necula 2000 | G. C. Necula. Translation Validation for an Optimizing Compiler. PLDI 2000, 83–94 | https://doi.org/10.1145/349299.349314 | Crossref; Berkeley PDF |
| Nipkow & Klein | T. Nipkow, G. Klein. *Concrete Semantics*; Isabelle HOL-IMP theory `Small_Step` | http://concrete-semantics.org/ and https://isabelle.in.tum.de/library/HOL/HOL-IMP/Small_Step.html | "Concrete Semantics"; "Small-Step Semantics of Commands" |
| Niu, Sterling & Harper 2024 | Y. Niu, J. Sterling, R. Harper. Cost-sensitive computational adequacy of higher-order recursion in synthetic domain theory. MFPS 2024 (ENTICS 4) | https://arxiv.org/abs/2404.00212 | arXiv abstract page; full text |
| O'Hearn, Reynolds & Yang 2001 | P. O'Hearn, J. Reynolds, H. Yang. Local Reasoning about Programs that Alter Data Structures. CSL 2001, LNCS 2142, 1–19 | https://doi.org/10.1007/3-540-44802-0_1 | Crossref; the UCL PDF (www0.cs.ucl.ac.uk/staff/p.ohearn/papers/localreasoning.pdf) |
| Owens et al. 2016 | S. Owens, M. O. Myreen, R. Kumar, Y. K. Tan. Functional Big-Step Semantics. ESOP 2016, LNCS 9632, 589–615 | https://doi.org/10.1007/978-3-662-49498-1_23 | Crossref; "Functional Big-step Semantics" (cl.cam.ac.uk PDF) |
| Oxide | A. Weiss, O. Gierczak, D. Patterson, A. Ahmed. Oxide: The Essence of Rust. arXiv:1903.00982; theorem and lemma numbers cited here are v4's (19 Oct 2021), and v1 (3 Mar 2019) numbers them differently | https://arxiv.org/abs/1903.00982 | "Oxide: The Essence of Rust" |
| Pearce, Kelly & Hankin 2004 | D. J. Pearce, P. H. J. Kelly, C. Hankin. Efficient field-sensitive pointer analysis for C. PASTE 2004, 37–42 | https://doi.org/10.1145/996821.996835 | Crossref; the Imperial College PDF of the same title |
| Pierce 2002 (TAPL) | B. C. Pierce. *Types and Programming Languages*. MIT Press, 2002 | https://www.cis.upenn.edu/~bcpierce/tapl/ | "Types and Programming Languages", with its table of contents and errata (text not read) |
| Plotkin 1977 | G. D. Plotkin. LCF considered as a programming language. *Theor. Comput. Sci.* 5(3):223–255, 1977 **(record)** | https://doi.org/10.1016/0304-3975(77)90044-5 | Crossref record |
| Plotkin 1981/2004 | G. D. Plotkin. A Structural Approach to Operational Semantics. DAIMI FN-19, Aarhus, 1981; *J. Log. Algebr. Program.* 60–61:17–139, 2004 | https://doi.org/10.1016/j.jlap.2004.05.001 | Crossref/OpenAlex record; the author's own 2004 edition (homepages.inf.ed.ac.uk/gdp/publications/sos_jlap.pdf), whose numbering is used here |
| Plotkin 2004b | G. D. Plotkin. The Origins of Structural Operational Semantics. *J. Log. Algebr. Program.* 60–61:3–15, 2004 | https://doi.org/10.1016/j.jlap.2004.03.009 | "The Origins of Structural Operational Semantics" (author PDF) |
| Pnueli et al. 1998 | A. Pnueli, M. Siegel, E. Singerman. Translation Validation. TACAS 1998, LNCS 1384, 151–166 | https://doi.org/10.1007/BFb0054170 | Crossref; Weizmann research-portal page |
| Polonius | The Polonius book, "Atoms" | https://rust-lang.github.io/polonius/rules/atoms.html | "Atoms - Polonius" |
| Python Reference | *The Python Language Reference*, §3.1 "Objects, values and types" | https://docs.python.org/3/reference/datamodel.html | "3. Data model" |
| Raza & Gardner 2009 | M. Raza, P. Gardner. Footprints in Local Reasoning. *Log. Methods Comput. Sci.* 5(2:4), 2009 | https://doi.org/10.2168/LMCS-5(2:4)2009 | Crossref; arXiv:0903.1032 PDF |
| Reynolds 1972 | J. C. Reynolds. Definitional interpreters for higher-order programming languages. ACM '72, 717–740; reprinted *Higher-Order Symb. Comput.* 11(4):363–397, 1998 **(record)** | https://doi.org/10.1023/A:1010027404223 | Crossref records for both |
| RFC 2119 | S. Bradner. Key words for use in RFCs to Indicate Requirement Levels. RFC 2119 (BCP 14), 1997 | https://www.rfc-editor.org/rfc/rfc2119.txt | "Key words for use in RFCs to Indicate Requirement Levels" |
| RFC 320 | Rust RFC 320, Non-zeroing dynamic drop (start date 2014-09-24) | https://github.com/rust-lang/rfcs/blob/master/text/0320-nonzeroing-dynamic-drop.md | "0320-nonzeroing-dynamic-drop.md" (its "Drop obligations" section) |
| Rust Book | *The Rust Programming Language*, §4.1 "What is Ownership?" | https://doc.rust-lang.org/book/ch04-01-what-is-ownership.html | "What is Ownership? - The Rust Programming Language" |
| Rust Reference | *The Rust Reference*: Destructors; Expressions; Glossary; Patterns (and the whole-book `print.html`, searched for "affine") | https://doc.rust-lang.org/reference/destructors.html | "Destructors - The Rust Reference" (and "Expressions", "Glossary", "Patterns") |
| Rust std `Cow` | *The Rust Standard Library*, `std::borrow::Cow` | https://doc.rust-lang.org/std/borrow/enum.Cow.html | "Cow in std::borrow - Rust" |
| Rustonomicon | *The Rustonomicon*: Drop Flags; Destructors | https://doc.rust-lang.org/nomicon/drop-flags.html | "Drop Flags - The Rustonomicon" |
| rustc-dev-guide | *Rust Compiler Development Guide*: Move paths; Drop elaboration | https://rustc-dev-guide.rust-lang.org/borrow-check/moves-and-initialization/move-paths.html | "Move paths - Rust Compiler Development Guide"; "Drop elaboration - …" |
| RustBelt 2018 | R. Jung, J.-H. Jourdan, R. Krebbers, D. Dreyer. RustBelt: Securing the Foundations of the Rust Programming Language. *PACMPL* 2(POPL), Art. 66, 2018 | https://doi.org/10.1145/3158154 | Crossref; plv.mpi-sws.org/rustbelt/popl18; the paper PDF |
| Schneider 2000 | F. B. Schneider. Enforceable Security Policies. *ACM TISSEC* 3(1):30–50, 2000 | https://doi.org/10.1145/353323.353382 | Crossref; Cornell PDF |
| Siek 2013 | J. Siek. Type Safety in Three Easy Lemmas. Blog post, 2013-05-27 | https://siek.blogspot.com/2013/05/type-safety-in-three-easy-lemmas.html | "Jeremy Siek: Type Safety in Three Easy Lemmas" |
| Software Foundations | B. C. Pierce et al. *Software Foundations*, vol. 1 (`ImpCEvalFun`) and vol. 2 (`Smallstep`, `StlcProp`) | https://softwarefoundations.cis.upenn.edu/plf-current/Smallstep.html | "Smallstep: Small-step Operational Semantics"; "StlcProp"; "ImpCEvalFun" |
| Swift performance guide | *Writing High-Performance Swift Code* (`docs/OptimizationTips.rst` in the Swift repository), "Advice: Use copy-on-write semantics for large values" and "Advice: Use inplace mutation instead of object-reassignment" | https://github.com/swiftlang/swift/blob/main/docs/OptimizationTips.rst | "Writing High-Performance Swift Code" |
| Swift SE-0176 | J. McCall. SE-0176: Enforce Exclusive Access to Memory. Swift Evolution proposal, implemented in Swift 4.0 | https://github.com/swiftlang/swift-evolution/blob/main/proposals/0176-enforce-exclusive-access-to-memory.md | "Enforce Exclusive Access to Memory" |
| Tarski 1955 | A. Tarski. A lattice-theoretical fixpoint theorem and its applications. *Pacific J. Math.* 5(2):285–309, 1955 | https://doi.org/10.2140/pjm.1955.5.285 | Crossref; the MSP PDF (msp.org/pjm/1955/5-2/pjm-v5-n2-p11-s.pdf) |
| Timany et al. 2024 | A. Timany, R. Krebbers, D. Dreyer, L. Birkedal. A Logical Approach to Type Soundness. *J. ACM* 71(6), Art. 40, 2024 | https://doi.org/10.1145/3676954 | "A Logical Approach to Type Soundness" (author page; iris-project.org PDF) |
| TPIL | J. Avigad, L. de Moura, S. Kong, S. Ullrich. *Theorem Proving in Lean 4* | https://lean-lang.org/theorem_proving_in_lean4/ | "Theorem Proving in Lean 4" and its chapters |
| Tov & Pucella 2011 | J. A. Tov, R. Pucella. Practical affine types. POPL 2011, 447–458 | https://doi.org/10.1145/1926385.1926436 | Crossref; long-version PDF "Practical Affine Types" |
| Walker 2005 | D. Walker. Substructural Type Systems. In B. C. Pierce (ed.), *Advanced Topics in Types and Programming Languages*, ch. 1, 3–44. MIT Press | https://doi.org/10.7551/mitpress/1104.003.0003 | Crossref (which dates the chapter 2004); MIT Press sample PDF "1 Substructural Type Systems" |
| Wadler 1992 | P. Wadler. Monads for Functional Programming. In J. Jeuring, E. Meijer (eds.), *Advanced Functional Programming*, LNCS 925, 24–52. Springer, 1995 (notes appear 1992) | https://homepages.inf.ed.ac.uk/wadler/papers/marktoberdorf/baastad.pdf | the author's PDF, "Monads for functional programming" |
| WHATWG Encoding | *Encoding Standard* (WHATWG Living Standard), §8.1.1 "UTF-8 decoder" | https://encoding.spec.whatwg.org/ | "Encoding Standard" |
| Wikipedia *Tombstone* | Wikipedia, *Tombstone (programming)* | https://en.wikipedia.org/wiki/Tombstone_(programming) | "Tombstone (programming)" (its source text) |
| Wirth 1996/2017 | N. Wirth. *Compiler Construction*. Addison-Wesley, 1996; slightly revised edition, Zürich, May 2017 | https://people.inf.ethz.ch/wirth/CompilerConstruction/ | "Compiler Construction" (the author's PDFs; §12.1 read) |
| Wright & Felleisen 1994 | A. K. Wright, M. Felleisen. A Syntactic Approach to Type Soundness. *Inf. Comput.* 115(1):38–94, 1994 | https://doi.org/10.1006/inco.1994.1093 | OpenAlex record; the Rice TR91-160 preprint of the same title (numbering here is the preprint's) |
| Yang & O'Hearn 2002 | H. Yang, P. W. O'Hearn. A Semantic Basis for Local Reasoning. FoSSaCS 2002, LNCS 2303, 402–416 **(record)** | https://doi.org/10.1007/3-540-45931-6_28 | Crossref record (Springer returned a bot challenge); content cited through Raza & Gardner 2009 §2.1 |
