# Lean mechanization spike: findings and project outline

**Status:** spike complete (2026-08-28). Working artifact: `docs/formal/lean/`
(Lean 4.33.1 package `RueCore`, ~1.3k lines, zero `sorry`, axioms
`propext`/`Quot.sound` only). This note records what the spike proved out,
what we learned, what looks provable about the language, and a milestone
outline for making mechanized proofs a real part of the design and
verification system. Everything here is proposal material for maintainer
decisions, not enacted policy.

## 1. What the spike is

A machine-checked mechanization of a fragment of the core calculus
(`docs/formal/01-core-calculus.md`) with its slice of the §7 memory-safety
theorems actually proved:

- **Statics** (§4.2, §5.1–§5.6): the ownership-threading judgment
  `Γ;Σ ⊢ e ⇒ T ⊣ Σ'` as an inductive relation over a fused flow-sensitive
  context; (Use-Copy)/(Use-Move), (@Drop), (Assign) with the `3.8:77`
  linear-overwrite premise — the exact premise the RUE-387 hole demanded —
  (Seq)'s `3.8:64` discard check, (Let) with §5.6's residual-linear leak
  check, and (If) with the §5.5 conservative branch join.
- **Dynamics** (§6): a total definitional interpreter over the §6.1
  configuration shape (store of binding allocations with `⊘`/`†` markers,
  environment, drop trace), with §6.4's overflow/div-zero traps and §6.7's
  scope-exit drop-retire. Memory violations are *named refusals*
  (`useAfterMove`, `useAfterFree`, `linearLeak`, `linearOverwrite`,
  `linearDiscard`) rather than silent behavior — §7's falsifiability
  discipline, mechanized.
- **The theorem** (§7): well-typed programs never reach a violation — they
  yield a well-typed value or a *defined* panic. Stated over the invariant
  `Matches` ("Σ faithfully tracks the store's initialization" — §7's own
  phrase), whose preservation is the substance of the proof. Corollaries are
  named per §7 bullet: `no_use_after_move`, `no_use_after_free`,
  `no_linear_leak`, `no_linear_overwrite`, `no_linear_discard`.
- **A verified checker**: `check : Ctx → Expr → Option (Ty × Ctx)` with
  `check_sound` (every acceptance is a derivation). Example programs are
  accepted/rejected by `rfl` — kernel-checked facts, not test assertions.

Fragment boundaries, deliberately: whole bindings only (no
projections/partial moves), no borrows, no calls, no loops, an abstract
resource type instead of structs. §5's Λ is ambiently empty in the current
core anyway, so the borrow omission tracks the calculus itself.

## 2. Findings

### 2.1 The calculus is unusually mechanization-ready

`01-core-calculus.md`'s claim that it is "plain ASCII so it can be
transcribed into a proof assistant" held up. The rules transcribed
essentially one-for-one; every place the Lean encoding needed a decision,
the calculus text had already made it (explicitly, with a RUE citation). The
flow-sensitive Σ-threading — the part that makes ownership typing unusual —
translates directly to an output-context judgment; no separate-worlds
encoding needed. This is *not* the norm for language specs; it is the payoff
of the keystone-first §4.2/§5 design.

### 2.2 The mechanization immediately touched real, load-bearing subtleties

The preservation invariant could not be the naive "Owned ⇔ initialized,
MovedOut ⇔ moved". The §5.5 conservative join (affine place `MovedOut` on
one branch joins to `MovedOut`) makes the static state a conservative
*approximation*: a statically-`MovedOut` cell can still dynamically hold a
live value, which the machine then drops path-specifically (`3.8:73`). The
invariant that works is asymmetric: `Owned` entries hold well-typed values;
`MovedOut` entries hold either `⊘` or a live **non-linear** value — and the
"non-linear" exclusion (forced by the join refusing linear disagreements,
`3.8:50`) is precisely what makes the linear-leak and linear-overwrite
refusals unreachable. The calculus knew all of this; the mechanization
*forced* it to be said as one predicate and checked in every rule. That is
the value proposition in miniature: RUE-387, RUE-1591, RUE-1614/1615 all
lived exactly in this class of cross-rule invariant, and each cost
compiler-vs-spec reconciliation work to find by hand.

### 2.3 Definitional interpreter beats small-step for this system — and matches the oracle

