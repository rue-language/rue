# Field map: the literature the formal core belongs to

This file grounds the formal core's terminology in the programming-languages
literature. The calculus ([01-core-calculus.md](01-core-calculus.md)), the
metatheory ([03-metatheory.md](03-metatheory.md)) and the mechanization
([lean/](lean/README.md)) draw on eight subfields. For each one, this file
records:

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
| reduction semantics | A semantics given as a rewriting relation on terms, the style of Wright & Felleisen's syntactic approach | Amin & Rompf §2 |
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
| `EvalRes.stuck w`: "stuck" also covers the interpreter's four monitors (GUIDE §2) | stuck = no rule applies. On statically invalid input, a monitored case is a step §6 does take | partial |
| "hole" for the `⊘` cell content (`Contents.hole`; GUIDE §§2–3; 01 §5.1 and §6.11; the metatheory's "No use-after-move" section) | "hole" is the empty position of a context. The accepted words for a moved-out cell are in §5 below | partial: a second meaning of an accepted term, and both meanings appear in GUIDE §2 |
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
| syntactic type soundness | Soundness proved by induction over the syntax of typing. Wright & Felleisen prove it by subject reduction plus uniform evaluation (faulty expressions are untypable); the now-standard proof uses progress and preservation instead | Wright & Felleisen Thm 4.12; Timany §2.5; PFPL §6.4 |
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
| `Γ ⊢ e : τ` | "under Γ, e has type τ" | Wright & Felleisen `Γ ▷ e : τ`; SF `Gamma \|-- t \in T`; PFPL writes `e : τ` for closed terms |
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
| Semantic type soundness | `∅ ⊢ e : τ ⇒ safe(e)`, obtained from the fundamental theorem and adequacy | Timany Cor. 6.7 |

Timany et al. argue (§3) that progress and preservation are too weak rather
than false. They cover only syntactically well-typed code, say nothing about
data abstraction, and put `unsafe` code behind a safe API out of scope.

### Terms we currently use that differ from this

| Our term | Accepted term | Confidence |
|---|---|---|
| "type safety (progress + preservation)" (§7, metatheory) | type safety = progress ∧ preservation | clear |
| `step_progress`: every configuration reachable from a checked program's start steps or has halted | progress is stated per well-typed term. The reachability form is Timany's `safe(e)` | partial |
| `step_preservation` over `Config.SafeAt` | Preservation is syntactic (`⊢ e′ : τ`). A statement over "nothing reachable is stuck, and every halted value is typed" is a semantic statement in Timany's safety form. Our own docs already say this: the `Config.SafeAt` docstring ("Which preservation", `Adequacy.lean`), GUIDE §2 and the metatheory's type-safety section call the configuration typing semantic and claim no syntactic `⊢ C : T` | partial: the name clashes with the accepted syntactic meaning, and our docs disclose the clash |
| `Config.SafeAt` | `safe(e)` (Timany §2.4) plus value typing. Timany's adequacy (Thm 6.6) concludes `safe(e)` only; our second conjunct is the syntactic value typing `HasTy` | partial |
| "fundamental lemma" `init_safeAt` (metatheory): a checked program's initial configuration is `SafeAt` | Timany's fundamental theorem is `⊢ ⇒ ⊨`; `⊢ ⇒ safe` is his Cor. 6.7, which he calls semantic type soundness. Our proof has no `⊨` and no logical relation: it is syntactic soundness of `eval` (a `Matches` invariant) followed by the `eval`/`Step` agreement of §3. "The fundamental theorem composed with adequacy" is only an analogy | partial |
| `FrameMatches` / `Matches` ("Σ faithfully tracks the store's initialization") | the invariant a syntactic proof carries: store typing / a well-typed machine state. Ours checks each cell against both its ownership state and its type (`ContentsMatches`; "an owned node holds a hole-free well-typed contents"), and `Soundness.lean` calls `Matches` "the §7 preservation invariant" | partial: store typing extended with ownership state |
| Σ for the ownership state (§5) | Σ is store typing in TAPL and the global environment in Oxide | partial: symbol clash |
| `Violation` (`useAfterMove`, `useAfterDrop`, …) | `wrong` (Milner), "going wrong" (CompCert), stuck (PFPL) | partial: ours is a named refusal, and four of its eight constructors are the monitors of §6 below, which are not stuck states of §6's `Step` |
| `soundness`, `run_safe` over `eval` | type soundness via a definitional interpreter (§3 below) | clear |

---

## 3. Big-step, small-step and definitional interpreters; fuel and clocks

**Sources:** Plotkin 1977; Reynolds 1972 (HOSC reprint 1998); Leroy & Grall
2009; Owens, Myreen, Kumar & Tan 2016; Amin & Rompf 2017; Siek 2013;
Nipkow & Klein, *Concrete Semantics*, with its Isabelle theory `Small_Step`;
*Software Foundations* (`Smallstep`, `ImpCEvalFun`); Niu, Sterling & Harper
2024 (as a secondary source for Plotkin 1977); Charguéraud 2013. Plotkin 1977
and Reynolds 1972 are cited for existence only.

### Accepted terms

| Term | Meaning | Source |
|---|---|---|
| natural semantics | The name Kahn and his coworkers gave big-step operational semantics | Plotkin 2004b |
| big-step / small-step semantics | An evaluation relation to a final result / a one-step relation and its closure | Leroy & Grall §§3–4; Concrete Semantics §§7.2–7.3 |
| definitional interpreter | An interpreter that serves as the definition of a language | Amin & Rompf §2.1 ("in the style of Reynolds") |
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
| "adequacy", "the adequacy lemma", `Adequacy.lean` (`eval` ⇔ `Step*`) | equivalence of big-step (definitional-interpreter) and small-step semantics; "semantic equivalence" (Amin & Rompf). Charguéraud's "adequate with respect to" is the same kind of relation | partial: the word is used in this sense (Charguéraud), but more often for the two relations in the list above; model adequacy is the sense in this map's §2, while our docs use "adequacy" only for `eval` ⇔ `Step`. Ours also holds only on checked programs: on other input only the one-directional `run_sim` holds (`Adequacy.lean`), whereas Leroy & Grall's and Amin & Rompf's theorems are unconditional |
| `eval_sound` / `eval_complete` | the two directions of the equivalence (`big_to_small` / `small_to_big` in Isabelle HOL-IMP; SF's `eval__multistep` / `multistep__eval` relate a *relational* big-step semantics to multistep) | clear. Note that "sound" here names a simulation direction, next to the type-soundness theorem `soundness` in the same package |
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
| context split | `Γ = Γ₁ ∘ Γ₂`, which distributes linear assumptions between subterms | Walker Fig. 1-4 |
| dereliction subtyping | An unlimited-use function may be used where a one-use function is expected, after linear logic's dereliction rule | Tov & Pucella |
| multiplicity | An arrow or binder annotation: 1, ω, a variable, or a sum or product of these. Multiplicities form a semiring without a zero | Linear Haskell Fig. 5 |
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
| "class" `class(T) ∈ {Copy, Affine, Linear}` (§3; `Mult`) | qualifier (Walker), usage qualifier (Tov & Pucella) | partial: same role, different word |
| `Copy` | unrestricted (`un`, Walker), unlimited (`U`, Tov & Pucella). "Copy" is Rust's trait name | partial |
| `Affine`, `Linear` with §3's rule glosses | affine, linear; §3 names contraction and weakening correctly | clear |
| "multiplicity lattice" (§3), `Mult` | a qualifier order (Walker, Tov & Pucella). "Multiplicity" (Linear Haskell) means an arrow annotation in a semiring generated by 1 and ω | partial: the word comes from a neighboring framework with a different structure |
| `Copy ⊑ Affine ⊑ Linear` ("more restrictive is higher") | Tov & Pucella's direction (`U ⊑ A`). The reverse of Walker's (`lin ⊑ un`) | clear, once the direction is stated |
| "infectious" (a struct takes its fields' join) | Walker's containment rules (a container is at least as restrictive as what it contains: an `un` pair cannot hold a `lin` component); Tov & Pucella give a product the join `⊔` of its components' qualifiers | partial: the same constraint, with no single-word name |
| `Typed … Ω`, the "outgoing state" | Walker's algorithmic `Γ₁ ⊢ t : T; Γ₂`; Oxide's output context `⇒ Γ′` | partial |
| `check_sound` | algorithmic soundness (Walker 1.2.9) | clear |

---

## 5. Ownership, borrowing, moves and drop

**Sources:** RustBelt (Jung et al. 2018); Oxide (Weiss et al. 2019/2021); the
Rust Reference (*Destructors*, *Expressions*, *Glossary*, *Patterns*); the
Rustonomicon (*Drop Flags*, *Destructors*); the rustc-dev-guide (*Move paths*,
*Drop elaboration*); the Polonius book (*Atoms*); the Rust Book §4.1; Swift
SE-0176 (*Enforce Exclusive Access to Memory*).

### Accepted terms

| Term | Meaning | Source |
|---|---|---|
| place expression / value expression | Denotes a memory location / denotes a value (formerly lvalue / rvalue) | Ref. *Expressions* |
| place | Oxide: a place expression with no dereference | Oxide |
| move path | A location that can be initialized or moved. Move paths form a tree | rustc-dev-guide *Move paths* |
| moved from; deinitialized | After a move out of a place, the place is deinitialized | Ref. *Expressions*; Ref. *Glossary* |
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
| drop flag | A per-variable runtime flag recording whether a drop is still owed | Rustonomicon *Drop Flags* |
| drop elaboration; static / dead / conditional / open drop | Rewriting drops into code guarded by flags. The target is always initialized / always uninitialized / either wholly initialized or wholly uninitialized / possibly partly initialized. "Dynamic drops" is the guide's heading for the flag-based scheme (RFC 320), not a kind | rustc-dev-guide *Drop elaboration* |
| substructural (context) | RustBelt's typing context is substructural | RustBelt §2, §3.3 |
| law of exclusivity | A modification of a variable must be exclusive with any other access to it | Swift SE-0176, which takes the name from the Swift Ownership Manifesto |
| ownership / sharing predicate | `⟦τ⟧.own`, `⟦τ⟧.shr` | RustBelt §4 |

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
| "hole", `⊘`, `Contents.hole` (an uninitialised or moved-out cell, §6.1) | moved from / deinitialized / uninitialized (Rust Reference); dead (Oxide) | partial: the accepted words exist, and "hole" already means something else (§1) |
| `MovedOut` (`OwnSt.movedOut`) | moved from; `τ†` (Oxide) | clear |
| `OwnSt.fields` ("a partially moved value") | partial move / partially initialized (Ref.); maybe-dead type (Oxide); the move-path tree (rustc) | clear |
| "residue": first (§4) `residue(T, π)`, the places a declared-linear destructure leaves unselected, which are dropped at once; also (Dynamics, GUIDE) what remains of a partially moved cell | the initialized fields of a partially moved value. There is no accepted noun | partial |
| "place", "path" (§4, `Place.path`) | place (Ref., Oxide); path (Polonius); move path (rustc) | clear |
| scope record `s`, dropped "newest-first" (§6.1) | drop scope; reverse order of declaration | clear |
| §6.11's drop order (a value's destructor first, then its contents by kind: a struct's fields in declaration order, an array's elements in ascending order, an enum's active payload only) | drop glue; drop order (Ref. *Destructors*) | clear |
| the dynamic `⊘` skip during a drop walk | the job drop flags do: conditional and open drops. GUIDE already calls it a per-element drop flag | partial: the same job, but the state lives in the cell rather than in a flag |
| `Owned` / `MovedOut` (Σ's two states, §5); the `Borrowed` place-use mode; `inout`/`borrow` parameters | owner; moved from; borrow / loan; unique / shared reference | partial |
| "law of exclusivity" (§5.4) | law of exclusivity (Swift SE-0176); in Rust terms, unique (`mut`/`uniq`) vs shared (`shr`/`shrd`) references | clear for the Swift term |

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
& Schneider's. Our properties over *finished* traces (`Blocks`, `no_double_free`)
become ordinary trace properties in the infinite-trace sense by the same
stuttering padding.

### Terms we currently use that differ from this

| Our term | Accepted term | Confidence |
|---|---|---|
| "monitor" (`linearLeak`, `linearOverwrite`, `linearDiscard`, `ownedUnderCopy`): a check the interpreter `eval` adds, which refuses the step (`.stuck w`); `Step` has none | an EM mechanism (Schneider 2000): it terminates the target before a violating step, and Schneider counts a virtual machine whose instruction cycle is augmented this way as EM. In runtime verification a monitor typically only returns a verdict and does not change the execution (Leucker & Schallhart) | partial. Mechanically ours match Schneider's enforcement sense. But they are part of the definitional interpreter rather than isolated from a target, they exist so the safety proof can go through, they make `eval` stricter than §6, and they never fire on checked programs |
| drop trace, `Event`, `tr` | trace of events | clear |
| `no_double_free` (each identity is freed at most once, and each destructor runs at most once, in every run's trace) | an at-most-once **safety** property over traces | clear once stated this way |
| `drop_exactly_once` ("consumed exactly once") | "exactly once" (delivery) = at most once ∧ at least once; linear use = exactly once. Ours is per value, per finished evaluation | partial |
| `Blocks`, `run_blocks` (every finished run's trace is in a grammar) | a trace property over finished traces. No accepted name for this shape | none |
| `Tidy` / `eval_tidy` (every cell an evaluation allocates is retired by its end) | no verified counterpart | none |
| "identity ledger" (explain renderings) | no verified counterpart | none |
| "observable outcome" (`@dbg` output, exit code; GUIDE §2) | observable behavior (C11; CompCert) | clear |

---

## 7. Testing against an executable semantics

**Sources:** McKeeman 1998; Yang, Chen, Eide & Regehr 2011 (Csmith);
Regehr et al. 2012 (C-Reduce); Claessen & Hughes 2000 (QuickCheck); Barr et
al. 2015; Chen et al. 2020 (survey; numbering below is from its 2019
preprint); Disselkoen et al. 2024 (Cedar); Leroy
2009 (CACM) and 2009 (JAR); Kumar et al. 2014 (CakeML); Pnueli, Siegel &
Singerman 1998; Necula 2000; the libFuzzer documentation.

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

**Where the bridge sits.** The bridge compares the model with the
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
the Rust implementation; ours prints each core program as a Rue module first. The
bridge is **not translation validation**, which establishes `S ≈ C` for every
execution of one compilation and must itself be proved sound. It is not
verified compilation either. For one closed, deterministic, terminating
program, a run checks the single behavior that program has, which is one
instance of backward simulation. The check is not proof-producing, and it
trusts the runner, the printer and the model.

### Terms we currently use that differ from this

| Our term | Accepted term | Confidence |
|---|---|---|
| "the bridge", "bridge corpus" (ADR-0097; [lean/README.md](lean/README.md)) | differential testing against an executable model; in `--gen` mode, randomized differential testing (Csmith) / DRT (Cedar) | partial: "bridge" is our word, and the activity has an accepted name. GUIDE §2 also uses "bridge" for the adequacy theorems between `eval` and `Step`, a second meaning |
| "differential-tested", `rue-oracle-diff` | differential testing | clear |
| "oracle" (`rue-oracle`, the executable reference interpreter) | test oracle, specifically a pseudo-oracle, which Barr class as a derived oracle. The accepted term names the pass/fail judge, not the interpreter itself | partial |
| "executable reference interpreter" / "the executable semantics" (§6) | executable model / specification (Cedar) | clear |
| "seed cases", "seed corpus" (the hand-written corpus cases) | the initial seeds of a fuzzing corpus (libFuzzer) | partial: fuzzing seeds are the initial inputs that mutation starts from |
| `--seed N`, `gen_<seed>_<i>` | random seed (libFuzzer `-seed=N`; Csmith §3.4) | clear |
| generated programs (`Gen.lean`) | random program generation | clear |
| "verdict" (the checker's accept or reject on a case) | no counterpart. In runtime verification a verdict is a monitor's output (§6), a different object | none |
| "model gap", "gap registry" (01 §6.13.6; these belong to `rue-oracle`, not to the Lean model) | no verified counterpart | none |
| "red case", "disagreement" | a candidate for a bug-exposing test (McKeeman: the results differ, or one system hangs or crashes) | partial |

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
| Atkey 2018 | R. Atkey. Syntax and Semantics of Quantitative Type Theory. LICS 2018, 56–65 | https://doi.org/10.1145/3209108.3209189 | Crossref; PDF at bentnib.org |
| Barendregt & Wiedijk 2005 | H. Barendregt, F. Wiedijk. The Challenge of Computer Mathematics. *Phil. Trans. R. Soc. A* 363(1835):2351–2375, 2005 | https://doi.org/10.1098/rsta.2005.1650 | Crossref; the authors' preprint in the Radboud repository (https://hdl.handle.net/2066/32307) |
| Barr et al. 2015 | E. T. Barr, M. Harman, P. McMinn, M. Shahbaz, S. Yoo. The Oracle Problem in Software Testing: A Survey. *IEEE TSE* 41(5):507–525, 2015 | https://doi.org/10.1109/TSE.2014.2372785 | Crossref; PDF of the same title |
| Bernardy et al. 2018 | J.-P. Bernardy, M. Boespflug, R. R. Newton, S. Peyton Jones, A. Spiwack. Linear Haskell: practical linearity in a higher-order polymorphic language. *PACMPL* 2(POPL), 2018 | https://arxiv.org/abs/1710.09756 | "Linear Haskell: practical linearity in a higher-order polymorphic language" |
| Cedar 2024 | C. Disselkoen et al. How We Built Cedar: A Verification-Guided Approach. FSE Companion '24, 351–357, 2024 (doi:10.1145/3663529.3663854) | https://arxiv.org/abs/2407.01688 | "How We Built Cedar: A Verification-Guided Approach"; the arXiv PDF's first page gives the FSE Companion venue and DOI |
| Charguéraud 2013 | A. Charguéraud. Pretty-Big-Step Semantics. ESOP 2013, *Programming Languages and Systems*, LNCS, 41–60 | https://doi.org/10.1007/978-3-642-37036-6_3 | Crossref; author PDF (chargueraud.org/research/2012/pretty/pretty.pdf) |
| Chen et al. 2020 | J. Chen, J. Patra, M. Pradel, Y. Xiong, H. Zhang, D. Hao, L. Zhang. A Survey of Compiler Testing. *ACM Comput. Surv.* 53(1), Art. 4, 2020 | https://doi.org/10.1145/3363562 | Crossref; 2019 preprint PDF of the same title (section numbers here are the preprint's) |
| Claessen & Hughes 2000 | K. Claessen, J. Hughes. QuickCheck: A Lightweight Tool for Random Testing of Haskell Programs. ICFP 2000, 268–279 | https://doi.org/10.1145/351240.351266 | Crossref; PDF of the same title |
| Clarkson & Schneider 2010 | M. R. Clarkson, F. B. Schneider. Hyperproperties. *J. Comput. Secur.* 18(6):1157–1210, 2010 | https://doi.org/10.3233/JCS-2009-0393 | "Hyperproperties" (Crossref; Cornell PDF) |
| Confluent | Kafka Message Delivery Guarantees (Confluent documentation) | https://docs.confluent.io/kafka/design/delivery-semantics.html | "Kafka Message Delivery Guarantees" |
| C11 | ISO/IEC 9899:201x, draft N1570, §5.1.2.3 | https://port70.net/~nsz/c/c11/n1570.html | "N1570 … ISO/IEC 9899:201x" |
| CakeML 2014 | R. Kumar, M. O. Myreen, M. Norrish, S. Owens. CakeML: A Verified Implementation of ML. POPL 2014, 179–191 | https://doi.org/10.1145/2535838.2535841 | Crossref; cakeml.org/popl14.pdf |
| C-Reduce 2012 | J. Regehr, Y. Chen, P. Cuoq, E. Eide, C. Ellison, X. Yang. Test-Case Reduction for C Compiler Bugs. PLDI 2012, 335–346 | https://doi.org/10.1145/2254064.2254104 | Crossref; preprint of the same title |
| Csmith 2011 | X. Yang, Y. Chen, E. Eide, J. Regehr. Finding and Understanding Bugs in C Compilers. PLDI 2011, 283–294 | https://doi.org/10.1145/1993498.1993532 | Crossref; preprint of the same title |
| Dreyer et al. 2019 | D. Dreyer, A. Timany, R. Krebbers, L. Birkedal, R. Jung. What Type Soundness Theorem Do You Really Want to Prove? SIGPLAN Blog, 2019-10-17 | https://blog.sigplan.org/2019/10/17/what-type-soundness-theorem-do-you-really-want-to-prove/ | same title |
| Felleisen & Hieb 1992 | M. Felleisen, R. Hieb. The revised report on the syntactic theories of sequential control and state. *Theor. Comput. Sci.* 103(2):235–271, 1992 | https://doi.org/10.1016/0304-3975(92)90014-7 | OpenAlex record; the Rice TR 100-89 preprint of the same title (numbering here is the preprint's) |
| Harper 2016 (PFPL) | R. Harper. *Practical Foundations for Programming Languages*, 2nd ed. Cambridge University Press, 2016 | https://doi.org/10.1017/CBO9781316576892 | CUP page; cs.cmu.edu/~rwh/pfpl and its abbreviated PDF |
| Lamport 1977 | L. Lamport. Proving the Correctness of Multiprocess Programs. *IEEE TSE* SE-3(2):125–143, 1977 | https://doi.org/10.1109/TSE.1977.229904 | Crossref; "The Writings of Leslie Lamport" (which says this paper introduced "safety" and "liveness") |
| Lean Reference | *The Lean Language Reference* | https://lean-lang.org/doc/reference/latest/ | "The Lean Language Reference" and its chapters 2, 4, 7.4, 7.6, 8, "Validating a Lean Proof" |
| Lean 4.29.0 | Lean 4.29.0 release notes | https://lean-lang.org/doc/reference/latest/releases/v4.29.0/ | "Lean 4.29.0 (2026-03-27)" |
| Lean API | `Lean.ReducibilityAttrs`, `Init.Tactics` | https://lean-lang.org/doc/api/Lean/ReducibilityAttrs.html | "Lean.ReducibilityAttrs"; "Init.Tactics" |
| Leroy 2009a (CACM) | X. Leroy. Formal verification of a realistic compiler. *CACM* 52(7):107–115, 2009 | https://doi.org/10.1145/1538788.1538814 | Crossref; xavierleroy.org PDF |
| Leroy 2009b (JAR) | X. Leroy. A formally verified compiler back-end. *J. Autom. Reasoning* 43(4):363–446, 2009 | https://arxiv.org/abs/0902.2137 | "A formally verified compiler back-end" |
| Leroy & Grall 2009 | X. Leroy, H. Grall. Coinductive big-step operational semantics. *Inf. Comput.* 207(2):284–304, 2009 | https://doi.org/10.1016/j.ic.2007.12.004 (arXiv:0808.0586) | "Coinductive big-step operational semantics" (arXiv) |
| Leucker & Schallhart 2009 | M. Leucker, C. Schallhart. A brief account of runtime verification. *J. Log. Algebr. Program.* 78(5):293–303, 2009 | https://doi.org/10.1016/j.jlap.2008.08.004 | Crossref; Lübeck PDF "A Brief Account of Runtime Verification" |
| libFuzzer | libFuzzer documentation | https://llvm.org/docs/LibFuzzer.html | "libFuzzer – a library for coverage-guided fuzz testing" |
| McBride 2016 | C. McBride. I Got Plenty o' Nuttin'. In *A List of Successes That Can Change the World*, LNCS 9600, 207–233, 2016 | https://doi.org/10.1007/978-3-319-30936-1_12 | "I got plenty o' nuttin'" (Strathclyde); preprint |
| McKeeman 1998 | W. M. McKeeman. Differential Testing for Software. *Digital Technical Journal* 10(1):100–107, 1998 | https://www.cs.tufts.edu/~nr/cs257/archive/bill-mckeeman/DifferentailTesting.pdf (no DOI found) | "Differential Testing for Software" |
| Milner 1978 | R. Milner. A theory of type polymorphism in programming. *J. Comput. Syst. Sci.* 17(3):348–375, 1978 | https://doi.org/10.1016/0022-0000(78)90014-4 | Edinburgh Research Explorer abstract page; OpenAlex; full text (homepages.inf.ed.ac.uk/wadler/papers/papers-we-love/milner-type-polymorphism.pdf) |
| Necula 2000 | G. C. Necula. Translation Validation for an Optimizing Compiler. PLDI 2000, 83–94 | https://doi.org/10.1145/349299.349314 | Crossref; Berkeley PDF |
| Nipkow & Klein | T. Nipkow, G. Klein. *Concrete Semantics*; Isabelle HOL-IMP theory `Small_Step` | http://concrete-semantics.org/ and https://isabelle.in.tum.de/library/HOL/HOL-IMP/Small_Step.html | "Concrete Semantics"; "Small-Step Semantics of Commands" |
| Niu, Sterling & Harper 2024 | Y. Niu, J. Sterling, R. Harper. Cost-sensitive computational adequacy of higher-order recursion in synthetic domain theory. MFPS 2024 (ENTICS 4) | https://arxiv.org/abs/2404.00212 | arXiv abstract page; full text |
| Owens et al. 2016 | S. Owens, M. O. Myreen, R. Kumar, Y. K. Tan. Functional Big-Step Semantics. ESOP 2016, LNCS 9632, 589–615 | https://doi.org/10.1007/978-3-662-49498-1_23 | Crossref; "Functional Big-step Semantics" (cl.cam.ac.uk PDF) |
| Oxide | A. Weiss, O. Gierczak, D. Patterson, A. Ahmed. Oxide: The Essence of Rust. arXiv:1903.00982 | https://arxiv.org/abs/1903.00982 | "Oxide: The Essence of Rust" |
| Pierce 2002 (TAPL) | B. C. Pierce. *Types and Programming Languages*. MIT Press, 2002 | https://www.cis.upenn.edu/~bcpierce/tapl/ | "Types and Programming Languages", with its table of contents and errata (text not read) |
| Plotkin 1977 | G. D. Plotkin. LCF considered as a programming language. *Theor. Comput. Sci.* 5(3):223–255, 1977 **(record)** | https://doi.org/10.1016/0304-3975(77)90044-5 | Crossref record |
| Plotkin 1981/2004 | G. D. Plotkin. A Structural Approach to Operational Semantics. DAIMI FN-19, Aarhus, 1981; *J. Log. Algebr. Program.* 60–61:17–139, 2004 | https://doi.org/10.1016/j.jlap.2004.05.001 | Crossref/OpenAlex record; the author's own 2004 edition (homepages.inf.ed.ac.uk/gdp/publications/sos_jlap.pdf), whose numbering is used here |
| Plotkin 2004b | G. D. Plotkin. The Origins of Structural Operational Semantics. *J. Log. Algebr. Program.* 60–61:3–15, 2004 | https://doi.org/10.1016/j.jlap.2004.03.009 | "The Origins of Structural Operational Semantics" (author PDF) |
| Pnueli et al. 1998 | A. Pnueli, M. Siegel, E. Singerman. Translation Validation. TACAS 1998, LNCS 1384, 151–166 | https://doi.org/10.1007/BFb0054170 | Crossref; Weizmann research-portal page |
| Polonius | The Polonius book, "Atoms" | https://rust-lang.github.io/polonius/rules/atoms.html | "Atoms - Polonius" |
| Reynolds 1972 | J. C. Reynolds. Definitional interpreters for higher-order programming languages. ACM '72, 717–740; reprinted *Higher-Order Symb. Comput.* 11(4):363–397, 1998 **(record)** | https://doi.org/10.1023/A:1010027404223 | Crossref records for both |
| Rust Book | *The Rust Programming Language*, §4.1 "What is Ownership?" | https://doc.rust-lang.org/book/ch04-01-what-is-ownership.html | "What is Ownership? - The Rust Programming Language" |
| Rust Reference | *The Rust Reference*: Destructors; Expressions; Glossary; Patterns (and the whole-book `print.html`, searched for "affine") | https://doc.rust-lang.org/reference/destructors.html | "Destructors - The Rust Reference" (and "Expressions", "Glossary", "Patterns") |
| Rustonomicon | *The Rustonomicon*: Drop Flags; Destructors | https://doc.rust-lang.org/nomicon/drop-flags.html | "Drop Flags - The Rustonomicon" |
| rustc-dev-guide | *Rust Compiler Development Guide*: Move paths; Drop elaboration | https://rustc-dev-guide.rust-lang.org/borrow-check/moves-and-initialization/move-paths.html | "Move paths - Rust Compiler Development Guide"; "Drop elaboration - …" |
| RustBelt 2018 | R. Jung, J.-H. Jourdan, R. Krebbers, D. Dreyer. RustBelt: Securing the Foundations of the Rust Programming Language. *PACMPL* 2(POPL), Art. 66, 2018 | https://doi.org/10.1145/3158154 | Crossref; plv.mpi-sws.org/rustbelt/popl18; the paper PDF |
| Schneider 2000 | F. B. Schneider. Enforceable Security Policies. *ACM TISSEC* 3(1):30–50, 2000 | https://doi.org/10.1145/353323.353382 | Crossref; Cornell PDF |
| Siek 2013 | J. Siek. Type Safety in Three Easy Lemmas. Blog post, 2013-05-27 | https://siek.blogspot.com/2013/05/type-safety-in-three-easy-lemmas.html | "Jeremy Siek: Type Safety in Three Easy Lemmas" |
| Software Foundations | B. C. Pierce et al. *Software Foundations*, vol. 1 (`ImpCEvalFun`) and vol. 2 (`Smallstep`, `StlcProp`) | https://softwarefoundations.cis.upenn.edu/plf-current/Smallstep.html | "Smallstep: Small-step Operational Semantics"; "StlcProp"; "ImpCEvalFun" |
| Swift SE-0176 | J. McCall. SE-0176: Enforce Exclusive Access to Memory. Swift Evolution proposal, implemented in Swift 4.0 | https://github.com/swiftlang/swift-evolution/blob/main/proposals/0176-enforce-exclusive-access-to-memory.md | "Enforce Exclusive Access to Memory" |
| Timany et al. 2024 | A. Timany, R. Krebbers, D. Dreyer, L. Birkedal. A Logical Approach to Type Soundness. *J. ACM* 71(6), Art. 40, 2024 | https://doi.org/10.1145/3676954 | "A Logical Approach to Type Soundness" (author page; iris-project.org PDF) |
| TPIL | J. Avigad, L. de Moura, S. Kong, S. Ullrich. *Theorem Proving in Lean 4* | https://lean-lang.org/theorem_proving_in_lean4/ | "Theorem Proving in Lean 4" and its chapters |
| Tov & Pucella 2011 | J. A. Tov, R. Pucella. Practical affine types. POPL 2011, 447–458 | https://doi.org/10.1145/1926385.1926436 | Crossref; long-version PDF "Practical Affine Types" |
| Walker 2005 | D. Walker. Substructural Type Systems. In B. C. Pierce (ed.), *Advanced Topics in Types and Programming Languages*, ch. 1, 3–44. MIT Press | https://doi.org/10.7551/mitpress/1104.003.0003 | Crossref (which dates the chapter 2004); MIT Press sample PDF "1 Substructural Type Systems" |
| Wright & Felleisen 1994 | A. K. Wright, M. Felleisen. A Syntactic Approach to Type Soundness. *Inf. Comput.* 115(1):38–94, 1994 | https://doi.org/10.1006/inco.1994.1093 | OpenAlex record; the Rice TR91-160 preprint of the same title (numbering here is the preprint's) |
