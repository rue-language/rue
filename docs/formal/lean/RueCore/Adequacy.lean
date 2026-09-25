import RueCore.Step
import RueCore.Soundness

/-!
# RueCore.Adequacy — `eval` is sound with respect to §6's `Step`

(module docstring to follow)
-/

namespace RueCore

/-! ## The simulation relation -/

/-- The configuration family of an expression in focus: `⟨H ; φ ; K ; E[e]⟩`
for every context `K` and every trace `tr` already produced (§6.1, §6.2). -/
abbrev evalConf (H : Store) (φ : Frame) (e : Expr) : List Kont → List Event → Config :=
  fun K tr => .run H φ K (.eval e) tr

/-- **The simulation relation** between an `eval` result and `→*` (RUE-2289,
parts 2 and 3). -/
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

theorem Steps.trans {M : FloatOps} {P : Program} {C₁ C₂ C₃ : Config}
    (h₁ : Steps M P C₁ C₂) (h₂ : Steps M P C₂ C₃) : Steps M P C₁ C₃ := by
  induction h₁ with
  | refl => exact h₂
  | step s _ ih => exact .step s (ih h₂)

theorem Steps.single {M : FloatOps} {P : Program} {C₁ C₂ : Config}
    (h : Step M P C₁ C₂) : Steps M P C₁ C₂ := .step h (.refl _)

/-- Whether a configuration has an expression in focus (helper). -/
def Config.evalFocus : Config → Prop
  | .run _ _ _ (.eval _) _ => True
  | _ => False

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

theorem Sim.pre {M : FloatOps} {P : Program} {φ : Frame} {C C₂ : List Kont → List Event → Config}
    {r : EvalRes} (hpre : ∀ K tr, Steps M P (C K tr) (C₂ K tr)) (h : Sim M P φ C₂ r) :
    Sim M P φ C r := by
  cases r <;> simp only [Sim] at h ⊢
  · intro K tr; exact (hpre K tr).trans (h K tr)
  · intro K tr φs K' hK; exact (hpre K tr).trans (h K tr φs K' hK)
  · intro K tr φs K' H' evs hK hu; exact (hpre K tr).trans (h K tr φs K' H' evs hK hu)
  · intro K tr; exact (hpre K tr).trans (h K tr)

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

end forms

end RueCore