§7 says "progress + preservation", implying a small-step relation. The spike
instead proved safety over a **total definitional interpreter** (no fuel
needed while the fragment is loop-free; fuel generalizes it): well-typedness
implies the interpreter never returns a violation. Same corollaries, far
less mechanization overhead (no evaluation contexts, no administrative
forms, no typing of intermediate configurations), and — decisively — the
Lean artifact then has the *same shape as `crates/rue-oracle`*: an
interpreter producing a result plus a drop trace. The formal README's "the
thing that governs the spec is the thing we can run" applies to the
mechanization too: `#eval` answers semantic questions, and the Lean model
can sit in a differential loop against the Rust oracle (this is AWS Cedar's
production pattern — Lean model, Rust implementation, fuzz both, compare;
Lean executes natively so there is no extraction gap). Recommendation: keep
interpreter-style safety as the primary mechanized theorem; add the
small-step relation later only if a theorem actually needs it (e.g.
fine-grained nonterminating-program claims), with the standard
`step`-function/relation equivalence lemma.

### 2.4 Ecosystem: the road is clear, and surprisingly open

(Web survey, Aug 2026.) Lean stable 4.33.1; **core Lean suffices** — the
spike has zero dependencies, builds in seconds, and needs no Mathlib.
Notable: **no published Lean 4 mechanization of an ownership/borrow
calculus with syntactic type safety exists**; Oxide and Featherweight Rust
(the closest published designs, both flow-sensitive-context based like
ours) were never fully mechanized in any assistant, and the RustBelt
lineage is Coq/Iris-only. iris-lean is real but lacks the program-logic and
lifetime-logic layers — a RustBelt-style *semantic* soundness proof in Lean
is not currently buildable, which independently confirms the syntactic
route the calculus already takes. CI is a solved problem
(`leanprover/lean-action` + `lean4checker`, minutes cold / seconds warm —
cheap enough for the merge queue, though starting as a scheduled tier is the
conservative path). For differential testing, the Cedar
verification-guided-development playbook maps one-to-one onto the RUE-50
oracle architecture.

### 2.5 Effort calibration

The fragment — 14 expression forms, 5 violation classes, join, checker,
all proofs — was one focused build from a standing start, ~1.3k lines. The
honest extrapolation is that paths/partial moves and the loan discipline
are each a multiple of the fragment, not an increment; §6.13 buffers are a
project of their own. Hence the milestone ladder below rather than a single
"mechanize the core" issue.

## 3. What looks provable, per §7 bullet

| §7 claim | Fragment status | Full-core assessment |
| --- | --- | --- |
| Type safety (progress + preservation) | **proved** (interpreter form) | Tractable through calls/loops; loops interact with 3.8:79 (below) |
| No use-after-move | **proved** | Tractable; paths make the invariant recursive (`fully-owned`) |
| No double-free | trace-visible, not yet a theorem | Statable over drop traces; needs value identity (mint ids at `mkres`) |
| No use-after-drop / no drop leak | **proved** (retire half) | "Exactly once at scope exit" needs the σ/endscope bookkeeping |
| No use-after-free (buffers) | out of fragment | §6.13 machine ops + (O1) uniqueness; obligations as explicit interfaces — hardest milestone, but statable without Iris |
| Linear consumed exactly once | **proved** (leak/overwrite/discard refusals) | Declared-linear destructure and residue ordering add real work |
| Exclusivity / no aliased mutation | out of fragment (Λ empty in core today) | Root-granular Λ is finite-state; the §5.4/§5.8 loan-extent rules look mechanizable as stated |

The §5.7/Ω divergence-provenance machinery and (Loop-Break)'s
`edge-observations` meta-projection are the least directly mechanizable
part of §5 as written — the mechanization will force them into an explicit
judgment form. That is a feature: RUE-1614/1615 were both bugs in exactly
that corner, and a Lean encoding of 3.8:79/80 would have refused the
ambiguity that produced them.

## 4. Project outline

**M0 — Spike.** Done; this note and `docs/formal/lean/`.

**M1 — Adopt and wire in** (maintainer decisions first; see §5):
placement blessed; CI leg added (elan bootstrap + `lake build`, initially a
scheduled tier alongside `slow`, promotion to merge-queue gate as a later
explicit decision); `docs/formal/README.md` gains the mechanization as a
fourth view of the language with RUE-305 reconciliation discipline
extended to it; the §-extension rubric gains a step 6: "extend the
mechanization, or file the gap as a tracked issue".

