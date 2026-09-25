import RueCore.Step
import RueCore.Soundness

/-!
# RueCore.Adequacy — `eval` is sound with respect to §6's `Step`

ADR-0097's decision 3 proves safety over the definitional interpreter `eval`
and says that "a theorem about `eval` is a theorem about §6 only once the two
are proved to agree". `Step.lean` mechanizes §6's reduction relation; this
module proves the first half of the agreement, RUE-2289's part 2: whatever
`eval` answers with a value or a panic, §6's `→*` reaches too, with the same
store, value and trace (`eval_sound`). The converse, completeness modulo fuel,
is part 3 (RUE-2332), stated over the same relation `Sim`.

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
((D-Break) §6.10). Part 3 (RUE-2332) proves the converse over this same
relation. -/
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
is not a state of §6's machine, and completeness modulo fuel is part 3
(RUE-2332). -/
theorem eval_sound (M : FloatModel) {P : Program} (h : ProgramTyped P) (fuel : Nat) :
    (∀ w, run M.toFloatOps P fuel ≠ .stuck w) ∧
    (∀ H v tr, run M.toFloatOps P fuel = .ok H v tr →
      Steps M.toFloatOps P Config.init (.run H Frame.empty [] (.ret v) tr)) ∧
    (∀ k tr, run M.toFloatOps P fuel = .panic k tr →
      Steps M.toFloatOps P Config.init (.panic k tr)) :=
  ⟨no_violation M h fuel, (run_sim M.toFloatOps P fuel).1, (run_sim M.toFloatOps P fuel).2⟩


/-- **The theorem at work**: `letAddProgram_runs` (`Step.lean`) found its
`→*` derivation by running `stepN`; here it comes from `run`'s answer alone,
through `run_sim` — `let x = 40; x + 2` reaches `✓42` with the `let`'s cell
retired and nothing printed (§6.7, §6.9, §6.12). -/
theorem letAddProgram_sound (M : FloatOps) :
    Steps M letAddProgram Config.init
      (.run [.dead] Frame.empty [] (.ret (.int .w32 .signed 42)) []) :=
  (run_sim M letAddProgram 100).1 _ _ _ rfl

end RueCore
