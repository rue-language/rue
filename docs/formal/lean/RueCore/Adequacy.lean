import RueCore.Step
import RueCore.Soundness
import RueCore.Examples

/-!
# RueCore.Adequacy — `eval` is adequate to §6's `Step`, both ways

ADR-0097's decision 3 proves safety over the definitional interpreter `eval`
and says that "a theorem about `eval` is a theorem about §6 only once the two
are proved to agree". `Step.lean` mechanizes §6's reduction relation; this
module proves the agreement in both directions. RUE-2289's part 2 is
soundness: whatever `eval` answers with a value or a panic, §6's `→*` reaches
too, with the same store, value and trace (`eval_sound`). Part 3 is
completeness modulo fuel: whatever terminal configuration §6's `→*` reaches,
every fuel past the length of the run makes `eval` answer it
(`eval_complete`); `eval` exhausts every fuel exactly when §6 diverges
(`eval_diverges_iff`); and "`eval` is never stuck" is "§6 is never stuck", in
§7's phrasing (`never_stuck_iff`).

## The simulation relation

`eval H φ e` is one expression run to its end; `Step` runs a configuration
whose focus is that expression under a context. §6.2 writes the configuration
`⟨H ; φ ; K ; E[e]⟩`; here the context is the list of frames `K` below the
focus (`Step.lean`'s module docstring), so an expression in focus is the
family `evalConf H φ e K tr`, one configuration for every context `K` and every
trace `tr` already produced. `Sim M P φ C r` says what `eval`'s result `r`
promises about `→*` from the family `C`:

* `.ok H' v tr'`: for every context, `E[e] →* E[v]`, in the frame `φ` the
  expression started in, with the store `H'` and the trace extended by `tr'`
  — §6.2's (Search) run to the hole's value;
* `.panic κ tr'`: for every context, `E[e] →* ↯κ` after `tr'` — (Panic-Lift)
  §6.2;
* `.returned H' v tr'`: for every context with a `ret(E', φs)` frame
  (`Kont.toCall`), the configuration reaches the caller `E'[v]` in `φs` —
  (D-Return) §6.9, whose drops `eval` has already run from the same frame;
* `.broke H' sc tr'`: for every context with a loop boundary before any call
  frame (`Kont.toLoop`) and every successful unwind of the cells the body
  still owed, the configuration reaches the loop's context with `⟨⟩` —
  (D-Break) §6.10. `eval`'s loop supplies both premises: the loop frame is on
  top of the body's context, and `unwindLocs_plain` turns its monitored unwind
  into the plain one;
* `.stuck` and `.outOfFuel`: nothing.

`eval_sim` is `Sim` for every expression, store, frame and fuel. Each
`andThen` in `eval` is one (Search) enter step, the operand's `Sim` under one
more frame, and a plug step (`Sim.andThen`); argument lists are
`evalArgs_sim`; the call boundary is `Sim.absorb`; and a loop turn that
finishes re-enters the body through (D-Loop-Iter) where `eval` re-evaluates
the whole loop at one less fuel, the one (D-Loop-Enter) between them peeled
off by determinism (`Sim.peel`, `Step.det`).

## Where typing enters, and where it does not

`eval_sim` and `run_sim` hold on **every** program. The places where `eval`
and `Step` differ are all refusals on `eval`'s side: the four monitors
(`linearLeak`, `linearOverwrite`, `linearDiscard`, `ownedUnderCopy`) and the
`useAfterMove` refusal of `@drop` at a `⊘` place, which §6.11 makes a no-op.
A refusal is `.stuck`, about which `Sim` promises nothing, and where a monitor
lets a drop through the plain drop does the same thing (`unwindLocs_plain`,
`destructure_plain`, `dropRetire_plain`). So soundness needs no typing
derivation.

Typing is what fixes the domain (RUE-2314): `eval_sound` is stated for the
programs `check` accepts, where `no_violation` says `run` is never `.stuck`.
There every outcome is a value, a panic, or `outOfFuel`, and the first two are
§6's. On other input `run_sim` still holds but says nothing about `.stuck`,
which is outside the correspondence: four monitors are not §6's, and `eval`
inspects operand shapes in an order §6.2 does not fix. Completeness (part 3)
is where the domain does work, because there `Step` can step where `eval`
refuses.

## Completeness: fuel is a lower bound on steps

Part 3 needs no converse simulation. Its one new fact is
`eval_steps_of_outOfFuel`: if `eval` exhausts `fuel` on an expression, §6's
reduction has a run of exactly `fuel` steps from that expression in focus
(`StepsN`). Each unit of fuel is paid for by at least one `Step`: every
recursive call of `eval` sits behind a (Search) enter step (§6.2), and the
operands before it reached values, which `Sim`'s `ok` clause turns into runs.
Two forms spend fuel without a step of their own, and each is paid for by the
next step. `@drop` at a dynamic place re-dispatches to the read, and the push
into its first index pays for that. The loop re-evaluates itself at one less
fuel, and (D-Loop-Iter) (§6.10) pays for that; the re-evaluation's
(D-Loop-Enter) is peeled off by determinism, as in `eval_sim`.

The rest is determinism (`Step.det`). A run that ends at a configuration with
no successor, terminal or stuck, bounds every run from the same start
(`StepsN.bound`). So past its length, `run` is not `outOfFuel`, and whatever
it answers is placed by `run_sim` at the same end (`Steps.final_unique`).
`run_complete` and `run_stuck_of_step_stuck` hold on every program, up to a
refusal of `eval`'s. The checked domain removes the refusal (`no_violation`),
which gives `eval_complete`, `never_stuck_iff` and `eval_diverges_iff`.
`dropMoved_refused` shows the refusal is really there off the domain.
-/

namespace RueCore

/-! ## The simulation relation -/

/-- The configuration family of an expression in focus: `⟨H ; φ ; K ; E[e]⟩`
for every context `K` and every trace `tr` already produced (§6.1, §6.2). -/
abbrev evalConf (H : Store) (φ : Frame) (e : Expr) : List Kont → List Event → Config :=
  fun K tr => .run H φ K (.eval e) tr

/-- **The simulation relation** between an `eval` result and §6's `→*`
(RUE-2289, parts 2 and 3; the module docstring reads it clause by clause). `C`
is a configuration family indexed by the context `K` below the focus and the
trace `tr` produced before it, §6.2's `⟨H ; φ ; K ; E[e]⟩`: a value reaches
`E[v]` in the frame `φ` (§6.2's (Search)), a panic reaches `↯κ` from every
context ((Panic-Lift) §6.2), an unwinding `return` reaches the nearest caller
((D-Return) §6.9), and an unwinding `break` reaches the nearest loop's context
((D-Break) §6.10). Part 3's completeness (`eval_complete`) takes its runs
through already-reduced operands from this relation's `ok` clause. -/
def Sim (M : FloatOps) (P : Program) (φ : Frame) (C : List Kont → List Event → Config) :
    EvalRes → Prop
  | .ok H v tr' => ∀ K tr, Steps M P (C K tr) (.run H φ K (.ret v) (tr ++ tr'))
  | .panic k tr' => ∀ K tr, Steps M P (C K tr) (.panic k (tr ++ tr'))
  | .returned H v tr' => ∀ K tr φs K', Kont.toCall K = some (φs, K') →
      Steps M P (C K tr) (.run H φs K' (.ret v) (tr ++ tr'))
  | .broke H sc tr' => ∀ K tr φs K' H' evs, Kont.toLoop K = some (φs, K') →
      plainUnwind P.decls H (sc.drop φs.scope.length).reverse = .ok (H', evs) →
      Steps M P (C K tr) (.run H' φs K' (.ret .unit) (tr ++ tr' ++ evs))
  | .stuck _ => True
  | .outOfFuel => True

/-! ## `→*` -/

/-- `→*` composes (§6.12) (helper). -/
theorem Steps.trans {M : FloatOps} {P : Program} {C₁ C₂ C₃ : Config}
    (h₁ : Steps M P C₁ C₂) (h₂ : Steps M P C₂ C₃) : Steps M P C₁ C₃ := by
  induction h₁ with
  | refl => exact h₂
  | step s _ ih => exact .step s (ih h₂)

/-- One step is a run (§6.12) (helper). -/
theorem Steps.single {M : FloatOps} {P : Program} {C₁ C₂ : Config}
    (h : Step M P C₁ C₂) : Steps M P C₁ C₂ := .step h (.refl _)

/-- Whether a configuration has an expression in focus (helper). -/
def Config.evalFocus : Config → Prop
  | .run _ _ _ (.eval _) _ => True
  | _ => False

/-- **Peeling a step by determinism** (`Step.det`, §6): a run from `C` that
ends at a configuration with no expression in focus passes through `C`'s one
successor (helper). -/
theorem Steps.peel {M : FloatOps} {P : Program} {C C' D : Config}
    (hs : Step M P C C') (h : Steps M P C D) (hC : C.evalFocus) (hD : ¬ D.evalFocus) :
    Steps M P C' D := by
  cases h with
  | refl => exact absurd hC hD
  | step s rest => rw [Step.det hs s]; exact rest

/-! ## Frames a `return` or a `break` passes through -/

/-- A frame `toCall` and `toLoop` look through: every frame but `call` and
`loop` (helper). -/
def Kont.Transparent (F : Kont) : Prop :=
  ∀ K, Kont.toCall (F :: K) = Kont.toCall K ∧ Kont.toLoop (F :: K) = Kont.toLoop K

/-! ## Combinators -/

/-- A run into the family carries its simulation back (helper). -/
theorem Sim.pre {M : FloatOps} {P : Program} {φ : Frame} {C C₂ : List Kont → List Event → Config}
    {r : EvalRes} (hpre : ∀ K tr, Steps M P (C K tr) (C₂ K tr)) (h : Sim M P φ C₂ r) :
    Sim M P φ C r := by
  cases r <;> simp only [Sim] at h ⊢
  · intro K tr; exact (hpre K tr).trans (h K tr)
  · intro K tr φs K' hK; exact (hpre K tr).trans (h K tr φs K' hK)
  · intro K tr φs K' H' evs hK hu; exact (hpre K tr).trans (h K tr φs K' H' evs hK hu)
  · intro K tr; exact (hpre K tr).trans (h K tr)

/-- A run into the family that emits `tr₁` carries its simulation back to
the result with `tr₁` prefixed (§6.12's accumulating output) (helper). -/
theorem Sim.withTrace {M : FloatOps} {P : Program} {φ : Frame}
    {C C₂ : List Kont → List Event → Config} {r : EvalRes} {tr₁ : List Event}
    (hpre : ∀ K tr, Steps M P (C K tr) (C₂ K (tr ++ tr₁))) (h : Sim M P φ C₂ r) :
    Sim M P φ C (r.withTrace tr₁) := by
  cases r <;> simp only [Sim, EvalRes.withTrace] at h ⊢
  · intro K tr; have := h K (tr ++ tr₁); simp only [List.append_assoc] at this
    exact (hpre K tr).trans this
  · intro K tr φs K' hK; have := h K (tr ++ tr₁) φs K' hK; simp only [List.append_assoc] at this
    exact (hpre K tr).trans this
  · intro K tr φs K' H' evs hK hu; have := h K (tr ++ tr₁) φs K' H' evs hK hu
    simp only [List.append_assoc] at this ⊢
    exact (hpre K tr).trans this
  · intro K tr; have := h K (tr ++ tr₁); simp only [List.append_assoc] at this
    exact (hpre K tr).trans this

/-- **§6.2's (Search), once**: `eval`'s `andThen` is an enter step pushing a
frame `F`, the operand run under `F`, and a plug of its value into `F`'s hole.
A `return` or a `break` passes through `F` unchanged because `F` is neither a
call frame nor a loop boundary, and a panic because (Panic-Lift) discards
every context (helper). -/
theorem Sim.andThen {M : FloatOps} {P : Program} {φ φ₁ : Frame}
    {C C₁ : List Kont → List Event → Config} {F : Kont} (hF : F.Transparent)
    (hC : ∀ K tr, Steps M P (C K tr) (C₁ (F :: K) tr))
    {r : EvalRes} (h₁ : Sim M P φ₁ C₁ r) {k : Store → Val → EvalRes}
    (hk : ∀ H₁ v tr₁, r = .ok H₁ v tr₁ →
      Sim M P φ (fun K tr => .run H₁ φ₁ (F :: K) (.ret v) tr) (k H₁ v)) :
    Sim M P φ C (r.andThen k) := by
  cases r with
  | ok H₁ v tr₁ =>
      simp only [EvalRes.andThen]
      exact Sim.withTrace (fun K tr => (hC K tr).trans (h₁ (F :: K) tr)) (hk H₁ v tr₁ rfl)
  | returned H₁ v tr₁ =>
      simp only [EvalRes.andThen, Sim] at h₁ ⊢
      intro K tr φs K' hK
      exact (hC K tr).trans (h₁ (F :: K) tr φs K' (by rw [(hF K).1]; exact hK))
  | broke H₁ sc tr₁ =>
      simp only [EvalRes.andThen, Sim] at h₁ ⊢
      intro K tr φs K' H' evs hK hu
      exact (hC K tr).trans (h₁ (F :: K) tr φs K' H' evs (by rw [(hF K).2]; exact hK) hu)
  | panic κ tr₁ =>
      simp only [EvalRes.andThen, Sim] at h₁ ⊢
      intro K tr
      exact (hC K tr).trans (h₁ (F :: K) tr)
  | stuck w => simp [EvalRes.andThen, Sim]
  | outOfFuel => simp [EvalRes.andThen, Sim]


/-- A result that is not a value passes through a transparent frame unchanged
(helper). -/
theorem Sim.lift {M : FloatOps} {P : Program} {φ φ₁ : Frame}
    {C C₁ : List Kont → List Event → Config} {F : Kont} (hF : F.Transparent)
    (hC : ∀ K tr, Steps M P (C K tr) (C₁ (F :: K) tr))
    {r : EvalRes} (h₁ : Sim M P φ₁ C₁ r) (hr : ∀ H v tr, r ≠ .ok H v tr) :
    Sim M P φ C r := by
  have := Sim.andThen (φ := φ) (k := fun _ _ => .outOfFuel) hF hC h₁
    (fun H v tr h => absurd h (hr H v tr))
  cases r <;> simp_all [EvalRes.andThen]

/-- §6.9's call boundary: the body's `returned` is caught at the `call φ`
frame, which is what `absorb` turns into a value (helper). -/
theorem Sim.absorb {M : FloatOps} {P : Program} {φ φ₁ : Frame}
    {C C₁ : List Kont → List Event → Config}
    (hC : ∀ K tr, Steps M P (C K tr) (C₁ (.call φ :: K) tr))
    {r : EvalRes} (h₁ : Sim M P φ₁ C₁ r) {k : Store → Val → EvalRes}
    (hk : ∀ H₁ v tr₁, r = .ok H₁ v tr₁ →
      Sim M P φ (fun K tr => .run H₁ φ₁ (.call φ :: K) (.ret v) tr) (k H₁ v)) :
    Sim M P φ C (r.absorb k) := by
  cases r with
  | ok H₁ v tr₁ =>
      simp only [EvalRes.absorb]
      exact Sim.withTrace (fun K tr => (hC K tr).trans (h₁ (.call φ :: K) tr)) (hk H₁ v tr₁ rfl)
  | returned H₁ v tr₁ =>
      simp only [EvalRes.absorb, Sim] at h₁ ⊢
      intro K tr
      exact (hC K tr).trans (h₁ (.call φ :: K) tr φ K rfl)
  | broke H₁ sc tr₁ => simp [EvalRes.absorb, Sim]
  | panic κ tr₁ =>
      simp only [EvalRes.absorb, Sim] at h₁ ⊢
      intro K tr
      exact (hC K tr).trans (h₁ (.call φ :: K) tr)
  | stuck w => simp [EvalRes.absorb, Sim]
  | outOfFuel => simp [EvalRes.absorb, Sim]

/-- §6.4's operator frames: a value plugs the hole, a trap is (Panic-Lift)
(helper). -/
theorem OpRes.sim {M : FloatOps} {P : Program} {φ : Frame} {H : Store} {F : Kont} {v : Val}
    (o : OpRes)
    (hv : ∀ K tr v', o = .val v' → Step M P (.run H φ (F :: K) (.ret v) tr) (.run H φ K (.ret v') tr))
    (ht : ∀ K tr κ, o = .trap κ → Step M P (.run H φ (F :: K) (.ret v) tr) (.panic κ tr)) :
    Sim M P φ (fun K tr => .run H φ (F :: K) (.ret v) tr) (o.toRes H) := by
  cases o with
  | val v' => intro K tr; simpa using Steps.single (hv K tr v' rfl)
  | trap κ => intro K tr; simpa using Steps.single (ht K tr κ rfl)
  | confused => trivial

/-- The induction hypothesis: `eval` at fuel `fuel` is simulated (helper). -/
def SimIH (M : FloatOps) (P : Program) (fuel : Nat) : Prop :=
  ∀ H φ e, Sim M P φ (evalConf H φ e) (eval M fuel P H φ e)

/-- A list context `…( v̄, E, ē )` at a store (helper). -/
abbrev argsConf (H : Store) (φ : Frame) (t : ArgsTag) (vs : List Val) (es : List Expr) :
    List Kont → List Event → Config :=
  fun K tr => .run H φ K (.args t vs es) tr

/-- **Argument lists** (§6.2's `…( v̄, E, ē )`): where `evalArgs` finishes,
`→*` walks the list to its redex; where it aborts, the aborting element's
result is simulated from the list context (helper). -/
theorem evalArgs_sim {M : FloatOps} {P : Program} {fuel : Nat} {φ : Frame}
    (IH : SimIH M P fuel) (t : ArgsTag) : ∀ (es : List Expr) (H : Store) (vs₀ : List Val),
    (∀ H' vs tr', evalArgs (fun H e => eval M fuel P H φ e) H es = .ok H' vs tr' →
      ∀ K tr, Steps M P (.run H φ K (.args t vs₀ es) tr)
        (.run H' φ K (.args t (vs₀ ++ vs) []) (tr ++ tr'))) ∧
    (∀ r, evalArgs (fun H e => eval M fuel P H φ e) H es = .abort r →
      Sim M P φ (argsConf H φ t vs₀ es) r)
  | [], H, vs₀ => by
      refine ⟨fun H' vs tr' h K tr => ?_, fun r h => ?_⟩
      · simp only [evalArgs, ArgsRes.ok.injEq] at h
        obtain ⟨rfl, rfl, rfl⟩ := h
        simpa using Steps.refl _
      · simp [evalArgs] at h
  | e :: es, H, vs₀ => by
      have hpush : ∀ K tr, Steps M P (argsConf H φ t vs₀ (e :: es) K tr)
          (evalConf H φ e (.args t vs₀ es :: K) tr) := fun K tr => Steps.single .argsPush
      cases he : eval M fuel P H φ e with
      | ok H₁ v tr₁ =>
          have hv : ∀ K tr, Steps M P (argsConf H φ t vs₀ (e :: es) K tr)
              (argsConf H₁ φ t (vs₀ ++ [v]) es K (tr ++ tr₁)) := by
            intro K tr
            have h₁ := IH H φ e
            rw [he] at h₁
            exact (hpush K tr).trans ((h₁ _ tr).trans (Steps.single .argsPlug))
          obtain ⟨ihok, ihab⟩ := evalArgs_sim IH t es H₁ (vs₀ ++ [v])
          refine ⟨fun H' vs tr' h K tr => ?_, fun r h => ?_⟩
          · simp only [evalArgs, he] at h
            split at h
            · rename_i H₂ vs₂ tr₂ h₂
              simp only [ArgsRes.ok.injEq] at h
              obtain ⟨rfl, rfl, rfl⟩ := h
              have := ihok _ _ _ h₂ K (tr ++ tr₁)
              simp only [List.append_assoc, List.singleton_append] at this
              exact (hv K tr).trans this
            · simp at h
          · simp only [evalArgs, he] at h
            split at h
            · simp at h
            · rename_i r' h₂
              simp only [ArgsRes.abort.injEq] at h
              subst h
              exact Sim.withTrace hv (ihab r' h₂)
      | _ =>
          refine ⟨fun H' vs tr' h => by simp [evalArgs, he] at h, fun r h => ?_⟩
          simp only [evalArgs, he, ArgsRes.abort.injEq] at h
          subst h
          have h₁ := IH H φ e
          rw [he] at h₁
          exact Sim.lift (fun _ => ⟨rfl, rfl⟩) hpush h₁ (by simp)


/-- Where no `Sim` target has an expression in focus, a first step of `C`
can be peeled off by determinism (helper). -/
theorem Sim.peel {M : FloatOps} {P : Program} {φ : Frame}
    {C C₂ : List Kont → List Event → Config} {r : EvalRes}
    (hs : ∀ K tr, Step M P (C K tr) (C₂ K tr)) (hC : ∀ K tr, (C K tr).evalFocus)
    (h : Sim M P φ C r) : Sim M P φ C₂ r := by
  cases r <;> simp only [Sim] at h ⊢
  · intro K tr; exact Steps.peel (hs K tr) (h K tr) (hC K tr) (by simp [Config.evalFocus])
  · intro K tr φs K' hK
    exact Steps.peel (hs K tr) (h K tr φs K' hK) (hC K tr) (by simp [Config.evalFocus])
  · intro K tr φs K' H' evs hK hu
    exact Steps.peel (hs K tr) (h K tr φs K' H' evs hK hu) (hC K tr) (by simp [Config.evalFocus])
  · intro K tr; exact Steps.peel (hs K tr) (h K tr) (hC K tr) (by simp [Config.evalFocus])

/-- `evalArgs` aborts only with a result that is not a value (helper). -/
theorem evalArgs_abort_ne_ok {ev : Store → Expr → EvalRes} :
    ∀ {es : List Expr} {H : Store} {r : EvalRes}, evalArgs ev H es = .abort r →
      ∀ H' v tr, r ≠ .ok H' v tr
  | [], _, _, h => by simp [evalArgs] at h
  | e :: es, H, r, h => by
      intro H' v tr hr
      subst hr
      simp only [evalArgs] at h
      split at h
      · split at h
        · simp at h
        · rename_i r' h'
          simp only [ArgsRes.abort.injEq] at h
          cases r' <;> simp [EvalRes.withTrace] at h
          exact evalArgs_abort_ne_ok h' _ _ _ rfl
      · rename_i hne
        simp only [ArgsRes.abort.injEq] at h
        exact hne _ _ _ h

/-- `andThen` after a trace prefix (helper). -/
theorem EvalRes.withTrace_andThen (r : EvalRes) (t : List Event) (k : Store → Val → EvalRes) :
    (r.withTrace t).andThen k = (r.andThen k).withTrace t := by
  cases r with
  | ok H v tr =>
      simp only [EvalRes.withTrace, EvalRes.andThen]
      cases k H v <;> simp [List.append_assoc]
  | _ => simp [EvalRes.withTrace, EvalRes.andThen]

/-- The root of a place, as `eval` resolves it inline, is `rootCell` (helper). -/
theorem rootCell_of {H : Store} {φ : Frame} {i ℓ : Nat} {c : Contents}
    (hℓ : φ.env[i]? = some ℓ) (hc : H[ℓ]? = some (.full c)) : rootCell H φ i = .ok (ℓ, c) := by
  simp [rootCell, hℓ, hc]

/-- (D-EndScope) restores the frame (D-Let) or (D-Match) extended (helper). -/
theorem Frame.popScope_push (φ : Frame) (ls : List Nat) :
    ({ env := ls.reverse ++ φ.env, scope := φ.scope ++ ls } : Frame).popScope ls.length = φ := by
  cases φ
  simp [Frame.popScope]

/-- (D-EndScope) after (D-Let) (helper). -/
theorem Frame.popScope_let (φ : Frame) (ℓ : Nat) :
    ({ env := ℓ :: φ.env, scope := φ.scope ++ [ℓ] } : Frame).popScope 1 = φ := by
  cases φ
  simp [Frame.popScope]

/-- The monitor-free unwind of one cell (helper). -/
theorem plainUnwind_single {D : Decls} {H H' : Store} {ℓ : Nat} {evs : List Event}
    (h : dropRetire D H ℓ = .ok (H', evs)) : plainUnwind D H [ℓ] = .ok (H', evs) := by
  simp [plainUnwind, dropRetire_plain h]

section forms
variable {M : FloatOps} {P : Program} {fuel : Nat} {H : Store} {φ : Frame}

/-- (D-Use-Declared-Linear), (D-Use-Copy), (D-Use-Move) §6.3 (helper). -/
theorem sim_use (p : Place) :
    Sim M P φ (evalConf H φ (.use p)) (eval M (fuel + 1) P H φ (.use p)) := by
  simp only [eval]
  split
  · trivial
  · rename_i ℓ hℓ
    split
    · trivial
    · trivial
    · rename_i c hc
      have hroot := rootCell_of hℓ hc
      split
      · rename_i πd πs hplan
        split
        · trivial
        · rename_i cd hcd
          split
          · trivial
          · rename_i leaf evs hd
            split
            · trivial
            · rename_i v hv
              split
              · trivial
              · rename_i c' hw
                intro K tr
                exact Steps.single (.useDeclared hroot hplan hcd (destructure_plain hd) hv hw)
      · rename_i hplan
        split
        · trivial
        · rename_i sub hsub
          split
          · trivial
          · rename_i v hv
            split
            · rename_i hcopy
              intro K tr; simpa using Steps.single (.useCopy hroot hplan hsub hv hcopy)
            · rename_i hcopy
              split
              · trivial
              · rename_i c' hw
                intro K tr; simpa using Steps.single (.useMove hroot hplan hsub hv hcopy hw)

/-- §6.11's `@drop` at a constant place (helper). -/
theorem sim_drop (p : Place) :
    Sim M P φ (evalConf H φ (.drop p)) (eval M (fuel + 1) P H φ (.drop p)) := by
  simp only [eval]
  split
  · trivial
  · rename_i ℓ hℓ
    split
    · trivial
    · trivial
    · rename_i c hc
      have hroot := rootCell_of hℓ hc
      split
      · rename_i πd πs hplan
        split
        · trivial
        · rename_i cd hcd
          split
          · trivial
          · rename_i leaf evs hd
            split
            · trivial
            · split
              · trivial
              · rename_i levs hl
                split
                · trivial
                · rename_i c' hw
                  intro K tr
                  exact Steps.single (.dropDeclared hroot hplan hcd (destructure_plain hd) hl hw)
      · rename_i hplan
        split
        · trivial
        · rename_i sub hsub
          split
          · trivial
          · split
            · trivial
            · rename_i evs hdrop
              split
              · rename_i hcopy
                intro K tr; simpa using Steps.single (.dropCopy hroot hplan hsub hcopy)
              · rename_i hcopy
                split
                · trivial
                · rename_i c' hw
                  intro K tr
                  simpa using Steps.single (.dropMove hroot hplan hsub hcopy hdrop hw)

/-- §6.4's binary operators after §6.2's `E ⊕ e` and `v ⊕ E` (helper). -/
theorem sim_binop (IH : SimIH M P fuel) (op : BinOp) (e₁ e₂ : Expr) :
    Sim M P φ (evalConf H φ (.binop op e₁ e₂)) (eval M (fuel + 1) P H φ (.binop op e₁ e₂)) := by
  simp only [eval]
  refine Sim.andThen (F := .binopL op e₂) (fun _ => ⟨rfl, rfl⟩)
    (fun _ _ => Steps.single .binopEnter) (IH H φ e₁) ?_
  intro H₁ v₁ _ _
  refine Sim.andThen (F := .binopR op v₁) (fun _ => ⟨rfl, rfl⟩)
    (fun _ _ => Steps.single .binopMid) (IH H₁ φ e₂) ?_
  intro H₂ v₂ _ _
  exact OpRes.sim _ (fun _ _ _ h => .binop h) (fun _ _ _ h => .binopTrap h)

/-- §6.4's unary operators after §6.2's `⊖ E` (helper). -/
theorem sim_unop (IH : SimIH M P fuel) (op : UnOp) (e : Expr) :
    Sim M P φ (evalConf H φ (.unop op e)) (eval M (fuel + 1) P H φ (.unop op e)) := by
  simp only [eval]
  refine Sim.andThen (F := .unop op) (fun _ => ⟨rfl, rfl⟩)
    (fun _ _ => Steps.single .unopEnter) (IH H φ e) ?_
  intro H₁ v _ _
  exact OpRes.sim _ (fun _ _ _ h => .unop h) (fun _ _ _ h => .unopTrap h)

/-- (D-Int-Cast) and its trap after §6.2's `@intCast( E )` (helper). -/
theorem sim_intCast (IH : SimIH M P fuel) (w : IntWidth) (sg : Sign) (e : Expr) :
    Sim M P φ (evalConf H φ (.intCast w sg e)) (eval M (fuel + 1) P H φ (.intCast w sg e)) := by
  simp only [eval]
  refine Sim.andThen (F := .intCast w sg) (fun _ => ⟨rfl, rfl⟩)
    (fun _ _ => Steps.single .intCastEnter) (IH H φ e) ?_
  intro H₁ v _ _
  exact OpRes.sim _ (fun _ _ _ h => .intCast h) (fun _ _ _ h => .intCastTrap h)

/-- §6.4's float intrinsics after §6.2's `@f( E )` (helper). -/
theorem sim_fintrin (IH : SimIH M P fuel) (k : FloatIntrin) (e : Expr) :
    Sim M P φ (evalConf H φ (.fintrin k e)) (eval M (fuel + 1) P H φ (.fintrin k e)) := by
  simp only [eval]
  refine Sim.andThen (F := .fintrin k) (fun _ => ⟨rfl, rfl⟩)
    (fun _ _ => Steps.single .fintrinEnter) (IH H φ e) ?_
  intro H₁ v _ _
  exact OpRes.sim _ (fun _ _ _ h => .fintrin h) (fun _ _ _ h => .fintrinTrap h)

/-- `@dbg` (§6.12) after §6.2's `@dbg( E )` (helper). -/
theorem sim_dbg (IH : SimIH M P fuel) (e : Expr) :
    Sim M P φ (evalConf H φ (.dbg e)) (eval M (fuel + 1) P H φ (.dbg e)) := by
  simp only [eval]
  refine Sim.andThen (F := .dbg) (fun _ => ⟨rfl, rfl⟩)
    (fun _ _ => Steps.single .dbgEnter) (IH H φ e) ?_
  intro H₁ v _ _ K tr
  exact Steps.single .dbg

/-- (D-Struct) §6.5 after §6.2's search through the initializers; the
identity is minted as `introVal` mints it (helper). -/
theorem sim_mkStruct (IH : SimIH M P fuel) (s : Nat) (args : List Expr) :
    Sim M P φ (evalConf H φ (.mkStruct s args)) (eval M (fuel + 1) P H φ (.mkStruct s args)) := by
  simp only [eval]
  obtain ⟨ihok, ihab⟩ := evalArgs_sim (φ := φ) IH (.struct s) args H []
  have hent : ∀ K tr, Steps M P (evalConf H φ (.mkStruct s args) K tr)
      (argsConf H φ (.struct s) [] args K tr) := fun _ _ => Steps.single .structEnter
  split
  · rename_i r hr; exact Sim.pre hent (ihab r hr)
  · rename_i H₁ vs tr₁ hr
    refine Sim.withTrace (C₂ := argsConf H₁ φ (.struct s) vs [])
      (fun K tr => (hent K tr).trans (by simpa using ihok _ _ _ hr K tr)) ?_
    split
    · trivial
    · rename_i sd hsd
      split
      · rename_i hlen
        simp only [introVal]
        split
        · intro K tr; simpa using Steps.single (.mkStruct hsd hlen)
        · trivial
      · trivial

/-- (D-Enum-Intro) §6.6 after §6.2's search through the payload (helper). -/
theorem sim_mkEnum (IH : SimIH M P fuel) (e k : Nat) (args : List Expr) :
    Sim M P φ (evalConf H φ (.mkEnum e k args)) (eval M (fuel + 1) P H φ (.mkEnum e k args)) := by
  simp only [eval]
  obtain ⟨ihok, ihab⟩ := evalArgs_sim (φ := φ) IH (.enum e k) args H []
  have hent : ∀ K tr, Steps M P (evalConf H φ (.mkEnum e k args) K tr)
      (argsConf H φ (.enum e k) [] args K tr) := fun _ _ => Steps.single .enumEnter
  split
  · rename_i r hr; exact Sim.pre hent (ihab r hr)
  · rename_i H₁ vs tr₁ hr
    refine Sim.withTrace (C₂ := argsConf H₁ φ (.enum e k) vs [])
      (fun K tr => (hent K tr).trans (by simpa using ihok _ _ _ hr K tr)) ?_
    split
    · trivial
    · rename_i ed hed
      split
      · trivial
      · rename_i Ts hTs
        split
        · rename_i hlen
          simp only [introVal]
          split
          · intro K tr; simpa using Steps.single (.mkEnum hed hTs hlen)
          · trivial
        · trivial

/-- (D-Array) §6.5 after §6.2's search through the elements (helper). -/
theorem sim_mkArray (IH : SimIH M P fuel) (T : Ty) (args : List Expr) :
    Sim M P φ (evalConf H φ (.mkArray T args)) (eval M (fuel + 1) P H φ (.mkArray T args)) := by
  simp only [eval]
  obtain ⟨ihok, ihab⟩ := evalArgs_sim (φ := φ) IH (.array T) args H []
  have hent : ∀ K tr, Steps M P (evalConf H φ (.mkArray T args) K tr)
      (argsConf H φ (.array T) [] args K tr) := fun _ _ => Steps.single .arrayEnter
  split
  · rename_i r hr; exact Sim.pre hent (ihab r hr)
  · rename_i H₁ vs tr₁ hr
    refine Sim.withTrace (C₂ := argsConf H₁ φ (.array T) vs [])
      (fun K tr => (hent K tr).trans (by simpa using ihok _ _ _ hr K tr)) ?_
    simp only [introVal]
    split
    · intro K tr; simpa using Steps.single .mkArray
    · trivial

/-- The repeat form (`7.1:39`) (helper). -/
theorem sim_repeat (IH : SimIH M P fuel) (T : Ty) (e : Expr) (n : Nat) :
    Sim M P φ (evalConf H φ (.repeatArray T e n)) (eval M (fuel + 1) P H φ (.repeatArray T e n)) := by
  simp only [eval]
  refine Sim.andThen (F := .repeatArray T n) (fun _ => ⟨rfl, rfl⟩)
    (fun _ _ => Steps.single .repeatEnter) (IH H φ e) ?_
  intro H₁ v _ _
  split
  · rename_i hcopy
    simp only [introVal]
    split
    · intro K tr; simpa using Steps.single (.repeatArray hcopy)
    · trivial
  · trivial

/-- (D-Index)/(D-Index-Trap) §6.5 and (D-Use-Untrackable-Dynamic-Copy) §6.3,
from the index list's context (helper). -/
theorem sim_indexRead_args (IH : SimIH M P fuel) (p : Place) (idx : List Expr)
    (πs : List (List Nat)) :
    Sim M P φ (argsConf H φ (.indexRead p πs) [] idx)
      (eval M (fuel + 1) P H φ (.indexRead p idx πs)) := by
  simp only [eval]
  obtain ⟨ihok, ihab⟩ := evalArgs_sim (φ := φ) IH (.indexRead p πs) idx H []
  split
  · rename_i r hr; exact ihab r hr
  · rename_i H₁ vs tr₁ hr
    refine Sim.withTrace (C₂ := argsConf H₁ φ (.indexRead p πs) vs [])
      (fun K tr => by simpa using ihok _ _ _ hr K tr) ?_
    split
    · trivial
    · rename_i hb
      intro K tr; simpa using Steps.single (.indexReadTrap hb)
    · rename_i ℓ c sub ρ hd
      split
      · trivial
      · rename_i leaf hleaf
        split
        · trivial
        · rename_i v hv
          split
          · rename_i hcopy
            intro K tr; simpa using Steps.single (.indexRead hd hleaf hv hcopy)
          · trivial

/-- (D-Index) at an expression in focus (helper). -/
theorem sim_indexRead (IH : SimIH M P fuel) (p : Place) (idx : List Expr)
    (πs : List (List Nat)) :
    Sim M P φ (evalConf H φ (.indexRead p idx πs)) (eval M (fuel + 1) P H φ (.indexRead p idx πs)) :=
  Sim.pre (fun _ _ => Steps.single .indexReadEnter) (sim_indexRead_args IH p idx πs)

/-- §6.11's `@drop` at a `Copy` place below a dynamic index. `eval` runs it
as the read with its value discarded, at the same fuel, so the argument list
is at two less (helper). -/
theorem sim_indexDrop (IH : SimIH M P fuel) (p : Place) (idx : List Expr)
    (πs : List (List Nat)) :
    Sim M P φ (evalConf H φ (.indexDrop p idx πs)) (eval M (fuel + 2) P H φ (.indexDrop p idx πs)) := by
  simp only [eval]
  obtain ⟨ihok, ihab⟩ := evalArgs_sim (φ := φ) IH (.indexDrop p πs) idx H []
  have hent : ∀ K tr, Steps M P (evalConf H φ (.indexDrop p idx πs) K tr)
      (argsConf H φ (.indexDrop p πs) [] idx K tr) := fun _ _ => Steps.single .indexDropEnter
  split
  · rename_i r hr
    have hne := evalArgs_abort_ne_ok hr
    have : r.andThen (fun H' _ => .ok H' .unit []) = r := by
      cases r <;> simp_all [EvalRes.andThen]
    rw [this]
    exact Sim.pre hent (ihab r hr)
  · rename_i H₁ vs tr₁ hr
    rw [EvalRes.withTrace_andThen]
    refine Sim.withTrace (C₂ := argsConf H₁ φ (.indexDrop p πs) vs [])
      (fun K tr => (hent K tr).trans (by simpa using ihok _ _ _ hr K tr)) ?_
    split
    · trivial
    · rename_i hb
      intro K tr; simpa [EvalRes.andThen] using Steps.single (.indexDropTrap hb)
    · rename_i ℓ c sub ρ hd
      split
      · trivial
      · rename_i leaf hleaf
        split
        · trivial
        · rename_i v hv
          split
          · rename_i hcopy
            rw [← Contents.mult_toVal _ _ _ hv] at hcopy
            intro K tr; simpa [EvalRes.andThen] using Steps.single (.indexDrop hd hleaf hv hcopy)
          · trivial

/-- (D-Assign) §6.8 below a dynamic index, in `5.2:14`'s order (helper). -/
theorem sim_indexWrite (IH : SimIH M P fuel) (p : Place) (idx : List Expr)
    (πs : List (List Nat)) (e : Expr) :
    Sim M P φ (evalConf H φ (.indexWrite p idx πs e))
      (eval M (fuel + 1) P H φ (.indexWrite p idx πs e)) := by
  simp only [eval]
  refine Sim.andThen (F := .indexWriteRhs p idx πs) (fun _ => ⟨rfl, rfl⟩)
    (fun _ _ => Steps.single .indexWriteEnter) (IH H φ e) ?_
  intro H₁ v _ _
  obtain ⟨ihok, ihab⟩ := evalArgs_sim (φ := φ) IH (.indexWrite p πs v) idx H₁ []
  have hent : ∀ K tr, Steps M P (.run H₁ φ (.indexWriteRhs p idx πs :: K) (.ret v) tr)
      (argsConf H₁ φ (.indexWrite p πs v) [] idx K tr) := fun _ _ => Steps.single .indexWriteRhs
  split
  · rename_i r hr; exact Sim.pre hent (ihab r hr)
  · rename_i H₂ vs tr₂ hr
    refine Sim.withTrace (C₂ := argsConf H₂ φ (.indexWrite p πs v) vs [])
      (fun K tr => (hent K tr).trans (by simpa using ihok _ _ _ hr K tr)) ?_
    split
    · trivial
    · rename_i hb
      intro K tr; simpa using Steps.single (.indexWriteTrap hb)
    · rename_i ℓ c sub ρ hd
      split
      · trivial
      · rename_i old hold
        split
        · trivial
        · split
          · trivial
          · rename_i evs hdrop
            split
            · trivial
            · rename_i sub' hw₁
              split
              · trivial
              · rename_i c' hw₂
                split
                · intro K tr; exact Steps.single (.indexWrite hd hold hdrop hw₁ hw₂)
                · trivial

/-- (D-Match) §6.6: the arm runs under its `endscope`, which (D-EndScope)
closes (helper). -/
theorem sim_match (IH : SimIH M P fuel) (scrut : Expr) (arms : List Expr) :
    Sim M P φ (evalConf H φ (.«match» scrut arms)) (eval M (fuel + 1) P H φ (.«match» scrut arms)) := by
  simp only [eval]
  refine Sim.andThen (F := .«match» arms) (fun _ => ⟨rfl, rfl⟩)
    (fun _ _ => Steps.single .matchEnter) (IH H φ scrut) ?_
  intro H₀ v _ _hv
  split
  · rename_i en k i vs
    split
    · trivial
    · rename_i body hbody
      refine Sim.andThen (F := .endscope (mintParams H₀ vs).2) (fun _ => ⟨rfl, rfl⟩)
        (fun _ _ => Steps.single (.«match» hbody rfl)) (IH _ _ body) ?_
      intro H₂ v₂ _ _
      split
      · trivial
      · rename_i H₃ evs hu
        intro K tr
        have := Steps.single (M := M) (P := P) (.endScope (K := K) (tr := tr) (v := v₂)
          (φ := { env := (mintParams H₀ vs).2.reverse ++ φ.env,
                  scope := φ.scope ++ (mintParams H₀ vs).2 }) (unwindLocs_plain hu))
        rwa [Frame.popScope_push] at this
  · trivial

/-- (D-Let) §6.7, then (D-EndScope) (helper). -/
theorem sim_letIn (IH : SimIH M P fuel) (m : Bool) (e₁ e₂ : Expr) :
    Sim M P φ (evalConf H φ (.letIn m e₁ e₂)) (eval M (fuel + 1) P H φ (.letIn m e₁ e₂)) := by
  simp only [eval]
  refine Sim.andThen (F := .letIn e₂) (fun _ => ⟨rfl, rfl⟩)
    (fun _ _ => Steps.single .letEnter) (IH H φ e₁) ?_
  intro H₁ v₁ _ _
  refine Sim.andThen (F := .endscope [H₁.length]) (fun _ => ⟨rfl, rfl⟩)
    (fun _ _ => Steps.single .letBind) (IH _ _ e₂) ?_
  intro H₂ v₂ _ _
  split
  · trivial
  · rename_i H₃ evs hd
    intro K tr
    have := Steps.single (M := M) (P := P) (.endScope (K := K) (tr := tr) (v := v₂)
      (φ := { env := H₁.length :: φ.env, scope := φ.scope ++ [H₁.length] })
      (ℓs := [H₁.length]) (plainUnwind_single hd))
    simpa [Frame.popScope_let] using this

/-- (D-Assign) §6.8 (helper). -/
theorem sim_assign (IH : SimIH M P fuel) (p : Place) (e : Expr) :
    Sim M P φ (evalConf H φ (.assign p e)) (eval M (fuel + 1) P H φ (.assign p e)) := by
  simp only [eval]
  refine Sim.andThen (F := .assign p) (fun _ => ⟨rfl, rfl⟩)
    (fun _ _ => Steps.single .assignEnter) (IH H φ e) ?_
  intro H₁ v _ _
  split
  · trivial
  · rename_i ℓ hℓ
    split
    · trivial
    · trivial
    · rename_i c hc
      split
      · trivial
      · rename_i old hold
        split
        · trivial
        · split
          · trivial
          · rename_i evs hdrop
            split
            · trivial
            · rename_i c' hw
              split
              · intro K tr; exact Steps.single (.assign (rootCell_of hℓ hc) hold hdrop hw)
              · trivial

/-- (D-Seq) §6.7 (helper). -/
theorem sim_seq (IH : SimIH M P fuel) (e₁ e₂ : Expr) :
    Sim M P φ (evalConf H φ (.seq e₁ e₂)) (eval M (fuel + 1) P H φ (.seq e₁ e₂)) := by
  simp only [eval]
  refine Sim.andThen (F := .seq e₂) (fun _ => ⟨rfl, rfl⟩)
    (fun _ _ => Steps.single .seqEnter) (IH H φ e₁) ?_
  intro H₁ v₁ _ _
  split
  · trivial
  · rename_i hm
    split
    · trivial
    · rename_i evs hd
      have hne : v₁.mult P.decls ≠ .copy := by rw [hm]; exact nofun
      exact Sim.withTrace (C₂ := evalConf H₁ φ e₂) (fun _ _ => Steps.single (.seqDrop hne hd))
        (IH H₁ φ e₂)
  · rename_i hm
    exact Sim.pre (fun _ _ => Steps.single (.seqCopy hm)) (IH H₁ φ e₂)

/-- (D-If-T)/(D-If-F) §6.6 (helper). -/
theorem sim_ite (IH : SimIH M P fuel) (c e₁ e₂ : Expr) :
    Sim M P φ (evalConf H φ (.ite c e₁ e₂)) (eval M (fuel + 1) P H φ (.ite c e₁ e₂)) := by
  simp only [eval]
  refine Sim.andThen (F := .ite e₁ e₂) (fun _ => ⟨rfl, rfl⟩)
    (fun _ _ => Steps.single .iteEnter) (IH H φ c) ?_
  intro H₀ v _ _hv
  split
  · rename_i b
    split
    · rename_i hb
      subst hb
      exact Sim.pre (fun _ _ => Steps.single .iteTrue) (IH H₀ φ e₁)
    · rename_i hb
      simp only [Bool.not_eq_true] at hb
      subst hb
      exact Sim.pre (fun _ _ => Steps.single .iteFalse) (IH H₀ φ e₂)
  · trivial

/-- (D-Call) §6.9, then (D-Return-Value), or (D-Return)'s value caught at the
`call` frame (helper). -/
theorem sim_call (IH : SimIH M P fuel) (f : Nat) (args : List Expr) :
    Sim M P φ (evalConf H φ (.call f args)) (eval M (fuel + 1) P H φ (.call f args)) := by
  simp only [eval]
  obtain ⟨ihok, ihab⟩ := evalArgs_sim (φ := φ) IH (.call f) args H []
  have hent : ∀ K tr, Steps M P (evalConf H φ (.call f args) K tr)
      (argsConf H φ (.call f) [] args K tr) := fun _ _ => Steps.single .callEnter
  split
  · rename_i r hr; exact Sim.pre hent (ihab r hr)
  · rename_i H₁ vs tr₁ hr
    refine Sim.withTrace (C₂ := argsConf H₁ φ (.call f) vs [])
      (fun K tr => (hent K tr).trans (by simpa using ihok _ _ _ hr K tr)) ?_
    split
    · trivial
    · rename_i fd hfd
      split
      · rename_i hlen
        refine Sim.absorb (fun _ _ => Steps.single (.call hfd hlen rfl)) (IH _ _ fd.body) ?_
        intro H₃ v _ _
        split
        · trivial
        · rename_i H₄ evs hu
          intro K tr
          exact Steps.single (.callReturn (unwindLocs_plain hu))
      · trivial

/-- (D-Return) §6.9 (helper). -/
theorem sim_ret (IH : SimIH M P fuel) (e : Expr) :
    Sim M P φ (evalConf H φ (.ret e)) (eval M (fuel + 1) P H φ (.ret e)) := by
  simp only [eval]
  refine Sim.andThen (F := .ret) (fun _ => ⟨rfl, rfl⟩)
    (fun _ _ => Steps.single .retEnter) (IH H φ e) ?_
  intro H₁ v _ _
  split
  · trivial
  · rename_i H₂ evs hu
    intro K tr φs K' hK
    exact Steps.single (.ret hK (unwindLocs_plain hu))

/-- (D-Break) §6.10 (helper). -/
theorem sim_brk : Sim M P φ (evalConf H φ .brk) (eval M (fuel + 1) P H φ .brk) := by
  simp only [eval]
  intro K tr φs K' H' evs hK hu
  simpa using Steps.single (.brk hK hu)

/-- (D-Loop-Enter), (D-Loop-Iter) and (D-Break)'s landing §6.10. A turn that
finishes re-enters the body; `eval` re-evaluates the loop at one less fuel,
whose simulation starts one (D-Loop-Enter) earlier, peeled off by
determinism (helper). -/
theorem sim_loop (IH : SimIH M P fuel) (e : Expr) :
    Sim M P φ (evalConf H φ (.loop e)) (eval M (fuel + 1) P H φ (.loop e)) := by
  simp only [eval]
  have hent : ∀ K tr, Steps M P (evalConf H φ (.loop e) K tr) (evalConf H φ e (.loop e φ :: K) tr) :=
    fun _ _ => Steps.single .loopEnter
  have h₁ := IH H φ e
  cases hr : eval M fuel P H φ e with
  | ok H₁ v tr₁ =>
      rw [hr] at h₁
      simp only
      have hpeel : Sim M P φ (evalConf H₁ φ e ∘ (Kont.loop e φ :: ·))
          (eval M fuel P H₁ φ (.loop e)) :=
        Sim.peel (fun _ _ => .loopEnter) (fun _ _ => trivial) (IH H₁ φ (.loop e))
      refine Sim.withTrace (C₂ := evalConf H₁ φ e ∘ (Kont.loop e φ :: ·)) (fun K tr => ?_) hpeel
      have hit : Step M P (.run H₁ φ (.loop e φ :: K) (.ret v) (tr ++ tr₁))
          (.run H₁ φ (.loop e φ :: K) (.eval e) (tr ++ tr₁ ++ [])) :=
        .loopIter (by simp [plainUnwind])
      simp only [List.append_nil] at hit
      exact (hent K tr).trans ((h₁ _ tr).trans (Steps.single hit))
  | broke H₁ sc tr₁ =>
      rw [hr] at h₁
      simp only
      split
      · trivial
      · rename_i H₂ evs hu
        intro K tr
        have := h₁ (.loop e φ :: K) tr φ K H₂ evs rfl (unwindLocs_plain hu)
        simp only [List.append_assoc] at this ⊢
        exact (hent K tr).trans this
  | returned H₁ v tr₁ =>
      rw [hr] at h₁
      simp only [Sim] at h₁ ⊢
      intro K tr φs K' hK
      exact (hent K tr).trans (h₁ (.loop e φ :: K) tr φs K' hK)
  | panic κ tr₁ =>
      rw [hr] at h₁
      simp only [Sim] at h₁ ⊢
      intro K tr
      exact (hent K tr).trans (h₁ (.loop e φ :: K) tr)
  | stuck w => trivial
  | outOfFuel => trivial

end forms

/-! ## Soundness of `eval` with respect to `Step` -/

/-- **`eval` is simulated by §6's `→*`** (RUE-2289 part 2, ADR-0097 decision
3), for every expression, store, frame and fuel, on every program: a value, a
panic, an unwinding `return` and an unwinding `break` are each reached by
`Step` from the expression in focus under any context, as `Sim` reads them
(§6.2's (Search) and (Panic-Lift), (D-Return) §6.9, (D-Break) §6.10). The
proof is a strong induction on fuel with one lemma per form. -/
theorem eval_sim (M : FloatOps) (P : Program) (fuel : Nat) : SimIH M P fuel := by
  induction fuel using Nat.strongRecOn with
  | ind n ih =>
  intro H φ e
  cases n with
  | zero => simp [eval, Sim]
  | succ fuel =>
    have IH := ih fuel (Nat.lt_succ_self _)
    cases e with
    | intLit w s n => intro K tr; simpa [eval] using Steps.single .intLit
    | floatLit w l => intro K tr; simpa [eval] using Steps.single .floatLit
    | boolLit b => intro K tr; simpa [eval] using Steps.single .boolLit
    | unitLit => intro K tr; simpa [eval] using Steps.single .unitLit
    | use p => exact sim_use p
    | binop op e₁ e₂ => exact sim_binop IH op e₁ e₂
    | unop op e => exact sim_unop IH op e
    | intCast w s e => exact sim_intCast IH w s e
    | fintrin k e => exact sim_fintrin IH k e
    | panic msg => intro K tr; simpa [eval] using Steps.single .panic
    | dbg e => exact sim_dbg IH e
    | mkStruct s args => exact sim_mkStruct IH s args
    | mkEnum e k args => exact sim_mkEnum IH e k args
    | «match» scrut arms => exact sim_match IH scrut arms
    | mkArray T args => exact sim_mkArray IH T args
    | repeatArray T e n => exact sim_repeat IH T e n
    | indexRead p idx πs => exact sim_indexRead IH p idx πs
    | indexWrite p idx πs e => exact sim_indexWrite IH p idx πs e
    | indexDrop p idx πs =>
        cases fuel with
        | zero => simp [eval, EvalRes.andThen, Sim]
        | succ f => exact sim_indexDrop (ih f (by omega)) p idx πs
    | drop p => exact sim_drop p
    | letIn m e₁ e₂ => exact sim_letIn IH m e₁ e₂
    | assign p e => exact sim_assign IH p e
    | seq e₁ e₂ => exact sim_seq IH e₁ e₂
    | ite c e₁ e₂ => exact sim_ite IH c e₁ e₂
    | call f args => exact sim_call IH f args
    | ret e => exact sim_ret IH e
    | loop e => exact sim_loop IH e
    | brk => exact sim_brk

/-- The empty frame the entry point is called from (helper). -/
abbrev Frame.empty : Frame := { env := [], scope := [] }

/-- **`run` is simulated by `→*` from §6.12's initial configuration**, on
every program: a value `run` returns is a terminal configuration `✓` that
`Config.init` reaches with the same store and trace ((D-Return-Main) §6.9,
(Result-Ok) §6.12), and a panic is `↯κ` after the same trace ((Result-Panic)
§6.12). No typing hypothesis: every place `eval` and `Step` differ is a
refusal on `eval`'s side. -/
theorem run_sim (M : FloatOps) (P : Program) (fuel : Nat) :
    (∀ H v tr, run M P fuel = .ok H v tr →
      Steps M P Config.init (.run H Frame.empty [] (.ret v) tr)) ∧
    (∀ k tr, run M P fuel = .panic k tr → Steps M P Config.init (.panic k tr)) := by
  have h := eval_sim M P fuel [] Frame.empty (.call 0 [])
  refine ⟨fun H v tr hr => ?_, fun k tr hr => ?_⟩
  · simp only [run] at hr
    rw [hr] at h
    simpa [Config.init] using h [] []
  · simp only [run] at hr
    rw [hr] at h
    simpa [Config.init] using h [] []

/-- **`eval` is sound with respect to §6's reduction** (RUE-2289 part 2;
ADR-0097 decision 3: "a theorem about `eval` is a theorem about §6 only once
the two are proved to agree"). For a program `check` accepts
(`ProgramTyped`, RUE-2314's domain), `run` is never stuck (`no_violation`),
so it answers a value, a panic or `outOfFuel`; a value is reached by §6's
`→*` from the initial configuration as a terminal configuration with the same
store and trace, and a panic as `↯κ` after the same trace (§6.2, §6.12).
`.stuck` is outside the correspondence and does not occur here; `outOfFuel`
is not a state of §6's machine. The converse, completeness modulo fuel, is
`eval_complete`. -/
theorem eval_sound (M : FloatModel) {P : Program} (h : ProgramTyped P) (fuel : Nat) :
    (∀ w, run M.toFloatOps P fuel ≠ .stuck w) ∧
    (∀ H v tr, run M.toFloatOps P fuel = .ok H v tr →
      Steps M.toFloatOps P Config.init (.run H Frame.empty [] (.ret v) tr)) ∧
    (∀ k tr, run M.toFloatOps P fuel = .panic k tr →
      Steps M.toFloatOps P Config.init (.panic k tr)) :=
  ⟨no_violation M h fuel, (run_sim M.toFloatOps P fuel).1, (run_sim M.toFloatOps P fuel).2⟩


/-! ## Counted runs -/

/-- `→ⁿ`: a run of exactly `n` steps of §6's reduction (helper). Completeness
counts steps, because fuel is a bound on them. -/
inductive StepsN (M : FloatOps) (P : Program) : Nat → Config → Config → Prop where
  | refl (C : Config) : StepsN M P 0 C C
  | step {n : Nat} {C₁ C₂ C₃ : Config} :
      Step M P C₁ C₂ → StepsN M P n C₂ C₃ → StepsN M P (n + 1) C₁ C₃

section counted
variable {M : FloatOps} {P : Program}

/-- A counted run is a run (§6.12's `→*`) (helper). -/
theorem StepsN.toSteps {n : Nat} {C D : Config} (h : StepsN M P n C D) : Steps M P C D := by
  induction h with
  | refl => exact .refl _
  | step s _ ih => exact .step s ih

/-- Every run has a length (helper). -/
theorem Steps.toN {C D : Config} (h : Steps M P C D) : ∃ n, StepsN M P n C D := by
  induction h with
  | refl C => exact ⟨0, .refl C⟩
  | step s _ ih => obtain ⟨n, h⟩ := ih; exact ⟨n + 1, .step s h⟩

/-- Counted runs compose (helper). -/
theorem StepsN.trans {a b : Nat} {C E D : Config} (h₁ : StepsN M P a C E) (h₂ : StepsN M P b E D) :
    StepsN M P (a + b) C D := by
  induction h₁ with
  | refl => simpa using h₂
  | step s _ ih => rw [Nat.add_right_comm]; exact .step s (ih h₂)

/-- A run of `n` steps has a run of every shorter length from the same start
(helper). -/
theorem StepsN.prefix {n : Nat} {C D : Config} (h : StepsN M P n C D) :
    ∀ {m : Nat}, m ≤ n → ∃ E, StepsN M P m C E := by
  induction h with
  | refl C => intro m hm; exact ⟨C, by rw [Nat.le_zero.mp hm]; exact .refl C⟩
  | @step n C₁ C₂ C₃ s _ ih =>
      intro m hm
      cases m with
      | zero => exact ⟨C₁, .refl C₁⟩
      | succ m =>
          obtain ⟨E, hE⟩ := ih (Nat.le_of_succ_le_succ hm)
          exact ⟨E, .step s hE⟩

/-- **Determinism, counted** (`Step.det`, §6): two runs of the same length
from one configuration end at the same configuration (helper). -/
theorem StepsN.det {n : Nat} {C D D' : Config} (h : StepsN M P n C D) (h' : StepsN M P n C D') :
    D = D' := by
  induction h generalizing D' with
  | refl => cases h'; rfl
  | step s _ ih =>
      cases h' with
      | step s' rest' => rw [Step.det s s'] at *; exact ih rest'

/-- Peeling a counted run's first step by determinism (helper). -/
theorem StepsN.peel {n : Nat} {C C' D : Config} (hs : Step M P C C') (h : StepsN M P (n + 1) C D) :
    StepsN M P n C' D := by
  cases h with
  | step s rest => rw [Step.det hs s]; exact rest

/-- **A configuration that takes no step bounds every run** through it (§6,
by `Step.det`): if `→ᵏ` reaches a configuration with no successor — terminal
or stuck — no run from the same start is longer than `k` (helper). -/
theorem StepsN.bound {k : Nat} {C T : Config} (hT : StepsN M P k C T) (hfin : ∀ C', ¬ Step M P T C') :
    ∀ {n : Nat} {D : Config}, StepsN M P n C D → n ≤ k := by
  induction hT with
  | refl =>
      intro n D h
      cases h with
      | refl => exact Nat.le_refl 0
      | step s _ => exact absurd s (hfin _)
  | step s _ ih =>
      intro n D h
      cases h with
      | refl => exact Nat.zero_le _
      | step s' rest' =>
          rw [Step.det s s'] at ih
          exact Nat.succ_le_succ (ih hfin rest')

/-- **One end per start** (§6, by `Step.det`): two configurations with no
successor reached from the same configuration are the same one (helper). -/
theorem Steps.final_unique {C T₁ T₂ : Config} (h₁ : Steps M P C T₁) (h₂ : Steps M P C T₂)
    (hf₁ : ∀ C', ¬ Step M P T₁ C') (hf₂ : ∀ C', ¬ Step M P T₂ C') : T₁ = T₂ := by
  induction h₁ with
  | refl => cases h₂ with
    | refl => rfl
    | step s _ => exact absurd s (hf₁ _)
  | step s _ ih =>
      cases h₂ with
      | refl => exact absurd s (hf₂ _)
      | step s' rest' => rw [Step.det s s'] at ih; exact ih rest' hf₁

/-! ## Fuel counts steps -/

/-- A run of `n` steps from every member of a configuration family: from
`⟨H ; φ ; K ; E[e]⟩`, for every context `K` and trace `tr` (helper). -/
def Long (M : FloatOps) (P : Program) (C : List Kont → List Event → Config) (n : Nat) : Prop :=
  ∀ K tr, ∃ D, StepsN M P n (C K tr) D

/-- A family with long runs has shorter ones (helper). -/
theorem Long.mono {C : List Kont → List Event → Config} {m n : Nat} (hmn : m ≤ n)
    (h : Long M P C n) : Long M P C m := by
  intro K tr
  obtain ⟨D, hD⟩ := h K tr
  exact hD.prefix hmn

/-- A run into a family with long runs is at least as long (helper). -/
theorem Long.pre {C C₂ : List Kont → List Event → Config} {n : Nat}
    (hpre : ∀ K tr, ∃ tr', Steps M P (C K tr) (C₂ K tr')) (h : Long M P C₂ n) : Long M P C n := by
  intro K tr
  obtain ⟨tr', hs⟩ := hpre K tr
  obtain ⟨m, hm⟩ := hs.toN
  obtain ⟨D, hD⟩ := h K tr'
  exact (hm.trans hD).prefix (Nat.le_add_left n m)

/-- A step and then a run into a family with long runs is one step longer
(helper). -/
theorem Long.pre1 {C C₂ : List Kont → List Event → Config} {n : Nat}
    (hpre : ∀ K tr, ∃ C' tr', Step M P (C K tr) C' ∧ Steps M P C' (C₂ K tr'))
    (h : Long M P C₂ n) : Long M P C (n + 1) := by
  intro K tr
  obtain ⟨C', tr', s, hs⟩ := hpre K tr
  obtain ⟨m, hm⟩ := hs.toN
  obtain ⟨D, hD⟩ := h K tr'
  have := StepsN.step s (hm.trans hD)
  exact this.prefix (by omega)

/-- **§6.2's (Search), counted**: the twin of `Sim.andThen` for exhausted
fuel. If `eval` spent its fuel on the operand, the operand's run under the
pushed frame `F` is the long one, one enter step in; if the operand reached a
value (`Sim`'s `ok` clause gives the run to it) and the context spent the fuel,
the context's run is (helper). -/
theorem Long.andThen {φ₁ : Frame} {C C₁ : List Kont → List Event → Config} {F : Kont} {fuel : Nat}
    (hC : ∀ K tr, Step M P (C K tr) (C₁ (F :: K) tr))
    {r : EvalRes} (hsim : Sim M P φ₁ C₁ r) (h₁ : r = .outOfFuel → Long M P C₁ fuel)
    {k : Store → Val → EvalRes}
    (hk : ∀ H₁ v tr₁, r = .ok H₁ v tr₁ → k H₁ v = .outOfFuel →
      Long M P (fun K tr => .run H₁ φ₁ (F :: K) (.ret v) tr) fuel) :
    r.andThen k = .outOfFuel → Long M P C (fuel + 1) := by
  intro hr
  cases r with
  | ok H₁ v tr₁ =>
      simp only [EvalRes.andThen, EvalRes.withTrace_outOfFuel_iff] at hr
      exact Long.pre1 (fun K tr => ⟨_, tr ++ tr₁, hC K tr, hsim (F :: K) tr⟩) (hk H₁ v tr₁ rfl hr)
  | outOfFuel =>
      exact Long.pre1 (C₂ := fun K tr => C₁ (F :: K) tr)
        (fun K tr => ⟨_, tr, hC K tr, .refl _⟩) (fun K tr => h₁ rfl (F :: K) tr)
  | _ => simp [EvalRes.andThen] at hr

/-- The induction hypothesis: at fuel `fuel`, exhaustion is a run of `fuel`
steps (helper). -/
def LongIH (M : FloatOps) (P : Program) (fuel : Nat) : Prop :=
  ∀ H φ e, eval M fuel P H φ e = .outOfFuel → Long M P (evalConf H φ e) fuel

/-- **Argument lists, counted** (§6.2's `…( v̄, E, ē )`): a list that spent
its fuel on an element has a run one step longer than the element's fuel, the
extra step being the (Search) push into the element's hole (helper). -/
theorem evalArgs_long {fuel : Nat} {φ : Frame} (IH : LongIH M P fuel) (t : ArgsTag) :
    ∀ (es : List Expr) (H : Store) (vs₀ : List Val),
    evalArgs (fun H e => eval M fuel P H φ e) H es = .abort .outOfFuel →
      Long M P (argsConf H φ t vs₀ es) (fuel + 1)
  | [], H, vs₀, h => by simp [evalArgs] at h
  | e :: es, H, vs₀, h => by
      have hpush : ∀ K tr, Step M P (argsConf H φ t vs₀ (e :: es) K tr)
          (evalConf H φ e (.args t vs₀ es :: K) tr) := fun K tr => .argsPush
      cases he : eval M fuel P H φ e with
      | ok H₁ v tr₁ =>
          simp only [evalArgs, he] at h
          split at h
          · simp at h
          · rename_i r' h₂
            simp only [ArgsRes.abort.injEq, EvalRes.withTrace_outOfFuel_iff] at h
            subst h
            have ih := evalArgs_long IH t es H₁ (vs₀ ++ [v]) h₂
            have hs := eval_sim M P fuel H φ e
            rw [he] at hs
            refine Long.pre (fun K tr => ⟨tr ++ tr₁, ?_⟩) ih
            exact .step (hpush K tr) ((hs _ tr).trans (Steps.single .argsPlug))
      | outOfFuel =>
          exact Long.pre1 (C₂ := fun K tr => evalConf H φ e (.args t vs₀ es :: K) tr)
            (fun K tr => ⟨_, tr, hpush K tr, .refl _⟩) (fun K tr => IH H φ e he _ tr)
      | _ => simp [evalArgs, he] at h

/-- `evalArgs` finishes as `eval_sim` says, from the argument list (helper). -/
theorem evalArgs_ok_steps {fuel : Nat} {φ : Frame} {t : ArgsTag} {es : List Expr} {H H' : Store}
    {vs : List Val} {tr' : List Event}
    (h : evalArgs (fun H e => eval M fuel P H φ e) H es = .ok H' vs tr') :
    ∀ K tr, Steps M P (argsConf H φ t [] es K tr) (.run H' φ K (.args t vs []) (tr ++ tr')) := by
  intro K tr
  simpa using (evalArgs_sim (eval_sim M P fuel) t es H []).1 _ _ _ h K tr

end counted

/-! ## Fuel counts steps, per form -/

section longForms
variable {M : FloatOps} {P : Program} {fuel : Nat} {H : Store} {φ : Frame}

/-- Every family has runs of no steps (helper). -/
theorem Long.zero {C : List Kont → List Event → Config} : Long M P C 0 :=
  fun _ _ => ⟨_, .refl _⟩

/-- An operator's outcome is never exhausted fuel (helper). -/
theorem OpRes.toRes_ne_outOfFuel (o : OpRes) (H : Store) : o.toRes H ≠ .outOfFuel := by
  cases o <;> simp [OpRes.toRes]

/-- Aggregate introduction is never exhausted fuel (helper). -/
theorem introVal_ne_outOfFuel {D : Decls} {H : Store} {mk : Nat → Val} :
    introVal D H mk ≠ .outOfFuel := by
  simp only [introVal]; split <;> simp

/-- Close a context `k H v = .outOfFuel` whose context never spends fuel. -/
macro "never_oof" : tactic => `(tactic| (
  intro _ _ _ _ h
  first
  | exact absurd h (OpRes.toRes_ne_outOfFuel _ _)
  | exact absurd h introVal_ne_outOfFuel
  | (try simp only [] at h
     (repeat' split at h) <;> first
       | simp at h
       | exact absurd h (OpRes.toRes_ne_outOfFuel _ _)
       | exact absurd h introVal_ne_outOfFuel)))

/-- The place forms, literals, `@panic` and `break` spend no fuel of their own
beyond the unit they start with (helper). -/
theorem eval_leaf_ne_outOfFuel {e : Expr}
    (he : match e with
      | .intLit .. | .floatLit .. | .boolLit _ | .unitLit | .use _ | .drop _ | .panic _ | .brk => True
      | _ => False) :
    eval M (fuel + 1) P H φ e ≠ .outOfFuel := by
  cases e <;> simp only at he <;> simp only [eval]
  all_goals (repeat' split)
  all_goals simp

/-- §6.4's binary operators, counted (helper). -/
theorem long_binop (IH : LongIH M P fuel) (op : BinOp) (e₁ e₂ : Expr) :
    eval M (fuel + 1) P H φ (.binop op e₁ e₂) = .outOfFuel →
      Long M P (evalConf H φ (.binop op e₁ e₂)) (fuel + 1) := by
  simp only [eval]
  refine Long.andThen (C₁ := evalConf H φ e₁) (F := .binopL op e₂) (fun _ _ => .binopEnter)
    (eval_sim M P fuel H φ e₁) (IH H φ e₁) ?_
  intro H₁ v₁ _ _ hk
  refine Long.mono (Nat.le_succ _) (Long.andThen (C₁ := evalConf H₁ φ e₂) (F := .binopR op v₁)
    (fun _ _ => .binopMid) (eval_sim M P fuel H₁ φ e₂) (IH H₁ φ e₂) ?_ hk)
  never_oof

/-- §6.4's unary operators, counted (helper). -/
theorem long_unop (IH : LongIH M P fuel) (op : UnOp) (e : Expr) :
    eval M (fuel + 1) P H φ (.unop op e) = .outOfFuel →
      Long M P (evalConf H φ (.unop op e)) (fuel + 1) := by
  simp only [eval]
  refine Long.andThen (C₁ := evalConf H φ e) (F := .unop op) (fun _ _ => .unopEnter)
    (eval_sim M P fuel H φ e) (IH H φ e) ?_
  never_oof

/-- (D-Int-Cast), counted (helper). -/
theorem long_intCast (IH : LongIH M P fuel) (w : IntWidth) (sg : Sign) (e : Expr) :
    eval M (fuel + 1) P H φ (.intCast w sg e) = .outOfFuel →
      Long M P (evalConf H φ (.intCast w sg e)) (fuel + 1) := by
  simp only [eval]
  refine Long.andThen (C₁ := evalConf H φ e) (F := .intCast w sg) (fun _ _ => .intCastEnter)
    (eval_sim M P fuel H φ e) (IH H φ e) ?_
  never_oof

/-- §6.4's float intrinsics, counted (helper). -/
theorem long_fintrin (IH : LongIH M P fuel) (k : FloatIntrin) (e : Expr) :
    eval M (fuel + 1) P H φ (.fintrin k e) = .outOfFuel →
      Long M P (evalConf H φ (.fintrin k e)) (fuel + 1) := by
  simp only [eval]
  refine Long.andThen (C₁ := evalConf H φ e) (F := .fintrin k) (fun _ _ => .fintrinEnter)
    (eval_sim M P fuel H φ e) (IH H φ e) ?_
  never_oof

/-- `@dbg` (§6.12), counted (helper). -/
theorem long_dbg (IH : LongIH M P fuel) (e : Expr) :
    eval M (fuel + 1) P H φ (.dbg e) = .outOfFuel →
      Long M P (evalConf H φ (.dbg e)) (fuel + 1) := by
  simp only [eval]
  refine Long.andThen (C₁ := evalConf H φ e) (F := .dbg) (fun _ _ => .dbgEnter)
    (eval_sim M P fuel H φ e) (IH H φ e) ?_
  never_oof

/-- The repeat form (`7.1:39`), counted (helper). -/
theorem long_repeat (IH : LongIH M P fuel) (T : Ty) (e : Expr) (n : Nat) :
    eval M (fuel + 1) P H φ (.repeatArray T e n) = .outOfFuel →
      Long M P (evalConf H φ (.repeatArray T e n)) (fuel + 1) := by
  simp only [eval]
  refine Long.andThen (C₁ := evalConf H φ e) (F := .repeatArray T n) (fun _ _ => .repeatEnter)
    (eval_sim M P fuel H φ e) (IH H φ e) ?_
  never_oof

/-- (D-Return) §6.9, counted (helper). -/
theorem long_ret (IH : LongIH M P fuel) (e : Expr) :
    eval M (fuel + 1) P H φ (.ret e) = .outOfFuel →
      Long M P (evalConf H φ (.ret e)) (fuel + 1) := by
  simp only [eval]
  refine Long.andThen (C₁ := evalConf H φ e) (F := .ret) (fun _ _ => .retEnter)
    (eval_sim M P fuel H φ e) (IH H φ e) ?_
  never_oof

/-- (D-Assign) §6.8, counted (helper). -/
theorem long_assign (IH : LongIH M P fuel) (p : Place) (e : Expr) :
    eval M (fuel + 1) P H φ (.assign p e) = .outOfFuel →
      Long M P (evalConf H φ (.assign p e)) (fuel + 1) := by
  simp only [eval]
  refine Long.andThen (C₁ := evalConf H φ e) (F := .assign p) (fun _ _ => .assignEnter)
    (eval_sim M P fuel H φ e) (IH H φ e) ?_
  never_oof

/-- (D-Let) §6.7, counted: the body runs after (D-Let)'s step (helper). -/
theorem long_letIn (IH : LongIH M P fuel) (m : Bool) (e₁ e₂ : Expr) :
    eval M (fuel + 1) P H φ (.letIn m e₁ e₂) = .outOfFuel →
      Long M P (evalConf H φ (.letIn m e₁ e₂)) (fuel + 1) := by
  simp only [eval]
  refine Long.andThen (C₁ := evalConf H φ e₁) (F := .letIn e₂) (fun _ _ => .letEnter)
    (eval_sim M P fuel H φ e₁) (IH H φ e₁) ?_
  intro H₁ v₁ _ _ hk
  refine Long.mono (Nat.le_succ _) (Long.andThen (F := .endscope [H₁.length])
    (fun _ _ => .letBind) (eval_sim M P fuel _ _ e₂) (IH _ _ e₂) ?_ hk)
  never_oof

/-- (D-Match) §6.6, counted: the arm runs after (D-Match)'s step (helper). -/
theorem long_match (IH : LongIH M P fuel) (scrut : Expr) (arms : List Expr) :
    eval M (fuel + 1) P H φ (.«match» scrut arms) = .outOfFuel →
      Long M P (evalConf H φ (.«match» scrut arms)) (fuel + 1) := by
  simp only [eval]
  refine Long.andThen (C₁ := evalConf H φ scrut) (F := .«match» arms) (fun _ _ => .matchEnter)
    (eval_sim M P fuel H φ scrut) (IH H φ scrut) ?_
  intro H₀ v _ _ hk
  try simp only [] at hk
  split at hk
  · rename_i _ _ _ vs _
    split at hk
    · simp at hk
    · rename_i body hbody
      refine Long.mono (Nat.le_succ _) (Long.andThen (F := .endscope (mintParams H₀ vs).2)
        (fun _ _ => .«match» hbody rfl) (eval_sim M P fuel _ _ body) (IH _ _ body) ?_ hk)
      never_oof
  · simp at hk

/-- (D-Seq) §6.7, counted: the second operand runs after (D-Seq)'s step
(helper). -/
theorem long_seq (IH : LongIH M P fuel) (e₁ e₂ : Expr) :
    eval M (fuel + 1) P H φ (.seq e₁ e₂) = .outOfFuel →
      Long M P (evalConf H φ (.seq e₁ e₂)) (fuel + 1) := by
  simp only [eval]
  refine Long.andThen (C₁ := evalConf H φ e₁) (F := .seq e₂) (fun _ _ => .seqEnter)
    (eval_sim M P fuel H φ e₁) (IH H φ e₁) ?_
  intro H₁ v₁ _ _ hk
  try simp only [] at hk
  split at hk
  · simp at hk
  · rename_i hm
    split at hk
    · simp at hk
    · rename_i evs hd
      rw [EvalRes.withTrace_outOfFuel_iff] at hk
      have hne : v₁.mult P.decls ≠ .copy := by rw [hm]; exact nofun
      exact Long.mono (Nat.le_succ _) (Long.pre1 (C₂ := evalConf H₁ φ e₂)
        (fun K tr => ⟨_, _, .seqDrop hne hd, .refl _⟩) (IH H₁ φ e₂ hk))
  · rename_i hm
    exact Long.mono (Nat.le_succ _) (Long.pre1 (C₂ := evalConf H₁ φ e₂)
      (fun K tr => ⟨_, _, .seqCopy hm, .refl _⟩) (IH H₁ φ e₂ hk))

/-- (D-If-T)/(D-If-F) §6.6, counted (helper). -/
theorem long_ite (IH : LongIH M P fuel) (c e₁ e₂ : Expr) :
    eval M (fuel + 1) P H φ (.ite c e₁ e₂) = .outOfFuel →
      Long M P (evalConf H φ (.ite c e₁ e₂)) (fuel + 1) := by
  simp only [eval]
  refine Long.andThen (C₁ := evalConf H φ c) (F := .ite e₁ e₂) (fun _ _ => .iteEnter)
    (eval_sim M P fuel H φ c) (IH H φ c) ?_
  intro H₀ v _ _ hk
  try simp only [] at hk
  split at hk
  · rename_i b
    split at hk
    · rename_i hb
      subst hb
      exact Long.mono (Nat.le_succ _) (Long.pre1 (C₂ := evalConf H₀ φ e₁)
        (fun K tr => ⟨_, _, .iteTrue, .refl _⟩) (IH H₀ φ e₁ hk))
    · rename_i hb
      simp only [Bool.not_eq_true] at hb
      subst hb
      exact Long.mono (Nat.le_succ _) (Long.pre1 (C₂ := evalConf H₀ φ e₂)
        (fun K tr => ⟨_, _, .iteFalse, .refl _⟩) (IH H₀ φ e₂ hk))
  · simp at hk

/-- An argument-list form whose list spent the fuel, from its enter step
(helper). -/
theorem long_argsForm {t : ArgsTag} {es : List Expr} {e : Expr} (IH : LongIH M P fuel)
    (hent : ∀ K tr, Step M P (evalConf H φ e K tr) (argsConf H φ t [] es K tr))
    (h : evalArgs (fun H e => eval M fuel P H φ e) H es = .abort .outOfFuel) :
    Long M P (evalConf H φ e) (fuel + 1) :=
  Long.pre (fun K tr => ⟨tr, Steps.single (hent K tr)⟩) (evalArgs_long IH t es H [] h)

/-- (D-Struct) §6.5, counted (helper). -/
theorem long_mkStruct (IH : LongIH M P fuel) (s : Nat) (args : List Expr) :
    eval M (fuel + 1) P H φ (.mkStruct s args) = .outOfFuel →
      Long M P (evalConf H φ (.mkStruct s args)) (fuel + 1) := by
  simp only [eval]
  intro h
  split at h
  · subst h; exact long_argsForm IH (fun _ _ => .structEnter) ‹_›
  · rw [EvalRes.withTrace_outOfFuel_iff] at h
    (repeat' split at h) <;> first | simp at h | exact absurd h introVal_ne_outOfFuel

/-- (D-Enum-Intro) §6.6, counted (helper). -/
theorem long_mkEnum (IH : LongIH M P fuel) (e k : Nat) (args : List Expr) :
    eval M (fuel + 1) P H φ (.mkEnum e k args) = .outOfFuel →
      Long M P (evalConf H φ (.mkEnum e k args)) (fuel + 1) := by
  simp only [eval]
  intro h
  split at h
  · subst h; exact long_argsForm IH (fun _ _ => .enumEnter) ‹_›
  · rw [EvalRes.withTrace_outOfFuel_iff] at h
    (repeat' split at h) <;> first | simp at h | exact absurd h introVal_ne_outOfFuel

/-- (D-Array) §6.5, counted (helper). -/
theorem long_mkArray (IH : LongIH M P fuel) (T : Ty) (args : List Expr) :
    eval M (fuel + 1) P H φ (.mkArray T args) = .outOfFuel →
      Long M P (evalConf H φ (.mkArray T args)) (fuel + 1) := by
  simp only [eval]
  intro h
  split at h
  · subst h; exact long_argsForm IH (fun _ _ => .arrayEnter) ‹_›
  · rw [EvalRes.withTrace_outOfFuel_iff] at h
    exact absurd h introVal_ne_outOfFuel

/-- (D-Index) §6.5, counted (helper). -/
theorem long_indexRead (IH : LongIH M P fuel) (p : Place) (idx : List Expr) (πs : List (List Nat)) :
    eval M (fuel + 1) P H φ (.indexRead p idx πs) = .outOfFuel →
      Long M P (evalConf H φ (.indexRead p idx πs)) (fuel + 1) := by
  simp only [eval]
  intro h
  split at h
  · subst h; exact long_argsForm IH (fun _ _ => .indexReadEnter) ‹_›
  · rw [EvalRes.withTrace_outOfFuel_iff] at h
    (repeat' split at h) <;> simp at h

/-- `@drop` at a dynamic place, counted. `eval` re-dispatches it to the read
at one less fuel without a step of its own; the (Search) push into the first
index pays for that unit (helper). -/
theorem long_indexDrop (IH : LongIH M P fuel) (p : Place) (idx : List Expr) (πs : List (List Nat)) :
    eval M (fuel + 2) P H φ (.indexDrop p idx πs) = .outOfFuel →
      Long M P (evalConf H φ (.indexDrop p idx πs)) (fuel + 2) := by
  intro h
  have he : eval M (fuel + 2) P H φ (.indexDrop p idx πs) =
      (eval M (fuel + 1) P H φ (.indexRead p idx πs)).andThen (fun H' _ => .ok H' .unit []) := by
    simp only [eval]
  have hr : eval M (fuel + 1) P H φ (.indexRead p idx πs) = .outOfFuel := by
    rw [he] at h
    revert h
    cases eval M (fuel + 1) P H φ (.indexRead p idx πs) <;> simp [EvalRes.andThen, EvalRes.withTrace]
  simp only [eval] at hr
  split at hr
  · subst hr
    exact Long.pre1 (C₂ := argsConf H φ (.indexDrop p πs) [] idx)
      (fun _ tr => ⟨_, tr, .indexDropEnter, .refl _⟩) (evalArgs_long IH _ idx H [] ‹_›)
  · rw [EvalRes.withTrace_outOfFuel_iff] at hr
    (repeat' split at hr) <;> simp at hr

/-- `@drop` at a dynamic place at the smallest fuel: one step, (Search) into
the indices (helper). -/
theorem long_indexDrop_one (p : Place) (idx : List Expr) (πs : List (List Nat)) :
    Long M P (evalConf H φ (.indexDrop p idx πs)) 1 :=
  Long.pre1 (C₂ := argsConf H φ (.indexDrop p πs) [] idx)
    (fun _ tr => ⟨_, tr, .indexDropEnter, .refl _⟩) Long.zero

/-- (D-Assign) below a dynamic index (§6.8, `5.2:14`), counted (helper). -/
theorem long_indexWrite (IH : LongIH M P fuel) (p : Place) (idx : List Expr)
    (πs : List (List Nat)) (e : Expr) :
    eval M (fuel + 1) P H φ (.indexWrite p idx πs e) = .outOfFuel →
      Long M P (evalConf H φ (.indexWrite p idx πs e)) (fuel + 1) := by
  simp only [eval]
  refine Long.andThen (C₁ := evalConf H φ e) (F := .indexWriteRhs p idx πs)
    (fun _ _ => .indexWriteEnter) (eval_sim M P fuel H φ e) (IH H φ e) ?_
  intro H₁ v _ _ hk
  try simp only [] at hk
  split at hk
  · subst hk
    exact Long.mono (Nat.le_succ _)
      (Long.pre (fun K tr => ⟨tr, Steps.single .indexWriteRhs⟩)
        (evalArgs_long IH (.indexWrite p πs v) idx H₁ [] ‹_›))
  · rw [EvalRes.withTrace_outOfFuel_iff] at hk
    (repeat' split at hk) <;> simp at hk

/-- (D-Call) §6.9, counted: the body runs after the arguments and (D-Call)'s
step (helper). -/
theorem long_call (IH : LongIH M P fuel) (f : Nat) (args : List Expr) :
    eval M (fuel + 1) P H φ (.call f args) = .outOfFuel →
      Long M P (evalConf H φ (.call f args)) (fuel + 1) := by
  simp only [eval]
  intro h
  split at h
  · subst h; exact long_argsForm IH (fun _ _ => .callEnter) ‹_›
  · rename_i H₁ vs tr₁ hr
    rw [EvalRes.withTrace_outOfFuel_iff] at h
    split at h
    · simp at h
    · rename_i fd hfd
      split at h
      · rename_i hlen
        have hb : eval M fuel P (mintParams H₁ vs).1
            { env := (mintParams H₁ vs).2.reverse, scope := (mintParams H₁ vs).2 } fd.body =
              .outOfFuel := by
          revert h
          cases eval M fuel P (mintParams H₁ vs).1
              { env := (mintParams H₁ vs).2.reverse, scope := (mintParams H₁ vs).2 } fd.body
          all_goals simp only [EvalRes.absorb, EvalRes.withTrace_outOfFuel_iff, imp_self]
          all_goals (try split)
          all_goals simp
        exact Long.pre1 (C₂ := fun K tr => evalConf _ _ fd.body (.call φ :: K) tr)
          (fun K tr => ⟨_, tr ++ tr₁, .callEnter,
            (evalArgs_ok_steps hr K tr).trans (Steps.single (.call hfd hlen rfl))⟩)
          (fun K tr => IH _ _ _ hb _ tr)
      · simp at h

/-- (D-Loop-Enter) and (D-Loop-Iter) §6.10, counted. A turn that finishes
re-enters the body through (D-Loop-Iter) where `eval` re-evaluates the loop
at one less fuel; that re-evaluation's first step, (D-Loop-Enter), is peeled
off by determinism (`StepsN.peel`), and (D-Loop-Iter) stands in for it
(helper). -/
theorem long_loop (IH : LongIH M P fuel) (e : Expr) :
    eval M (fuel + 1) P H φ (.loop e) = .outOfFuel →
      Long M P (evalConf H φ (.loop e)) (fuel + 1) := by
  simp only [eval]
  have hent : ∀ K tr, Step M P (evalConf H φ (.loop e) K tr) (evalConf H φ e (.loop e φ :: K) tr) :=
    fun _ _ => .loopEnter
  cases hr : eval M fuel P H φ e with
  | ok H₁ v tr₁ =>
      intro h
      simp only [EvalRes.withTrace_outOfFuel_iff] at h
      have hs := eval_sim M P fuel H φ e
      rw [hr] at hs
      cases fuel with
      | zero => simp [eval] at hr
      | succ f =>
          have hL := IH H₁ φ (.loop e) h
          have hL' : Long M P (fun K tr => evalConf H₁ φ e (.loop e φ :: K) tr) f := by
            intro K tr
            obtain ⟨D, hD⟩ := hL K tr
            exact ⟨D, hD.peel .loopEnter⟩
          have hit : Long M P (fun K tr => .run H₁ φ (.loop e φ :: K) (.ret v) tr) (f + 1) :=
            Long.pre1 (fun K tr => ⟨_, tr ++ [], .loopIter (by simp [plainUnwind]), .refl _⟩) hL'
          exact Long.pre1 (fun K tr => ⟨_, tr ++ tr₁, hent K tr, hs _ tr⟩) hit
  | broke H₁ sc tr₁ =>
      intro h
      simp only [] at h
      split at h <;> simp at h
  | outOfFuel =>
      intro _
      exact Long.pre1 (C₂ := fun K tr => evalConf H φ e (.loop e φ :: K) tr)
        (fun K tr => ⟨_, tr, hent K tr, .refl _⟩) (fun K tr => IH H φ e hr _ tr)
  | returned => simp
  | panic => simp
  | stuck => simp

end longForms

/-- **Fuel counts steps** (RUE-2332; ADR-0097 decision 3). If `eval` exhausts
`fuel` on an expression, then from that expression in focus, under any context
and after any trace, §6's reduction has a run of exactly `fuel` steps. Each
unit of fuel `eval` spends is paid for by at least one `Step`: a (Search)
enter step (§6.2) before every recursive call, the operands already reduced
before it (`eval_sim`'s `ok` clause), and (D-Loop-Iter) (§6.10) for the loop's
re-evaluation. No typing hypothesis. The proof is a strong induction on fuel,
as `eval_sim`'s is. -/
theorem eval_steps_of_outOfFuel (M : FloatOps) (P : Program) (fuel : Nat) : LongIH M P fuel := by
  induction fuel using Nat.strongRecOn with
  | ind n ih =>
  intro H φ e
  cases n with
  | zero => intro _; exact Long.zero
  | succ fuel =>
    have IH := ih fuel (Nat.lt_succ_self _)
    cases e with
    | intLit | floatLit | boolLit | unitLit | use | drop | panic | brk =>
        intro h; exact absurd h (eval_leaf_ne_outOfFuel trivial)
    | binop op e₁ e₂ => exact long_binop IH op e₁ e₂
    | unop op e => exact long_unop IH op e
    | intCast w s e => exact long_intCast IH w s e
    | fintrin k e => exact long_fintrin IH k e
    | dbg e => exact long_dbg IH e
    | mkStruct s args => exact long_mkStruct IH s args
    | mkEnum e k args => exact long_mkEnum IH e k args
    | «match» scrut arms => exact long_match IH scrut arms
    | mkArray T args => exact long_mkArray IH T args
    | repeatArray T e n => exact long_repeat IH T e n
    | indexRead p idx πs => exact long_indexRead IH p idx πs
    | indexWrite p idx πs e => exact long_indexWrite IH p idx πs e
    | indexDrop p idx πs =>
        cases fuel with
        | zero => intro _; exact long_indexDrop_one p idx πs
        | succ f => exact long_indexDrop (ih f (by omega)) p idx πs
    | letIn m e₁ e₂ => exact long_letIn IH m e₁ e₂
    | assign p e => exact long_assign IH p e
    | seq e₁ e₂ => exact long_seq IH e₁ e₂
    | ite c e₁ e₂ => exact long_ite IH c e₁ e₂
    | call f args => exact long_call IH f args
    | ret e => exact long_ret IH e
    | loop e => exact long_loop IH e

/-! ## Completeness of `eval` modulo fuel -/

/-- Prefixing a trace never makes an unwinding `break` (helper). -/
theorem EvalRes.withTrace_ne_broke {r : EvalRes} {t : List Event}
    (h : ∀ H sc tr, r ≠ .broke H sc tr) : ∀ H sc tr, r.withTrace t ≠ .broke H sc tr := by
  cases r <;> simp_all [EvalRes.withTrace]

/-- The call boundary never passes an unwinding `break` on (helper). -/
theorem EvalRes.absorb_ne_broke {r : EvalRes} {k : Store → Val → EvalRes}
    (hk : ∀ H v H' sc tr, k H v ≠ .broke H' sc tr) : ∀ H sc tr, r.absorb k ≠ .broke H sc tr := by
  cases r with
  | ok H₁ v₁ tr₁ =>
      simp only [EvalRes.absorb]
      exact EvalRes.withTrace_ne_broke (fun H' sc' tr' => hk H₁ v₁ H' sc' tr')
  | _ => simp [EvalRes.absorb]

/-- A program's outcome is never an unwinding `break`: the entry point is a
call, and the call boundary turns a `break` that reached it into
`typeConfusion` (§6.10: "a `break` in a callee would be ill-formed")
(helper). -/
theorem run_ne_broke (M : FloatOps) {P : Program} {fuel : Nat} :
    ∀ H sc tr, run M P fuel ≠ .broke H sc tr := by
  unfold run
  cases fuel with
  | zero => simp [eval]
  | succ n =>
      simp only [eval, evalArgs]
      refine EvalRes.withTrace_ne_broke ?_
      cases P.fns[0]? with
      | none => simp
      | some fd =>
          simp only []
          split
          · exact EvalRes.absorb_ne_broke (by intro _ _ _ _ _; split <;> simp)
          · simp

/-- **Where a run of `Step` ends, `run` answers** (§6.12, `Step.det`): if
`→*` takes §6.12's initial configuration to a configuration with no successor
in `n` steps, then at every fuel past `n`, `run` answers a value whose
terminal configuration is that one, a panic that is that one, or a refusal.
Exhaustion is ruled out by `eval_steps_of_outOfFuel` (it would be a longer run
than `StepsN.bound` allows), an `ok` or a `panic` is placed by `run_sim` and
`Steps.final_unique`, and `run` is never `returned` or `broke` (helper). -/
theorem run_classify {M : FloatOps} {P : Program} {T : Config} (hT : Steps M P Config.init T)
    (hfin : ∀ C', ¬ Step M P T C') :
    ∃ n, ∀ fuel, n < fuel →
      (∃ H v tr, run M P fuel = .ok H v tr ∧ T = .run H Frame.empty [] (.ret v) tr) ∨
      (∃ κ tr, run M P fuel = .panic κ tr ∧ T = .panic κ tr) ∨
      (∃ w, run M P fuel = .stuck w) := by
  obtain ⟨n, hn⟩ := hT.toN
  refine ⟨n, fun fuel hlt => ?_⟩
  cases hr : run M P fuel with
  | ok H v tr =>
      exact .inl ⟨H, v, tr, rfl, Steps.final_unique hT ((run_sim M P fuel).1 H v tr hr) hfin
        (fun _ => Step.terminal trivial)⟩
  | panic κ tr =>
      exact .inr (.inl ⟨κ, tr, rfl, Steps.final_unique hT ((run_sim M P fuel).2 κ tr hr) hfin
        (fun _ => Step.terminal trivial)⟩)
  | stuck w => exact .inr (.inr ⟨w, rfl⟩)
  | outOfFuel =>
      obtain ⟨D, hD⟩ := eval_steps_of_outOfFuel M P fuel [] Frame.empty (.call 0 []) hr [] []
      exact absurd (hn.bound hfin hD) (by omega)
  | returned H v tr => exact absurd hr (run_ne_returned M H v tr)
  | broke H sc tr => exact absurd hr (run_ne_broke M H sc tr)

/-- **Completeness of `eval` modulo fuel, on every program** (RUE-2289 part
3, ADR-0097 decision 3; §6.2, §6.12). If §6's `→*` takes the initial
configuration to `✓` — a value at an empty stack — then at every fuel past
the number of steps, `run` answers that value with the same store and trace,
or refuses; likewise for `↯κ`. The refusal disjunct is where `eval`'s
monitors and its `@drop ⊘` refusal sit (RUE-2314); `eval_complete` removes it
on checked programs. -/
theorem run_complete (M : FloatOps) (P : Program) :
    (∀ H φ v tr, Steps M P Config.init (.run H φ [] (.ret v) tr) →
      ∃ n, ∀ fuel, n < fuel → run M P fuel = .ok H v tr ∨ ∃ w, run M P fuel = .stuck w) ∧
    (∀ κ tr, Steps M P Config.init (.panic κ tr) →
      ∃ n, ∀ fuel, n < fuel → run M P fuel = .panic κ tr ∨ ∃ w, run M P fuel = .stuck w) := by
  refine ⟨fun H φ v tr hT => ?_, fun κ tr hT => ?_⟩
  · obtain ⟨n, hn⟩ := run_classify hT (fun _ => Step.terminal trivial)
    refine ⟨n, fun fuel hlt => ?_⟩
    rcases hn fuel hlt with ⟨H', v', tr', hr, he⟩ | ⟨κ, tr', _, he⟩ | ⟨w, hr⟩
    · cases he; exact .inl hr
    · cases he
    · exact .inr ⟨w, hr⟩
  · obtain ⟨n, hn⟩ := run_classify hT (fun _ => Step.terminal trivial)
    refine ⟨n, fun fuel hlt => ?_⟩
    rcases hn fuel hlt with ⟨H', v', tr', _, he⟩ | ⟨κ', tr', hr, he⟩ | ⟨w, hr⟩
    · cases he
    · cases he; exact .inl hr
    · exact .inr ⟨w, hr⟩

/-- **Completeness of `eval` modulo fuel** (RUE-2289 part 3; ADR-0097
decision 3: "a theorem about `eval` is a theorem about §6 only once the two
are proved to agree"). For a program `check` accepts (`ProgramTyped`,
RUE-2314's domain): if §6's `→*` takes the initial configuration to a
terminal configuration — `✓`, a value at an empty stack, or `↯κ` — then some
fuel makes `run` answer that outcome with the same store, value and trace,
and so does every larger fuel (§6.2, §6.12). With `eval_sound` this is
adequacy in both directions: on checked programs, `run`'s values and panics
are exactly the ends of §6's runs, and `outOfFuel` at every fuel is exactly
divergence (`eval_diverges_iff`). -/
theorem eval_complete (M : FloatModel) {P : Program} (h : ProgramTyped P) :
    (∀ H φ v tr, Steps M.toFloatOps P Config.init (.run H φ [] (.ret v) tr) →
      ∃ n, ∀ fuel, n < fuel → run M.toFloatOps P fuel = .ok H v tr) ∧
    (∀ κ tr, Steps M.toFloatOps P Config.init (.panic κ tr) →
      ∃ n, ∀ fuel, n < fuel → run M.toFloatOps P fuel = .panic κ tr) := by
  obtain ⟨hv, hp⟩ := run_complete M.toFloatOps P
  refine ⟨fun H φ v tr hT => ?_, fun κ tr hT => ?_⟩
  · obtain ⟨n, hn⟩ := hv H φ v tr hT
    exact ⟨n, fun fuel hlt =>
      (hn fuel hlt).resolve_right (fun ⟨w, hw⟩ => no_violation M h fuel w hw)⟩
  · obtain ⟨n, hn⟩ := hp κ tr hT
    exact ⟨n, fun fuel hlt =>
      (hn fuel hlt).resolve_right (fun ⟨w, hw⟩ => no_violation M h fuel w hw)⟩

/-! ## "Never stuck", both ways -/

/-- **A stuck `Step` run is a refusal of `run`, on every program** (§6, §7):
if `→*` takes the initial configuration to a stuck one, then at every fuel past
the number of steps `run` refuses. The refusal need not name the same
`Violation`: `eval` inspects operand shapes in its own order (RUE-2314). -/
theorem run_stuck_of_step_stuck (M : FloatOps) (P : Program) {C : Config} {w : Violation}
    (hC : Steps M P Config.init C) (hs : C.Stuck M P w) :
    ∃ n, ∀ fuel, n < fuel → ∃ w', run M P fuel = .stuck w' := by
  obtain ⟨n, hn⟩ := run_classify hC (fun _ => hs.no_step)
  refine ⟨n, fun fuel hlt => ?_⟩
  rcases hn fuel hlt with ⟨H, v, tr, _, he⟩ | ⟨κ, tr, _, he⟩ | hw
  · subst he; simp [Config.Stuck, step] at hs
  · subst he; simp [Config.Stuck, step] at hs
  · exact hw

/-- **`eval` never stuck ⇒ `Step` never stuck, on every program** (§7's
phrasing: "it either reduces, halts with a value, or halts with one of the
defined panics"). If no fuel makes `run` refuse, every configuration `→*`
reaches from the initial one is terminal or takes a step. The converse fails
off the checked domain (RUE-2314): `@drop` of a `⊘` place is a refusal of
`eval` and a no-op of §6.11, and `eval` refuses `true + 1/0` where §6.2 panics
first. -/
theorem step_never_stuck_of_run (M : FloatOps) (P : Program)
    (hnv : ∀ fuel w, run M P fuel ≠ .stuck w) :
    ∀ C, Steps M P Config.init C → C.Terminal ∨ ∃ C', Step M P C C' := by
  intro C hC
  rcases Config.trichotomy M P C with hs | ht | ⟨w, hw⟩
  · exact .inr hs
  · exact .inl ht
  · obtain ⟨n, hn⟩ := run_stuck_of_step_stuck M P hC hw
    obtain ⟨w', hw'⟩ := hn (n + 1) (Nat.lt_succ_self n)
    exact absurd hw' (hnv _ _)

/-- **"Never stuck", both ways, in §7's phrasing** (RUE-2289 part 3; §7's
type-safety bullet; ADR-0097 decision 3). For a program `check` accepts,
"for every fuel, `run` is never `.stuck`" is equivalent to "every
configuration §6's `→*` reaches from the initial one reduces or has halted
with a value or a panic". The forward direction holds on every program
(`step_never_stuck_of_run`) and is the one with content; on this domain the
backward one is `no_violation`, and off it the backward one fails
(RUE-2314's discriminators). `fuel_mono` and `no_masking` (`Soundness.lean`)
say the same stability from `eval`'s side: its answer, once it is not
`outOfFuel`, is the answer at every larger fuel. -/
theorem never_stuck_iff (M : FloatModel) {P : Program} (h : ProgramTyped P) :
    (∀ fuel w, run M.toFloatOps P fuel ≠ .stuck w) ↔
      ∀ C, Steps M.toFloatOps P Config.init C → C.Terminal ∨ ∃ C', Step M.toFloatOps P C C' :=
  ⟨step_never_stuck_of_run M.toFloatOps P, fun _ fuel w => no_violation M h fuel w⟩

/-- **Divergence is exhaustion at every fuel** (RUE-2289 part 3, ADR-0097
decision 3). For a program `check` accepts, `run` is `outOfFuel` at every
fuel exactly when §6's reduction has a run of every length from the initial
configuration — by `Step.det`, one infinite run. So `outOfFuel` is never a
premature stop on a checked program: past the length of §6's run, `eval`
answers (`eval_complete`), and where it never answers §6 never halts. -/
theorem eval_diverges_iff (M : FloatModel) {P : Program} (h : ProgramTyped P) :
    (∀ fuel, run M.toFloatOps P fuel = .outOfFuel) ↔
      ∀ n, ∃ D, StepsN M.toFloatOps P n Config.init D := by
  constructor
  · intro hf n
    exact eval_steps_of_outOfFuel _ P n [] Frame.empty (.call 0 []) (hf n) [] []
  · intro hd fuel
    cases hr : run M.toFloatOps P fuel with
    | outOfFuel => rfl
    | ok H v tr =>
        obtain ⟨k, hk⟩ := ((run_sim _ P fuel).1 H v tr hr).toN
        obtain ⟨D, hD⟩ := hd (k + 1)
        exact absurd (hk.bound (fun _ => Step.terminal trivial) hD) (by omega)
    | panic κ tr =>
        obtain ⟨k, hk⟩ := ((run_sim _ P fuel).2 κ tr hr).toN
        obtain ⟨D, hD⟩ := hd (k + 1)
        exact absurd (hk.bound (fun _ => Step.terminal trivial) hD) (by omega)
    | stuck w => exact absurd hr (no_violation M h fuel w)
    | returned H v tr => exact absurd hr (run_ne_returned _ H v tr)
    | broke H sc tr => exact absurd hr (run_ne_broke _ H sc tr)

/-- **Why completeness is stated on checked programs** (RUE-2314): in
`let s = S{}; let t = s; @drop(s); 0`, §6's `→*` reaches `✓0`, because §6.11
makes `@drop` of a `⊘` place a no-op (`demo_dropMoved_runs`, `Step.lean`).
`run` refuses it with `useAfterMove` instead. That refusal is the one disjunct
`run_complete` allows, and `check` rejects the program. -/
theorem dropMoved_refused (M : FloatOps) :
    (∃ H, Steps M (demoProgram (.letIn false demoS
        (.letIn false (.use (.var 0)) (.seq (.drop (.var 1)) (demoI32 0))))) Config.init
      (.run H Frame.empty [] (.ret (.int .w32 .signed 0))
        [.drop 2 (demoSc 0), .dtor 0 (demoSc 0)])) ∧
    run M (demoProgram (.letIn false demoS
        (.letIn false (.use (.var 0)) (.seq (.drop (.var 1)) (demoI32 0))))) 100 =
      .stuck .useAfterMove :=
  ⟨demo_dropMoved_runs M, rfl⟩

/-- **The theorem at work**: `letAddProgram_runs` (`Step.lean`) found its
`→*` derivation by running `stepN`; here it comes from `run`'s answer alone,
through `run_sim` — `let x = 40; x + 2` reaches `✓42` with the `let`'s cell
retired and nothing printed (§6.7, §6.9, §6.12). -/
theorem letAddProgram_sound (M : FloatOps) :
    Steps M letAddProgram Config.init
      (.run [.dead] Frame.empty [] (.ret (.int .w32 .signed 42)) []) :=
  (run_sim M letAddProgram 100).1 _ _ _ rfl

/-! ## §7 over `Step`: progress and preservation (RUE-2289 part 4)

§7's first bullet is a statement about §6's machine: "a well-typed core
program does not get stuck: it either reduces, halts with a value, or halts
with one of the defined panics. Types are preserved under reduction." The
theorems above state it over `eval` (`soundness`, `run_safe`); this section
restates it over `Step`, derived from `soundness` and the two adequacy
directions, so the metatheory can cite a theorem in §7's own terms.

**Which preservation.** The configuration typing here is *semantic*:
`Config.SafeAt T C` says every configuration `C` reaches is terminal or
steps, and every value it halts with has type `T`. Progress and preservation
of `SafeAt` hold by construction, as in any semantic-typing proof; the content
is the fundamental lemma `init_safeAt` — a checked program's initial
configuration is safe at its entry type — and that is `soundness` carried to
§6 by adequacy. A *syntactic* configuration typing `⊢ C : T` (a typed store, a
typed frame stack with a Σ per suspended caller, and one preservation case per
`Step` constructor) would be a second safety proof over `Step`, not a
corollary of the first, and is not claimed here. -/

/-- **A configuration typed at `T`, semantically** (§7, first bullet): every
configuration `→*` reaches from `C` reduces or has halted ((Result-Ok),
(Result-Panic) §6.12), and every value `C` halts with — `✓v`, a value at an
empty stack — has type `T` (§5's value typing, `HasTy`). The typing is
defined by reduction, not by a syntactic judgment over the configuration
(this section's docstring says why). -/
def Config.SafeAt (M : FloatOps) (P : Program) (T : Ty) (C : Config) : Prop :=
  (∀ D, Steps M P C D → D.Terminal ∨ ∃ D', Step M P D D') ∧
  (∀ H φ v tr, Steps M P C (.run H φ [] (.ret v) tr) → HasTy P.decls v T)

/-- **Progress for a typed configuration** (§7, first bullet): it has halted
with a value or a defined panic, or it takes a step (§6.12's terminal
configurations; `Config.trichotomy` leaves stuck as the only other case). -/
theorem Config.SafeAt.progress {M : FloatOps} {P : Program} {T : Ty} {C : Config}
    (h : C.SafeAt M P T) : C.Terminal ∨ ∃ C', Step M P C C' :=
  h.1 C (.refl C)

/-- **Preservation for a typed configuration** (§7, first bullet: "types are
preserved under reduction"): a step of §6's `→` from a configuration typed at
`T` lands on one typed at `T`. -/
theorem Config.SafeAt.preservation {M : FloatOps} {P : Program} {T : Ty} {C C' : Config}
    (h : C.SafeAt M P T) (hs : Step M P C C') : C'.SafeAt M P T :=
  ⟨fun D hD => h.1 D (.step hs hD), fun H φ v tr hD => h.2 H φ v tr (.step hs hD)⟩

/-- Preservation along `→*` (§6.12) (helper). -/
theorem Config.SafeAt.steps {M : FloatOps} {P : Program} {T : Ty} {C C' : Config}
    (h : C.SafeAt M P T) (hs : Steps M P C C') : C'.SafeAt M P T :=
  ⟨fun D hD => h.1 D (hs.trans hD), fun H φ v tr hD => h.2 H φ v tr (hs.trans hD)⟩

/-- **The fundamental lemma: a checked program starts typed** (§7, first
bullet; §6.12's initial configuration). For a program `check` accepts, the
initial configuration is safe at the entry point's declared return type. The
"never stuck" half is `step_never_stuck_of_run` given `no_violation`; the
typing half takes a value §6 halts with to `run`'s answer at some fuel
(`eval_complete`), where `run_safe` (`soundness` over a whole program) types
it. This is the one place `soundness` enters the `Step` form. -/
theorem init_safeAt (M : FloatModel) {P : Program} (h : ProgramTyped P) :
    ∃ fd, P.fns[0]? = some fd ∧ Config.init.SafeAt M.toFloatOps P fd.ret := by
  obtain ⟨fd, h0, hp⟩ := h.entry
  refine ⟨fd, h0, step_never_stuck_of_run _ P (no_violation M h), ?_⟩
  intro H φ v tr hT
  obtain ⟨n, hn⟩ := (eval_complete M h).1 H φ v tr hT
  have hr := hn (n + 1) (Nat.lt_succ_self n)
  rcases run_safe M h.wf h0 hp (n + 1) with ho | ⟨κ, tr', hp'⟩ | ⟨H', v', tr', hr', hty⟩
  · rw [hr] at ho; cases ho
  · rw [hr] at hp'; cases hp'
  · rw [hr] at hr'; cases hr'; exact hty

/-- **Progress over §6's reduction** (§7, first bullet, in its own phrasing:
"a well-typed core program does not get stuck: it either reduces, halts with
a value, or halts with one of the defined panics"; ADR-0097 decision 3). For
a program `check` accepts, every configuration `→*` reaches from §6.12's
initial configuration is terminal or takes a step, so none is stuck
(`Config.stuck_iff`). Derived: `soundness` gives "`run` is never `.stuck`"
(`no_violation`), and `step_never_stuck_of_run` — built from `run_sim` and
the step count `eval_steps_of_outOfFuel` — carries it to `Step`. -/
theorem step_progress (M : FloatModel) {P : Program} (h : ProgramTyped P) :
    ∀ C, Steps M.toFloatOps P Config.init C → C.Terminal ∨ ∃ C', Step M.toFloatOps P C C' :=
  step_never_stuck_of_run _ P (no_violation M h)

/-- **Preservation over §6's reduction** (§7, first bullet: "types are
preserved under reduction"; ADR-0097 decision 3). For a program `check`
accepts, every configuration `→*` reaches from §6.12's initial configuration
is typed at the entry point's declared return type, in the semantic sense of
`Config.SafeAt`: it is never stuck from there on, and every value it halts
with has that type. With `Config.SafeAt.preservation` this is the one-step
form. The typing is semantic, not a syntactic `⊢ C : T`; this section's
docstring says what that does and does not claim. -/
theorem step_preservation (M : FloatModel) {P : Program} (h : ProgramTyped P) :
    ∃ fd, P.fns[0]? = some fd ∧
      ∀ C, Steps M.toFloatOps P Config.init C → C.SafeAt M.toFloatOps P fd.ret := by
  obtain ⟨fd, h0, hs⟩ := init_safeAt M h
  exact ⟨fd, h0, fun C hC => hs.steps hC⟩

/-- **The value §6 halts with has the declared type** (§7, first bullet;
§6.12's (Result-Ok)). For a program `check` accepts, if `→*` takes the
initial configuration to `✓v`, then `v` has the entry point's declared return
type. This is preservation read at the result, `Config.SafeAt`'s second half
at `Config.init`. -/
theorem step_value_typed (M : FloatModel) {P : Program} (h : ProgramTyped P) :
    ∃ fd, P.fns[0]? = some fd ∧ ∀ H φ v tr,
      Steps M.toFloatOps P Config.init (.run H φ [] (.ret v) tr) → HasTy P.decls v fd.ret := by
  obtain ⟨fd, h0, hs⟩ := init_safeAt M h
  exact ⟨fd, h0, hs.2⟩

/-- **Type safety over §6's reduction, at every horizon** (§7, first bullet;
§6.12; ADR-0097 decisions 3 and 5(b)). For a program `check` accepts and
every `n`: §6's machine runs `n` steps from the initial configuration, or it
has halted with a value of the entry point's declared type, or it has halted
with a defined panic — the three outcomes §7 allows, with stuck not among
them. By `Step.det` there is one run, so the halted cases are its end.

It is stated per horizon because "diverges or halts" is excluded middle on a
non-decidable property, outside this package's axioms; `eval_diverges_iff` is
the unbounded form of the first case. Fuel meets `Step` here directly: `run`
at fuel `n` is out of fuel (then §6 has an `n`-step run,
`eval_steps_of_outOfFuel`), a value (typed by `run_safe`, reached by
`run_sim`), or a panic (reached by `run_sim`); never `.stuck`. -/
theorem step_type_safety (M : FloatModel) {P : Program} (h : ProgramTyped P) :
    ∃ fd, P.fns[0]? = some fd ∧ ∀ n,
      (∃ D, StepsN M.toFloatOps P n Config.init D) ∨
      (∃ H v tr, Steps M.toFloatOps P Config.init (.run H Frame.empty [] (.ret v) tr) ∧
        HasTy P.decls v fd.ret) ∨
      (∃ κ tr, Steps M.toFloatOps P Config.init (.panic κ tr)) := by
  obtain ⟨fd, h0, hp⟩ := h.entry
  refine ⟨fd, h0, fun n => ?_⟩
  rcases run_safe M h.wf h0 hp n with ho | ⟨κ, tr, hr⟩ | ⟨H, v, tr, hr, hty⟩
  · exact .inl (eval_steps_of_outOfFuel _ P n [] Frame.empty (.call 0 []) ho [] [])
  · exact .inr (.inr ⟨κ, tr, (run_sim _ P n).2 κ tr hr⟩)
  · exact .inr (.inl ⟨H, v, tr, (run_sim _ P n).1 H v tr hr, hty⟩)

/-! ## One program, traced both ways -/

/-- The corpus case `affine_scope_drop` (`Corpus.lean`): `{ let v0: S1 =
S1 { x0: 7 }; 1 }` as the entry point returning `i64`, where `S1` is affine
with a destructor (helper). -/
abbrev affineScopeDropProgram : Program := Examples.prog Examples.tI64 Examples.affineDrop

/-- **One corpus program, both presentations** (GUIDE section 2, "One
program, traced both ways"; §6.2, §6.5, §6.7, §6.9, §6.11, §6.12). `check`
accepts `affine_scope_drop`; `run` answers `1` with both cells retired and
the trace "drop `ℓ1`, then `S1`'s destructor"; and §6's `→*` reaches the same
terminal configuration by the twelve steps written out here, one `Step`
constructor each: (Search) into the call's empty argument list, (D-Call),
(Search) into the `let`, (Search) into the struct literal and its one
initializer, the literal, the plug, (D-Struct) minting `#0`, (D-Let),
the body's literal, (D-EndScope) dropping and retiring `ℓ1`, and
(D-Return-Value). `explain/affine_scope_drop.txt` renders `eval`'s run of the
same program in seven rows: the (Search) steps are the part of `Step` that
`eval` does by recursion. -/
theorem affineScopeDrop_both_ways (M : FloatOps) :
    checkProgram affineScopeDropProgram = true ∧
    run M affineScopeDropProgram 100 =
      .ok [.dead, .dead] (.int .w64 .signed 1)
        [.drop 1 (.struct 1 0 [.int .w64 .signed 7]), .dtor 1 (.struct 1 0 [.int .w64 .signed 7])] ∧
    Steps M affineScopeDropProgram Config.init
      (.run [.dead, .dead] Frame.empty [] (.ret (.int .w64 .signed 1))
        [.drop 1 (.struct 1 0 [.int .w64 .signed 7]), .dtor 1 (.struct 1 0 [.int .w64 .signed 7])]) := by
  refine ⟨rfl, rfl, ?_⟩
  refine .step .callEnter ?_
  refine .step (.call rfl rfl rfl) ?_
  refine .step .letEnter ?_
  refine .step .structEnter ?_
  refine .step .argsPush ?_
  refine .step .intLit ?_
  refine .step .argsPlug ?_
  refine .step (.mkStruct rfl rfl) ?_
  refine .step .letBind ?_
  refine .step .intLit ?_
  refine .step (.endScope rfl) ?_
  refine .step (.callReturn rfl) ?_
  exact .refl _

end RueCore