**M2 — Full core statics.** Structs/enums/arrays with paths, partial
moves, `fully-owned`, recursive `residual-linear` (the RUE-1591 model),
declared-linear plans, `match` with join, calls ((Fn)/(Call), by-value
only), loops with back-edge invariance, and the Ω/divergence-provenance
rules — the last two forced into judgment form. Deliverable: `Typed` for
the full §2 grammar + the verified checker kept total, so the checker can
run on elaborated real programs.

**M3 — Full dynamics + safety.** Frames/call stack, σ scope records and
unwind drops, fuel-indexed interpreter (loops make fuel necessary; the
theorem becomes ∀-fuel violation-freedom), drop-order as a proved trace
property (declaration order, `3.9`), no-double-free via minted value
identities. This is where §7's first five bullets become theorems for the
real core.

**M4 — Loans and exclusivity.** Λ threading, call-scoped loans, the
equality-compare borrow, accessor-call loan extents (§5.8), the law of
exclusivity as the §7 theorem plus the owed lemmas (loan/drop
non-interference, loan-extent nesting, root separation, view-intact).

**M5 — The allocation store (§6.13).** Buffer machine ops, container
defining equations, (O1) handle uniqueness, no-use-after-free — with the
§6.13.5 library obligations stated as explicit Lean interfaces
(assumptions), exactly as the calculus frames them. Revisit iris-lean
maturity here; semantic proofs of the obligations themselves are a
separate, later decision.

**M6 — The differential bridge (RUE-50 extension).** Cedar-pattern
three-way agreement: a shared corpus of core programs (JSON), run through
(a) the Lean interpreter, (b) `rue-oracle`, (c) the compiler; any pairwise
disagreement is a bug in one of them. The Lean checker also cross-checks
the compiler's accept/reject decisions on ownership programs. This makes
the mechanization *continuously* falsifiable rather than correct-once.

**M7 — `03-metatheory.md`.** The planned proofs doc becomes real: each §7
bullet cites its Lean theorem by name; spec traceability gains a
paragraph↔theorem index in the `rue-spec` discipline's style.

Sequencing: M2→M3 are the long poles and can proceed without new
maintainer decisions once M1 lands. M4 and M5 each begin with the **[open]**
confirmations §9 already requests (loans second-class; buffers via
container equations). M6 is parallelizable with M4/M5.

## 5. Maintainer decision points (Backlog candidates)

1. **Adopt the artifact**: `docs/formal/lean/` as the mechanization home,
   with the spike as its seed. (Includes: is the toolchain pin acceptable
   dev tooling? elan is a one-command, user-local install; nothing touches
   the Buck graph initially.)
2. **CI posture**: scheduled tier first vs. merge-queue gate; whether a
   `lake build` failure on a language-change PR blocks it (the drift
   question — see risk 1).
3. **Authority**: extend RUE-305's three-views discipline to four (prose,
   core, compiler, mechanization), with the same "disagreement is a defect,
   no precedence" rule.
4. **Theorem-shape**: bless interpreter-style safety as the primary §7
   statement (with small-step as an optional later refinement) — this
   note's recommendation, and a change `03-metatheory.md` should record.
5. **Staffing/workflow**: mechanization changes are unusually reviewable
   (the kernel checks the proofs; review is about *statement* fidelity to
   the calculus) — a good fit for the agent-implemented,
   maintainer-reviewed division of labor, but statement review must stay
   human.

## 6. Risks

- **Drift.** Four artifacts can disagree in six pairs. Mitigation is the
  M6 differential bridge plus CI: drift becomes a red check, not a latent
  divergence. Until M6, the mechanization cites calculus sections the way
  the calculus cites prose paragraphs.
- **Proof-maintenance cost.** Every language change grows a proof
  obligation. Mitigations: keep proofs boring (structural inductions over
  syntax-directed rules — the spike pattern); the checker-first style
  (extending `check` is cheap and its soundness proof is mechanical); the
  rubric's "file the gap" escape hatch so a language change is never
  blocked on a proof, only tracked by an issue.
- **The §5.7/loop corner.** `edge-observations` is meta-level prose today;
  mechanizing it may force calculus rewrites. Treat those rewrites as the
  deliverable, not an obstacle.
- **Buffers need more logic.** If M5's obligation interfaces prove
  unsatisfying without separation logic, the semantic-proof layer waits on
  iris-lean's program-logic layer (not expected soon). The obligations
  remain explicit assumptions either way — which is already §6.13.5's
  framing.
- **Bus factor.** One more formalism in the repo. Mitigated by the spike's
  zero-dependency, core-Lean-only, heavily-commented style, and by the
  checker/examples giving non-Lean readers an executable entry point.
