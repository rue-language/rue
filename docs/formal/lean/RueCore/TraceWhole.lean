module

public import RueCore.TraceExact
public import RueCore.TracePrefix

@[expose] public section

/-!
# RueCore.TraceWhole — every owned value of a run ends exactly once (§7)

`drop_exactly_once` and `rest_exactly_once` (`TraceExact.lean`) state §7's
"no leak of drops" per evaluation and per form. Composed over a whole run they
are only an argument: at `run` both are vacuous (the run starts from the
empty store and the entry call's lead is empty), applying them at every
window of the run needs typing hypotheses at intermediate states that no
statement provides, and neither the trace nor the result records which
identities a run introduced (R2 and H4 of `REDTEAM-LOG.md`). What was proved
over a whole run is the "at most once" half (`no_double_free`,
`step_no_double_free`). This module proves the missing half — **no owned value
a run holds is lost** — and so the whole-program statement,
`whole_program_exactly_once`, over §6's relation.

## "Allocated along the run": what a configuration holds

`Config.held` (`Trace/Defs.lean`) reads off a configuration every owned
identity it keeps: in its store's cells, in the value or values in focus, and
pending on its control stack — a binary operator's left operand, a list
context's reduced values, an indexed assignment's right-hand side. An owned
value is allocated along a run exactly when some configuration of the run
holds it: (D-Struct), (D-Enum-Intro) and (D-Array) put a non-`Copy`
aggregate's fresh identity in focus the step they mint it. No event is added
to the trace and no existing statement changes.

## The ledger, and why it needs no configuration typing

A configuration's ledger is what it holds plus what its trace has ended
(`freedIds`). A step **loses nothing** when the target's ledger counts every
identity at least as often as the source's (`MSteps`). Stated over `Step`
alone this fails off the checked domain: `Step` has none of `eval`'s
monitors, so an ill-typed struct literal can hide an owned value under a
`Copy` node, where no drop reaches it (`Sharp.copy_leak`), and proving it on
the checked domain step by step would need a typing of every intermediate
configuration (RUE-2423).

So the ledger is carried along `eval`'s own run instead. `eval_msim` is
`eval_sim` (`Adequacy.lean`) with every run lossless: form by form, each
`Step` the simulation takes is shown to lose nothing, from the facts `eval`
itself established on the way — its copy-closure monitor passed at an
aggregate or an assignment, its store is copy-closed (`eval_exact`), and its
operands are scalars where §6.4 computes. The step's ledger is then one of
`TraceExact.lean`'s exact ledgers, read at every identity rather than only at
those the evaluation started with (`move_count`, `plainUnwind_count`, …).

The one place a step could discard a held value is an unwind: (D-Return) and
(D-Break) drop every frame above their target. `MSim`'s unwinding clauses
therefore ask that the discarded frames hold nothing, and `pendingSafe`
discharges it where a frame holds a value: the operand that runs under it
does not unwind (`eval_quiet`, `MSim.andThenHeld`). This is RUE-2316's
carve-out, used exactly where §6 discards a pending value.

## From one run to every configuration

`run_msteps` is the simulation at `run`: a finished run reaches its terminal
configuration from `Config.init` losslessly. By determinism (`Step.det`) every
configuration the run passes through lies on that path
(`MSteps.of_steps`), so its ledger is at most the end's. At the end the
store is empty of owned values (`eval_tidy`: every cell retired) and
`eval_conserves` bounds each identity's count in the value and the trace by
one. An identity the configuration held counts at least once, so exactly
once.

## What is not claimed

* A **panic** carries no claim. §6.12's `↯κ` keeps a trace and no store, and
  §5.7's `⊥_panic` edge runs no drop, so what a trap abandons is abandoned by
  design; `step_no_double_free` bounds its trace.
* A **run that never finishes** has no end to account at; its prefixes are
  bounded above by `step_no_double_free`.
* **RUE-2316** stays a hypothesis: `Sharp.pending_leak` is a checked program,
  not `pendingSafe`, whose `return` discards a pending owned argument that
  nothing ends.
-/

namespace RueCore

section ledger
variable {M : FloatOps} {P : Program}

/-! ## The ledger of a configuration -/

/-- The owned identities a control stack holds pending (helper). -/
abbrev stackOwn (D : Decls) (K : List Kont) : List Nat := K.flatMap (Kont.own D)

/-- A configuration's **ledger**: what it holds (`Config.held`) and what its
trace has ended (`freedIds`), as one multiset (helper). -/
def Config.ledger (D : Decls) (C : Config) : List Nat := C.held D ++ freedIds D C.trace

/-- A running configuration's ledger, counted part by part (helper). -/
theorem Config.ledger_count_run (D : Decls) (H : Store) (φ : Frame) (K : List Kont) (f : Focus)
    (tr : List Event) (a : Nat) :
    ((Config.run H φ K f tr).ledger D).count a =
      (storeOwn D H).count a + (f.own D).count a + (stackOwn D K).count a +
        (freedIds D tr).count a := by
  simp only [Config.ledger, Config.held, Config.trace, stackOwn, List.count_append]

/-- A step between running configurations loses nothing when every identity
the source holds is, after it, held or ended by what the step appended
(helper). -/
theorem ledger_le_run {D : Decls} {H H' : Store} {φ φ' : Frame} {K K' : List Kont} {f f' : Focus}
    {tr evs : List Event}
    (h : ∀ a, (storeOwn D H).count a + (f.own D).count a + (stackOwn D K).count a ≤
      (storeOwn D H').count a + (f'.own D).count a + (stackOwn D K').count a +
        (freedIds D evs).count a) :
    IdLe ((Config.run H φ K f tr).ledger D) ((Config.run H' φ' K' f' (tr ++ evs)).ledger D) := by
  intro a
  rw [Config.ledger_count_run, Config.ledger_count_run, freedIds_append, List.count_append]
  have := h a
  omega

/-- The same for a step that appends nothing to the trace (helper). -/
theorem ledger_le_run0 {D : Decls} {H H' : Store} {φ φ' : Frame} {K K' : List Kont} {f f' : Focus}
    {tr : List Event}
    (h : ∀ a, (storeOwn D H).count a + (f.own D).count a + (stackOwn D K).count a ≤
      (storeOwn D H').count a + (f'.own D).count a + (stackOwn D K').count a) :
    IdLe ((Config.run H φ K f tr).ledger D) ((Config.run H' φ' K' f' tr).ledger D) := by
  have := ledger_le_run (φ := φ) (φ' := φ') (tr := tr) (evs := [])
    (fun a => by have := h a; simp only [freedIds, List.flatMap_nil, List.count_nil]; omega)
  simpa using this

/-- `IdLe` is reflexive (helper). -/
theorem IdLe.refl' (l : List Nat) : IdLe l l := fun _ => Nat.le_refl _

/-- `IdLe` is transitive (helper). -/
theorem IdLe.trans' {l₁ l₂ l₃ : List Nat} (h₁ : IdLe l₁ l₂) (h₂ : IdLe l₂ l₃) : IdLe l₁ l₃ :=
  fun a => Nat.le_trans (h₁ a) (h₂ a)

/-! ## Runs that lose nothing -/

/-- **A run of §6's relation along which no step loses an owned identity**:
every step's target ledger counts every identity at least as often as its
source's (helper). -/
inductive MSteps (M : FloatOps) (P : Program) : Config → Config → Prop where
  | refl (C : Config) : MSteps M P C C
  | step {C₁ C₂ C₃ : Config} : Step M P C₁ C₂ → IdLe (C₁.ledger P.decls) (C₂.ledger P.decls) →
      MSteps M P C₂ C₃ → MSteps M P C₁ C₃

/-- Lossless runs compose (helper). -/
theorem MSteps.trans {C₁ C₂ C₃ : Config} (h₁ : MSteps M P C₁ C₂) (h₂ : MSteps M P C₂ C₃) :
    MSteps M P C₁ C₃ := by
  induction h₁ with
  | refl => exact h₂
  | step s hl _ ih => exact .step s hl (ih h₂)

/-- One lossless step is a lossless run (helper). -/
theorem MSteps.single {C₁ C₂ : Config} (s : Step M P C₁ C₂)
    (hl : IdLe (C₁.ledger P.decls) (C₂.ledger P.decls)) : MSteps M P C₁ C₂ :=
  .step s hl (.refl _)

/-- A lossless run's end counts everything its start counts (helper). -/
theorem MSteps.le {C₁ C₂ : Config} (h : MSteps M P C₁ C₂) :
    IdLe (C₁.ledger P.decls) (C₂.ledger P.decls) := by
  induction h with
  | refl C => exact IdLe.refl' _
  | step _ hl _ ih => exact IdLe.trans' hl ih

/-- A lossless run is a run (helper). -/
theorem MSteps.toSteps {C₁ C₂ : Config} (h : MSteps M P C₁ C₂) : Steps M P C₁ C₂ := by
  induction h with
  | refl C => exact .refl C
  | step s _ _ ih => exact .step s ih

/-- **Peeling a step by determinism** (`Step.det`): a lossless run from `C`
that ends where no expression is in focus passes through `C`'s one successor
(helper). -/
theorem MSteps.peel {C C' D : Config} (hs : Step M P C C') (h : MSteps M P C D)
    (hC : C.evalFocus) (hD : ¬ D.evalFocus) : MSteps M P C' D := by
  cases h with
  | refl => exact absurd hC hD
  | step s _ rest => rw [Step.det hs s]; exact rest

/-- **Every configuration a run reaches on the way to a lossless run's
terminal end loses nothing up to that end** (`Step.det`): if `C` reaches `T`
losslessly, `T` takes no step, and `C` reaches `D`, then `D` reaches `T`
losslessly (helper). -/
theorem MSteps.of_steps {C D T : Config} (hT : MSteps M P C T) (hfin : ∀ C', ¬ Step M P T C')
    (h : Steps M P C D) : MSteps M P D T := by
  induction h with
  | refl => exact hT
  | @step C₁ C₂ C₃ s _ ih =>
      cases hT with
      | refl => exact absurd s (hfin _)
      | step s' _ rest => rw [Step.det s s'] at *; exact ih rest

/-! ## The lossless simulation -/

/-- **`eval`'s result, simulated losslessly** (helper): `Sim`'s clauses
(`Adequacy.lean`) with every run lossless (`MSteps`), for a value, an
unwinding `return` and an unwinding `break`. The two unwinding clauses ask of
the context that the frames the unwind discards hold no owned value — what
`pendingSafe` guarantees at every form that pushes such a frame (RUE-2316). A
trap is not simulated: §6.12's `↯κ` keeps no store, so it holds nothing, and
§5.7's `⊥_panic` edge runs no drop. -/
def MSim (M : FloatOps) (P : Program) (φ : Frame) (C : List Kont → List Event → Config) :
    EvalRes → Prop
  | .ok H v tr' => ∀ K tr, MSteps M P (C K tr) (.run H φ K (.ret v) (tr ++ tr'))
  | .returned H v tr' => ∀ K tr φs K', Kont.toCall K = some (φs, K') →
      IdLe (stackOwn P.decls K) (stackOwn P.decls K') →
      MSteps M P (C K tr) (.run H φs K' (.ret v) (tr ++ tr'))
  | .broke H sc tr' => ∀ K tr φs K' H' evs, Kont.toLoop K = some (φs, K') →
      IdLe (stackOwn P.decls K) (stackOwn P.decls K') →
      plainUnwind P.decls H (sc.drop φs.scope.length).reverse = .ok (H', evs) →
      MSteps M P (C K tr) (.run H' φs K' (.ret .unit) (tr ++ tr' ++ evs))
  | .panic _ _ | .stuck _ | .outOfFuel => True

/-- A frame that holds nothing leaves the stack's holdings alone (helper). -/
theorem stackOwn_cons_nil {D : Decls} {F : Kont} (hF : F.own D = []) (K : List Kont) :
    stackOwn D (F :: K) = stackOwn D K := by
  simp [stackOwn, List.flatMap_cons, hF]

/-- A lossless run into the family carries its simulation back (helper). -/
theorem MSim.pre {φ : Frame} {C C₂ : List Kont → List Event → Config} {r : EvalRes}
    (hpre : ∀ K tr, MSteps M P (C K tr) (C₂ K tr)) (h : MSim M P φ C₂ r) : MSim M P φ C r := by
  cases r <;> simp only [MSim] at h ⊢
  · intro K tr; exact (hpre K tr).trans (h K tr)
  · intro K tr φs K' hK hs; exact (hpre K tr).trans (h K tr φs K' hK hs)
  · intro K tr φs K' H' evs hK hs hu; exact (hpre K tr).trans (h K tr φs K' H' evs hK hs hu)

/-- A lossless run that emits `tr₁` carries the simulation back to the result
with `tr₁` prefixed (helper). -/
theorem MSim.withTrace {φ : Frame} {C C₂ : List Kont → List Event → Config} {r : EvalRes}
    {tr₁ : List Event} (hpre : ∀ K tr, MSteps M P (C K tr) (C₂ K (tr ++ tr₁)))
    (h : MSim M P φ C₂ r) : MSim M P φ C (r.withTrace tr₁) := by
  cases r <;> simp only [MSim, EvalRes.withTrace] at h ⊢
  · intro K tr; have := h K (tr ++ tr₁); simp only [List.append_assoc] at this
    exact (hpre K tr).trans this
  · intro K tr φs K' hK hs; have := h K (tr ++ tr₁) φs K' hK hs
    simp only [List.append_assoc] at this
    exact (hpre K tr).trans this
  · intro K tr φs K' H' evs hK hs hu; have := h K (tr ++ tr₁) φs K' H' evs hK hs hu
    simp only [List.append_assoc] at this ⊢
    exact (hpre K tr).trans this

/-- A result that neither completes nor unwinds is simulated vacuously
(helper). -/
theorem MSim.of_quiet {φ : Frame} {C : List Kont → List Event → Config} {r : EvalRes}
    (hq : r.NoRet ∧ r.NoBrk) (hok : ∀ H v tr, r ≠ .ok H v tr) : MSim M P φ C r := by
  cases r with
  | ok H v tr => exact absurd rfl (hok H v tr)
  | returned => exact hq.1.elim
  | broke => exact hq.2.elim
  | _ => trivial

/-- **§6.2's (Search), once, losslessly**: an enter run pushing a frame `F`
that holds nothing, the operand simulated under `F`, and the context's
simulation from the operand's value (helper). -/
theorem MSim.andThen {φ φ₁ : Frame} {C C₁ : List Kont → List Event → Config} {F : Kont}
    (hF : F.Transparent) (hFo : F.own P.decls = [])
    (hC : ∀ K tr, MSteps M P (C K tr) (C₁ (F :: K) tr))
    {r : EvalRes} (h₁ : MSim M P φ₁ C₁ r) {k : Store → Val → EvalRes}
    (hk : ∀ H₁ v tr₁, r = .ok H₁ v tr₁ →
      MSim M P φ (fun K tr => .run H₁ φ₁ (F :: K) (.ret v) tr) (k H₁ v)) :
    MSim M P φ C (r.andThen k) := by
  cases r with
  | ok H₁ v tr₁ =>
      simp only [EvalRes.andThen]
      exact MSim.withTrace (fun K tr => (hC K tr).trans (h₁ (F :: K) tr)) (hk H₁ v tr₁ rfl)
  | returned H₁ v tr₁ =>
      simp only [EvalRes.andThen, MSim] at h₁ ⊢
      intro K tr φs K' hK hs
      exact (hC K tr).trans (h₁ (F :: K) tr φs K' (by rw [(hF K).1]; exact hK)
        (by rw [stackOwn_cons_nil hFo]; exact hs))
  | broke H₁ sc tr₁ =>
      simp only [EvalRes.andThen, MSim] at h₁ ⊢
      intro K tr φs K' H' evs hK hs hu
      exact (hC K tr).trans (h₁ (F :: K) tr φs K' H' evs (by rw [(hF K).2]; exact hK)
        (by rw [stackOwn_cons_nil hFo]; exact hs) hu)
  | panic κ tr₁ => trivial
  | stuck w => trivial
  | outOfFuel => trivial

/-- **A later operand under a held value** (RUE-2316): the frame holds a
value, so the operand must not unwind — `pendingSafe` — and then only its
value matters (helper). -/
theorem MSim.andThenHeld {φ φ₁ : Frame} {C C₁ : List Kont → List Event → Config} {F : Kont}
    (hC : ∀ K tr, MSteps M P (C K tr) (C₁ (F :: K) tr))
    {r : EvalRes} (hq : r.NoRet ∧ r.NoBrk) (h₁ : MSim M P φ₁ C₁ r) {k : Store → Val → EvalRes}
    (hk : ∀ H₁ v tr₁, r = .ok H₁ v tr₁ →
      MSim M P φ (fun K tr => .run H₁ φ₁ (F :: K) (.ret v) tr) (k H₁ v)) :
    MSim M P φ C (r.andThen k) := by
  cases r with
  | ok H₁ v tr₁ =>
      simp only [EvalRes.andThen]
      exact MSim.withTrace (fun K tr => (hC K tr).trans (h₁ (F :: K) tr)) (hk H₁ v tr₁ rfl)
  | returned => exact hq.1.elim
  | broke => exact hq.2.elim
  | panic κ tr₁ => trivial
  | stuck w => trivial
  | outOfFuel => trivial

/-- A result that is not a value passes through a frame that holds nothing
(helper). -/
theorem MSim.lift {φ φ₁ : Frame} {C C₁ : List Kont → List Event → Config} {F : Kont}
    (hF : F.Transparent) (hFo : F.own P.decls = [])
    (hC : ∀ K tr, MSteps M P (C K tr) (C₁ (F :: K) tr))
    {r : EvalRes} (h₁ : MSim M P φ₁ C₁ r) (hr : ∀ H v tr, r ≠ .ok H v tr) :
    MSim M P φ C r := by
  have := MSim.andThen (φ := φ) (k := fun _ _ => .outOfFuel) hF hFo hC h₁
    (fun H v tr h => absurd h (hr H v tr))
  cases r <;> simp_all [EvalRes.andThen]

/-- §6.9's call boundary, losslessly: the body's `returned` is caught at the
`call φ` frame, which holds nothing (helper). -/
theorem MSim.absorb {φ φ₁ : Frame} {C C₁ : List Kont → List Event → Config}
    (hC : ∀ K tr, MSteps M P (C K tr) (C₁ (.call φ :: K) tr))
    {r : EvalRes} (h₁ : MSim M P φ₁ C₁ r) {k : Store → Val → EvalRes}
    (hk : ∀ H₁ v tr₁, r = .ok H₁ v tr₁ →
      MSim M P φ (fun K tr => .run H₁ φ₁ (.call φ :: K) (.ret v) tr) (k H₁ v)) :
    MSim M P φ C (r.absorb k) := by
  cases r with
  | ok H₁ v tr₁ =>
      simp only [EvalRes.absorb]
      exact MSim.withTrace (fun K tr => (hC K tr).trans (h₁ (.call φ :: K) tr)) (hk H₁ v tr₁ rfl)
  | returned H₁ v tr₁ =>
      simp only [EvalRes.absorb, MSim] at h₁ ⊢
      intro K tr
      exact (hC K tr).trans (h₁ (.call φ :: K) tr φ K rfl
        (by rw [stackOwn_cons_nil rfl]; exact IdLe.refl' _))
  | broke H₁ sc tr₁ => simp [EvalRes.absorb, MSim]
  | panic κ tr₁ => trivial
  | stuck w => simp [EvalRes.absorb, MSim]
  | outOfFuel => simp [EvalRes.absorb, MSim]

/-- Where no target has an expression in focus, a first step of the family
can be peeled off by determinism (helper). -/
theorem MSim.peel {φ : Frame} {C C₂ : List Kont → List Event → Config} {r : EvalRes}
    (hs : ∀ K tr, Step M P (C K tr) (C₂ K tr)) (hC : ∀ K tr, (C K tr).evalFocus)
    (h : MSim M P φ C r) : MSim M P φ C₂ r := by
  cases r <;> simp only [MSim] at h ⊢
  · intro K tr; exact MSteps.peel (hs K tr) (h K tr) (hC K tr) (by simp [Config.evalFocus])
  · intro K tr φs K' hK hsK
    exact MSteps.peel (hs K tr) (h K tr φs K' hK hsK) (hC K tr) (by simp [Config.evalFocus])
  · intro K tr φs K' H' evs hK hsK hu
    exact MSteps.peel (hs K tr) (h K tr φs K' H' evs hK hsK hu) (hC K tr)
      (by simp [Config.evalFocus])

end ledger

/-! ## One step's ledger, identity by identity

The equalities `TraceExact.lean`'s exact ledgers are made of, at every
identity: `Exact` counts only the identities an evaluation starts with, and a
step of a run holds identities minted anywhere before it. -/

/-- `run-scope-drops` without the monitor (§6.1): each cell's owned
identities leave the store and exactly those reach the trace (helper). -/
theorem plainUnwind_count {D : Decls} : ∀ {H H' : Store} {ls : List Nat} {evs : List Event},
    plainUnwind D H ls = .ok (H', evs) → ∀ a,
      (storeOwn D H).count a = (storeOwn D H').count a + (freedIds D evs).count a
  | H, H', [], evs, h => by
      simp only [plainUnwind, Except.ok.injEq, Prod.mk.injEq] at h
      obtain ⟨rfl, rfl⟩ := h
      intro a; simp [freedIds]
  | H, H', ℓ :: ls, evs, h => by
      simp only [plainUnwind] at h
      split at h
      · cases h
      · rename_i H₁ evs₁ h₁
        split at h
        · cases h
        · rename_i H₂ evs₂ h₂
          simp only [Except.ok.injEq, Prod.mk.injEq] at h
          obtain ⟨rfl, rfl⟩ := h
          intro a
          have i₂ := plainUnwind_count h₂ a
          unfold plainDropRetire at h₁
          split at h₁
          · cases h₁
          · cases h₁
          · rename_i c hc
            split at h₁
            · cases h₁
            · rename_i evs' hd
              simp only [Except.ok.injEq, Prod.mk.injEq] at h₁
              obtain ⟨rfl, rfl⟩ := h₁
              have h1 := storeOwn_set_count D a Cell.dead hc
              rw [freedIds_append, List.count_append, dropCell_freed hd]
              simp only [Cell.own, List.count_nil] at h1
              omega

/-- (D-Use-Move) §6.3, at every identity (helper). -/
theorem move_count {D : Decls} {H : Store} {ℓ : Nat} {c c' sub : Contents} {π : List Nat}
    {v : Val} (hcc : StoreCC D H) (hc : H[ℓ]? = some (.full c)) (hr : c.readAt π = .ok sub)
    (hw : c.writeAt π .hole = some c') (hv : sub.toVal = some v) (a : Nat) :
    (storeOwn D H).count a = (storeOwn D (H.set ℓ (.full c'))).count a + (v.own D).count a := by
  have hccc := hcc ℓ c hc
  have hc' := Contents.writeAt_copyClosed π hccc rfl hw
  have hsub : Contents.ofVal v = sub := Contents.ofVal_toVal hv
  have h1 := storeOwn_set_count D a (.full c') hc
  have h2 := Contents.writeAt_own_eq a π hccc hc' hr hw
  simp only [Cell.own, Contents.own, List.count_nil] at h1 h2
  simp only [Val.own, hsub]
  omega

/-- (D-Use-Declared-Linear) §6.3, at every identity (helper). -/
theorem destructure_count {D : Decls} {H : Store} {ℓ : Nat} {c c' cd leaf : Contents}
    {πd πs : List Nat} {v : Val} {evs : List Event} (hcc : StoreCC D H)
    (hc : H[ℓ]? = some (.full c)) (hr : c.readAt πd = .ok cd)
    (hd : cd.destructure D ℓ πs = .ok (leaf, evs)) (hv : leaf.toVal = some v)
    (hw : c.writeAt πd .hole = some c') (a : Nat) :
    (storeOwn D H).count a = (storeOwn D (H.set ℓ (.full c'))).count a + (v.own D).count a +
      (freedIds D evs).count a := by
  have hccc := hcc ℓ c hc
  have hcd := Contents.readAt_copyClosed πd hccc hr
  have hc' := Contents.writeAt_copyClosed πd hccc rfl hw
  have hsub : Contents.ofVal v = leaf := Contents.ofVal_toVal hv
  have h1 := storeOwn_set_count D a (.full c') hc
  have h2 := Contents.writeAt_own_eq a πd hccc hc' hr hw
  have h3 := (Contents.destructure_exact hcd hd a).1
  simp only [Cell.own, Contents.own, List.count_nil] at h1 h2
  simp only [Val.own, hsub]
  omega

/-- §6.11's `@drop`, at every identity (helper). -/
theorem dropPlace_count {D : Decls} {H : Store} {ℓ : Nat} {c c' sub : Contents} {π : List Nat}
    {evs : List Event} (hcc : StoreCC D H) (hc : H[ℓ]? = some (.full c))
    (hr : c.readAt π = .ok sub) (hd : dropCell D ℓ sub = .ok evs)
    (hw : c.writeAt π .hole = some c') (a : Nat) :
    (storeOwn D H).count a = (storeOwn D (H.set ℓ (.full c'))).count a + (freedIds D evs).count a := by
  have hccc := hcc ℓ c hc
  have hc' := Contents.writeAt_copyClosed π hccc rfl hw
  have h1 := storeOwn_set_count D a (.full c') hc
  have h2 := Contents.writeAt_own_eq a π hccc hc' hr hw
  rw [dropCell_freed hd]
  simp only [Cell.own, Contents.own, List.count_nil] at h1 h2
  omega

/-- §6.11's `@drop` at a declared plan, at every identity (helper). -/
theorem dropDeclared_count {D : Decls} {H : Store} {ℓ : Nat} {c c' cd leaf : Contents}
    {πd πs : List Nat} {evs levs : List Event} (hcc : StoreCC D H)
    (hc : H[ℓ]? = some (.full c)) (hr : c.readAt πd = .ok cd)
    (hd : cd.destructure D ℓ πs = .ok (leaf, evs)) (hl : dropCell D ℓ leaf = .ok levs)
    (hw : c.writeAt πd .hole = some c') (a : Nat) :
    (storeOwn D H).count a =
      (storeOwn D (H.set ℓ (.full c'))).count a + (freedIds D (evs ++ levs)).count a := by
  have hccc := hcc ℓ c hc
  have hcd := Contents.readAt_copyClosed πd hccc hr
  have hc' := Contents.writeAt_copyClosed πd hccc rfl hw
  have h1 := storeOwn_set_count D a (.full c') hc
  have h2 := Contents.writeAt_own_eq a πd hccc hc' hr hw
  have h3 := (Contents.destructure_exact hcd hd a).1
  rw [freedIds_append, List.count_append, dropCell_freed hl]
  simp only [Cell.own, Contents.own, List.count_nil] at h1 h2
  omega

/-- (D-Assign) §6.8, at every identity (helper). -/
theorem assign_count {D : Decls} {H : Store} {ℓ : Nat} {c c' old : Contents} {π : List Nat}
    {v : Val} {evs : List Event} (hcc : StoreCC D H) (hc : H[ℓ]? = some (.full c))
    (hr : c.readAt π = .ok old) (hd : dropCell D ℓ old = .ok evs)
    (hw : c.writeAt π (Contents.ofVal v) = some c') (hc' : c'.copyClosed D = true) (a : Nat) :
    (storeOwn D H).count a + (v.own D).count a =
      (storeOwn D (H.set ℓ (.full c'))).count a + (freedIds D evs).count a := by
  have hccc := hcc ℓ c hc
  have h1 := storeOwn_set_count D a (.full c') hc
  have h2 := Contents.writeAt_own_eq a π hccc hc' hr hw
  rw [dropCell_freed hd]
  simp only [Cell.own] at h1
  simp only [Val.own] at *
  omega

/-- (D-Assign) below a dynamic index, at every identity (helper). -/
theorem assignDyn_count {D : Decls} {H : Store} {ℓ : Nat} {c c' sub sub' old : Contents}
    {π ρ : List Nat} {v : Val} {evs : List Event} (hcc : StoreCC D H)
    (hc : H[ℓ]? = some (.full c)) (hr : c.readAt π = .ok sub) (hr' : sub.readAt ρ = .ok old)
    (hd : dropCell D ℓ old = .ok evs) (hw' : sub.writeAt ρ (Contents.ofVal v) = some sub')
    (hw : c.writeAt π sub' = some c') (hc' : c'.copyClosed D = true) (a : Nat) :
    (storeOwn D H).count a + (v.own D).count a =
      (storeOwn D (H.set ℓ (.full c'))).count a + (freedIds D evs).count a := by
  have hccc := hcc ℓ c hc
  have hsub := Contents.readAt_copyClosed π hccc hr
  have hsub' : sub'.copyClosed D = true :=
    Contents.readAt_copyClosed π hc' (Contents.readAt_writeAt π hw)
  have h1 := storeOwn_set_count D a (.full c') hc
  have h2 := Contents.writeAt_own_eq a π hccc hc' hr hw
  have h2' := Contents.writeAt_own_eq a ρ hsub hsub' hr' hw'
  rw [dropCell_freed hd]
  simp only [Cell.own] at h1
  simp only [Val.own] at *
  omega

/-- A fresh struct owns at least its fields, copy-closed (helper). -/
theorem Contents.own_struct_ge {D : Decls} {s i : Nat} {cs : List Contents}
    (h : (Contents.struct s i cs).copyClosed D = true) (a : Nat) :
    (Contents.ownList D cs).count a ≤ ((Contents.struct s i cs).own D).count a := by
  simp only [Contents.copyClosed] at h
  simp only [Contents.own]
  split
  · rename_i hc; rw [if_pos hc] at h; simp [Contents.allCopyList_own h]
  · simp only [List.count_cons]; omega

/-- A fresh array owns at least its elements, copy-closed (helper). -/
theorem Contents.own_array_ge {D : Decls} {T : Ty} {i : Nat} {cs : List Contents}
    (h : (Contents.array T i cs).copyClosed D = true) (a : Nat) :
    (Contents.ownList D cs).count a ≤ ((Contents.array T i cs).own D).count a := by
  simp only [Contents.copyClosed] at h
  simp only [Contents.own]
  split
  · rename_i hc; rw [if_pos hc] at h; simp [Contents.allCopyList_own h]
  · simp only [List.count_cons]; omega

/-! ## Argument lists, losslessly -/

/-- The induction hypothesis: `eval` at fuel `fuel` is simulated losslessly
from every copy-closed store, for every `pendingSafe` expression (helper). -/
def MSimIH (M : FloatOps) (P : Program) (fuel : Nat) : Prop :=
  ∀ H φ e, StoreCC P.decls H → e.pendingSafe = true →
    MSim M P φ (evalConf H φ e) (eval M fuel P H φ e)

section forms
variable {M : FloatOps} {P : Program} {fuel : Nat} {H : Store} {φ : Frame}

/-- An argument list of `pendingSafe` members that finishes leaves a
copy-closed store and copy-closed values (`eval_exact`) (helper). -/
theorem evalArgs_cc (hp : P.pendingSafe = true) :
    ∀ {es : List Expr} {H H' : Store} {vs : List Val} {tr : List Event},
      Expr.pendingSafeList es = true → StoreCC P.decls H →
      evalArgs (fun H e => eval M fuel P H φ e) H es = .ok H' vs tr →
      StoreCC P.decls H' ∧ Contents.copyClosedList P.decls (Contents.ofVals vs) = true
  | [], H, H', vs, tr, _, hcc, h => by
      simp only [evalArgs, ArgsRes.ok.injEq] at h
      obtain ⟨rfl, rfl, rfl⟩ := h
      exact ⟨hcc, rfl⟩
  | e :: es, H, H', vs, tr, hps, hcc, h => by
      simp only [Expr.pendingSafeList, Bool.and_eq_true] at hps
      have hx := eval_exact M hp fuel H φ e hcc hps.1
      cases he : eval M fuel P H φ e with
      | ok H₁ v tr₁ =>
          rw [he] at hx
          obtain ⟨_, c₁, v₁, _⟩ := hx
          simp only [evalArgs, he] at h
          split at h
          · rename_i H₂ vs₂ tr₂ h₂
            simp only [ArgsRes.ok.injEq] at h
            obtain ⟨rfl, rfl, rfl⟩ := h
            obtain ⟨c₂, v₂⟩ := evalArgs_cc hp hps.2 c₁ h₂
            exact ⟨c₂, by simp [Contents.ofVals, Contents.copyClosedList, v₁, v₂]⟩
          · cases h
      | _ => simp [evalArgs, he] at h

/-- **An argument list that finishes, losslessly** (§6.2's `…( v̄, E, ē )`):
each member is pushed, simulated, and plugged back into the list (helper). -/
theorem evalArgs_msimOk (hp : P.pendingSafe = true) (IH : MSimIH M P fuel) (t : ArgsTag) :
    ∀ (es : List Expr) (H : Store) (vs₀ : List Val), Expr.pendingSafeList es = true →
      StoreCC P.decls H →
      ∀ H' vs tr', evalArgs (fun H e => eval M fuel P H φ e) H es = .ok H' vs tr' →
        ∀ K tr, MSteps M P (.run H φ K (.args t vs₀ es) tr)
          (.run H' φ K (.args t (vs₀ ++ vs) []) (tr ++ tr'))
  | [], H, vs₀, _, _, H', vs, tr', h, K, tr => by
      simp only [evalArgs, ArgsRes.ok.injEq] at h
      obtain ⟨rfl, rfl, rfl⟩ := h
      simpa using MSteps.refl _
  | e :: es, H, vs₀, hps, hcc, H', vs, tr', h, K, tr => by
      simp only [Expr.pendingSafeList, Bool.and_eq_true] at hps
      have hx := eval_exact M hp fuel H φ e hcc hps.1
      have h₁ := IH H φ e hcc hps.1
      cases he : eval M fuel P H φ e with
      | ok H₁ v tr₁ =>
          rw [he] at hx h₁
          obtain ⟨_, c₁, _, _⟩ := hx
          simp only [evalArgs, he] at h
          split at h
          · rename_i H₂ vs₂ tr₂ h₂
            simp only [ArgsRes.ok.injEq] at h
            obtain ⟨rfl, rfl, rfl⟩ := h
            have hpush : MSteps M P (.run H φ K (.args t vs₀ (e :: es)) tr)
                (.run H φ (.args t vs₀ es :: K) (.eval e) tr) :=
              MSteps.single .argsPush (ledger_le_run0 fun a => by
                simp only [Focus.own, Kont.own, stackOwn, List.flatMap_cons, List.count_append,
                  List.count_nil]
                omega)
            have hplug : MSteps M P (.run H₁ φ (.args t vs₀ es :: K) (.ret v) (tr ++ tr₁))
                (.run H₁ φ K (.args t (vs₀ ++ [v]) es) (tr ++ tr₁)) :=
              MSteps.single .argsPlug (ledger_le_run0 fun a => by
                simp only [Focus.own, Kont.own, stackOwn, List.flatMap_cons,
                  Contents.ownList_ofVals_snoc, List.count_append]
                omega)
            have hrest := evalArgs_msimOk hp IH t es H₁ (vs₀ ++ [v]) hps.2 c₁ _ _ _ h₂ K (tr ++ tr₁)
            simp only [List.append_assoc, List.singleton_append] at hrest
            exact hpush.trans ((h₁ _ tr).trans (hplug.trans hrest))
          · cases h
      | _ => simp [evalArgs, he] at h

/-- **An argument list that aborts, losslessly**, where only its first member
may unwind (`pendingSafe`): nothing is pending when the first one does, and
the list's tag holds nothing; a later member's abort is a trap, a refusal or
exhausted fuel (helper). -/
theorem evalArgs_msimAbort (IH : MSimIH M P fuel) (t : ArgsTag)
    (ht : t.own P.decls = []) :
    ∀ (es : List Expr) (H : Store), Expr.pendingSafeList es = true →
      Expr.quietList es.tail = true → StoreCC P.decls H →
      ∀ r, evalArgs (fun H e => eval M fuel P H φ e) H es = .abort r →
        MSim M P φ (argsConf H φ t [] es) r
  | [], H, _, _, _, r, h => by simp [evalArgs] at h
  | e :: es, H, hps, hql, hcc, r, h => by
      simp only [Expr.pendingSafeList, Bool.and_eq_true] at hps
      simp only [List.tail] at hql
      have h₁ := IH H φ e hcc hps.1
      have hpush : ∀ K tr, MSteps M P (argsConf H φ t [] (e :: es) K tr)
          (evalConf H φ e (.args t [] es :: K) tr) := fun K tr =>
        MSteps.single .argsPush (ledger_le_run0 fun a => by
          simp only [Focus.own, Kont.own, stackOwn, List.flatMap_cons, List.count_append, ht,
            Contents.ofVals, Contents.ownList, List.count_nil]
          omega)
      cases he : eval M fuel P H φ e with
      | ok H₁ v tr₁ =>
          simp only [evalArgs, he] at h
          split at h
          · cases h
          · rename_i r' h₂
            simp only [ArgsRes.abort.injEq] at h
            subst h
            have hq : r'.NoRet ∧ r'.NoBrk :=
              ⟨evalArgs_noRet H₁ (fun H' e' hm =>
                  (eval_quiet M P fuel H' φ e').1 (Expr.quietList_mem hql hm).1) r' h₂,
                evalArgs_noBrk H₁ (fun H' e' hm =>
                  (eval_quiet M P fuel H' φ e').2 (Expr.quietList_mem hql hm).2) r' h₂⟩
            refine MSim.of_quiet ⟨EvalRes.withTrace_noRet hq.1, EvalRes.withTrace_noBrk hq.2⟩ ?_
            intro H' v' tr' hok
            cases r' <;> simp [EvalRes.withTrace] at hok
            exact evalArgs_abort_ne_ok h₂ _ _ _ rfl
      | _ =>
          simp only [evalArgs, he, ArgsRes.abort.injEq] at h
          subst h
          rw [he] at h₁
          exact MSim.lift (F := .args t [] es) (fun _ => ⟨rfl, rfl⟩)
            (by simp [Kont.own, ht, Contents.ofVals, Contents.ownList]) hpush h₁ (by simp)

/-! ## The forms, losslessly -/

/-- A value produced where the store is, the stack untouched, loses nothing:
the source held no value in focus (helper). -/
theorem MSteps.toValue {H : Store} {K : List Kont} {tr : List Event} {e : Expr} {v : Val}
    (s : Step M P (.run H φ K (.eval e) tr) (.run H φ K (.ret v) tr)) :
    MSteps M P (.run H φ K (.eval e) tr) (.run H φ K (.ret v) tr) :=
  MSteps.single s (ledger_le_run0 fun a => by simp only [Focus.own, List.count_nil]; omega)

/-- An enter step of §6.2's (Search) pushing a frame that holds nothing
(helper). -/
theorem MSteps.enter {H : Store} {K : List Kont} {tr : List Event} {e e' : Expr} {F : Kont}
    (hF : F.own P.decls = [])
    (s : Step M P (.run H φ K (.eval e) tr) (.run H φ (F :: K) (.eval e') tr)) :
    MSteps M P (.run H φ K (.eval e) tr) (.run H φ (F :: K) (.eval e') tr) :=
  MSteps.single s (ledger_le_run0 fun a => by
    rw [stackOwn_cons_nil hF]; simp only [Focus.own, List.count_nil]; omega)

/-- An enter step into a list context whose tag holds nothing (helper). -/
theorem MSteps.enterArgs {H : Store} {K : List Kont} {tr : List Event} {e : Expr} {t : ArgsTag}
    {es : List Expr} (ht : t.own P.decls = [])
    (s : Step M P (.run H φ K (.eval e) tr) (.run H φ K (.args t [] es) tr)) :
    MSteps M P (.run H φ K (.eval e) tr) (.run H φ K (.args t [] es) tr) :=
  MSteps.single s (ledger_le_run0 fun a => by
    simp only [Focus.own, ht, Contents.ofVals, Contents.ownList, List.append_nil, List.count_nil]
    omega)

/-- §6.4's operator frames, losslessly: the operand and the frame hold
nothing owned — the operator's operands are scalars (helper). -/
theorem OpRes.msim {H : Store} {F : Kont} {v : Val} (o : OpRes)
    (hv : ∀ K tr v', o = .val v' →
      Step M P (.run H φ (F :: K) (.ret v) tr) (.run H φ K (.ret v') tr))
    (hown : ∀ v', o = .val v' → F.own P.decls = [] ∧ v.own P.decls = []) :
    MSim M P φ (fun K tr => .run H φ (F :: K) (.ret v) tr) (o.toRes H) := by
  cases o with
  | val v' =>
      intro K tr
      obtain ⟨h₁, h₂⟩ := hown v' rfl
      simpa using MSteps.single (hv K tr v' rfl) (ledger_le_run0 fun a => by
        rw [stackOwn_cons_nil h₁]; simp only [Focus.own, h₂, List.count_nil]; omega)
  | trap κ => trivial
  | confused => trivial

/-- (D-Use-Declared-Linear), (D-Use-Copy), (D-Use-Move) §6.3 (helper). -/
theorem msim_use (hcc : StoreCC P.decls H) (p : Place) :
    MSim M P φ (evalConf H φ (.use p)) (eval M (fuel + 1) P H φ (.use p)) := by
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
                exact MSteps.single (.useDeclared hroot hplan hcd (destructure_plain hd) hv hw)
                  (ledger_le_run fun a => by
                    have := destructure_count hcc hc hcd hd hv hw a
                    simp only [Focus.own, List.count_nil]; omega)
      · rename_i hplan
        split
        · trivial
        · rename_i sub hsub
          split
          · trivial
          · rename_i v hv
            split
            · rename_i hcopy
              intro K tr; simpa using MSteps.toValue (.useCopy hroot hplan hsub hv hcopy)
            · rename_i hcopy
              split
              · trivial
              · rename_i c' hw
                intro K tr
                simpa using MSteps.single (.useMove hroot hplan hsub hv hcopy hw)
                  (ledger_le_run0 fun a => by
                    have := move_count hcc hc hsub hw hv a
                    simp only [Focus.own, List.count_nil]; omega)

/-- §6.11's `@drop` at a constant place (helper). -/
theorem msim_drop (hcc : StoreCC P.decls H) (p : Place) :
    MSim M P φ (evalConf H φ (.drop p)) (eval M (fuel + 1) P H φ (.drop p)) := by
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
                  exact MSteps.single (.dropDeclared hroot hplan hcd (destructure_plain hd) hl hw)
                    (ledger_le_run fun a => by
                      have := dropDeclared_count hcc hc hcd hd hl hw a
                      simp only [Focus.own, Val.own_unit, List.count_nil]; omega)
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
                intro K tr; simpa using MSteps.toValue (.dropCopy hroot hplan hsub hcopy)
              · rename_i hcopy
                split
                · trivial
                · rename_i c' hw
                  intro K tr
                  exact MSteps.single (.dropMove hroot hplan hsub hcopy hdrop hw)
                    (ledger_le_run fun a => by
                      have := dropPlace_count hcc hc hsub hdrop hw a
                      simp only [Focus.own, Val.own_unit, List.count_nil]; omega)

/-- §6.4's binary operators: the right operand runs under a held left value,
which `pendingSafe` keeps from unwinding (helper). -/
theorem msim_binop (hp : P.pendingSafe = true) (IH : MSimIH M P fuel) (hcc : StoreCC P.decls H)
    (op : BinOp) (e₁ e₂ : Expr) (he : (Expr.binop op e₁ e₂).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.binop op e₁ e₂)) (eval M (fuel + 1) P H φ (.binop op e₁ e₂)) := by
  simp only [Expr.pendingSafe, Bool.and_eq_true, Bool.not_eq_eq_eq_not, Bool.not_true,
    Expr.unwinds, Bool.or_eq_false_iff] at he
  simp only [eval]
  refine MSim.andThen (F := .binopL op e₂) (fun _ => ⟨rfl, rfl⟩) rfl
    (fun _ _ => MSteps.enter rfl .binopEnter) (IH H φ e₁ hcc he.1.1) ?_
  intro H₁ v₁ tr₁ hr
  have hx := eval_exact M hp fuel H φ e₁ hcc he.1.1
  rw [hr] at hx
  obtain ⟨_, c₁, _, _⟩ := hx
  refine MSim.andThenHeld (F := .binopR op v₁)
    (fun _ _ => MSteps.single .binopMid (ledger_le_run0 fun a => by
      simp only [Focus.own, Kont.own, stackOwn, List.flatMap_cons, List.count_append,
        List.count_nil]
      omega))
    ⟨(eval_quiet M P fuel H₁ φ e₂).1 he.2.1, (eval_quiet M P fuel H₁ φ e₂).2 he.2.2⟩
    (IH H₁ φ e₂ c₁ he.1.2) ?_
  intro H₂ v₂ _ _
  exact OpRes.msim _ (fun _ _ _ h => .binop h) (fun v' h => by
    obtain ⟨s₁, s₂⟩ := evalBinOp_val_args h
    exact ⟨(Val.scalar_own s₁).1, (Val.scalar_own s₂).1⟩)

/-- §6.4's unary operators (helper). -/
theorem msim_unop (IH : MSimIH M P fuel) (hcc : StoreCC P.decls H) (op : UnOp) (e : Expr)
    (he : (Expr.unop op e).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.unop op e)) (eval M (fuel + 1) P H φ (.unop op e)) := by
  simp only [Expr.pendingSafe] at he
  simp only [eval]
  refine MSim.andThen (F := .unop op) (fun _ => ⟨rfl, rfl⟩) rfl
    (fun _ _ => MSteps.enter rfl .unopEnter) (IH H φ e hcc he) ?_
  intro H₁ v _ _
  exact OpRes.msim _ (fun _ _ _ h => .unop h)
    (fun v' h => ⟨rfl, (Val.scalar_own (evalUnOp_val_arg h)).1⟩)

/-- (D-Int-Cast) and its trap (helper). -/
theorem msim_intCast (IH : MSimIH M P fuel) (hcc : StoreCC P.decls H) (w : IntWidth) (sg : Sign)
    (e : Expr) (he : (Expr.intCast w sg e).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.intCast w sg e)) (eval M (fuel + 1) P H φ (.intCast w sg e)) := by
  simp only [Expr.pendingSafe] at he
  simp only [eval]
  refine MSim.andThen (F := .intCast w sg) (fun _ => ⟨rfl, rfl⟩) rfl
    (fun _ _ => MSteps.enter rfl .intCastEnter) (IH H φ e hcc he) ?_
  intro H₁ v _ _
  exact OpRes.msim _ (fun _ _ _ h => .intCast h)
    (fun v' h => ⟨rfl, (Val.scalar_own (evalIntCast_val_arg h)).1⟩)

/-- §6.4's float intrinsics (helper). -/
theorem msim_fintrin (IH : MSimIH M P fuel) (hcc : StoreCC P.decls H) (k : FloatIntrin) (e : Expr)
    (he : (Expr.fintrin k e).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.fintrin k e)) (eval M (fuel + 1) P H φ (.fintrin k e)) := by
  simp only [Expr.pendingSafe] at he
  simp only [eval]
  refine MSim.andThen (F := .fintrin k) (fun _ => ⟨rfl, rfl⟩) rfl
    (fun _ _ => MSteps.enter rfl .fintrinEnter) (IH H φ e hcc he) ?_
  intro H₁ v _ _
  exact OpRes.msim _ (fun _ _ _ h => .fintrin h)
    (fun v' h => ⟨rfl, (Val.scalar_own (evalFintrin_val_arg h)).1⟩)

/-- `@dbg` (§6.12): the operand is observable, so a scalar (helper). -/
theorem msim_dbg (IH : MSimIH M P fuel) (hcc : StoreCC P.decls H) (e : Expr)
    (he : (Expr.dbg e).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.dbg e)) (eval M (fuel + 1) P H φ (.dbg e)) := by
  simp only [Expr.pendingSafe] at he
  simp only [eval]
  refine MSim.andThen (F := .dbg) (fun _ => ⟨rfl, rfl⟩) rfl
    (fun _ _ => MSteps.enter rfl .dbgEnter) (IH H φ e hcc he) ?_
  intro H₁ v _ _
  split
  · rename_i hobs
    intro K tr
    exact MSteps.single (.dbg hobs) (ledger_le_run fun a => by
      simp only [Focus.own, Kont.own, stackOwn, List.flatMap_cons, List.count_append,
        (Val.scalar_own (D := P.decls) (Val.observable_scalar hobs)).1, List.count_nil]
      omega)
  · trivial

/-- An argument-list form's prefix: the enter step, and the list run to its
redex or its abort, losslessly (helper). -/
theorem msim_argsForm (hp : P.pendingSafe = true) (IH : MSimIH M P fuel)
    (hcc : StoreCC P.decls H) {e : Expr} {t : ArgsTag} (ht : t.own P.decls = []) {es : List Expr}
    (hps : Expr.pendingSafeList es = true) (hql : Expr.quietList es.tail = true)
    (hent : ∀ K tr, MSteps M P (evalConf H φ e K tr) (argsConf H φ t [] es K tr))
    {k : Store → List Val → List Event → EvalRes}
    (hk : ∀ H₁ vs tr₁, evalArgs (fun H e => eval M fuel P H φ e) H es = .ok H₁ vs tr₁ →
      MSim M P φ (argsConf H₁ φ t vs []) (k H₁ vs tr₁)) :
    MSim M P φ (evalConf H φ e)
      (match evalArgs (fun H e => eval M fuel P H φ e) H es with
       | .abort r => r
       | .ok H₁ vs tr₁ => (k H₁ vs tr₁).withTrace tr₁) := by
  split
  · rename_i r hr; exact MSim.pre hent (evalArgs_msimAbort IH t ht es H hps hql hcc r hr)
  · rename_i H₁ vs tr₁ hr
    exact MSim.withTrace (C₂ := argsConf H₁ φ t vs [])
      (fun K tr => (hent K tr).trans (by simpa using evalArgs_msimOk hp IH t es H [] hps hcc _ _ _ hr K tr))
      (hk H₁ vs tr₁ hr)

/-- (D-Struct) §6.5: the identity is minted as `introVal` mints it, whose
copy-closure monitor keeps every member's identities in the new value
(helper). -/
theorem msim_mkStruct (hp : P.pendingSafe = true) (IH : MSimIH M P fuel)
    (hcc : StoreCC P.decls H) (s : Nat) (args : List Expr)
    (he : (Expr.mkStruct s args).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.mkStruct s args)) (eval M (fuel + 1) P H φ (.mkStruct s args)) := by
  simp only [Expr.pendingSafe, Bool.and_eq_true] at he
  simp only [eval]
  refine msim_argsForm hp IH hcc (t := .struct s) rfl he.1 he.2
    (fun _ _ => MSteps.enterArgs rfl .structEnter) (fun H₁ vs tr₁ _ => ?_)
  split
  · trivial
  · rename_i sd hsd
    split
    · rename_i hlen
      simp only [introVal]
      split
      · rename_i hv
        intro K tr
        rw [List.append_nil]
        exact MSteps.single (.mkStruct hsd hlen) (ledger_le_run0 fun a => by
          have := Contents.own_struct_ge hv a
          simp only [Focus.own, ArgsTag.own, storeOwn_append, List.nil_append, List.count_append]
          simp only [storeOwn, Cell.own, List.flatMap_cons, List.flatMap_nil, List.append_nil,
            List.count_nil]
          simp only [Val.own, Contents.ofVal] at this ⊢
          omega)
      · trivial
    · trivial

/-- (D-Enum-Intro) §6.6 (helper). -/
theorem msim_mkEnum (hp : P.pendingSafe = true) (IH : MSimIH M P fuel)
    (hcc : StoreCC P.decls H) (en k : Nat) (args : List Expr)
    (he : (Expr.mkEnum en k args).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.mkEnum en k args)) (eval M (fuel + 1) P H φ (.mkEnum en k args)) := by
  simp only [Expr.pendingSafe, Bool.and_eq_true] at he
  simp only [eval]
  refine msim_argsForm hp IH hcc (t := .enum en k) rfl he.1 he.2
    (fun _ _ => MSteps.enterArgs rfl .enumEnter) (fun H₁ vs tr₁ _ => ?_)
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
        · rename_i hv
          intro K tr
          rw [List.append_nil]
          exact MSteps.single (.mkEnum hed hTs hlen) (ledger_le_run0 fun a => by
            have := (Contents.enum_payload hv a).1
            simp only [Focus.own, ArgsTag.own, storeOwn_append, List.nil_append, List.count_append]
            simp only [storeOwn, Cell.own, List.flatMap_cons, List.flatMap_nil, List.append_nil,
              List.count_nil]
            simp only [Val.own, Contents.ofVal] at this ⊢
            omega)
        · trivial
      · trivial

/-- (D-Array) §6.5 (helper). -/
theorem msim_mkArray (hp : P.pendingSafe = true) (IH : MSimIH M P fuel)
    (hcc : StoreCC P.decls H) (T : Ty) (args : List Expr)
    (he : (Expr.mkArray T args).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.mkArray T args)) (eval M (fuel + 1) P H φ (.mkArray T args)) := by
  simp only [Expr.pendingSafe, Bool.and_eq_true] at he
  simp only [eval]
  refine msim_argsForm hp IH hcc (t := .array T) rfl he.1 he.2
    (fun _ _ => MSteps.enterArgs rfl .arrayEnter) (fun H₁ vs tr₁ _ => ?_)
  simp only [introVal]
  split
  · rename_i hv
    intro K tr
    rw [List.append_nil]
    exact MSteps.single .mkArray (ledger_le_run0 fun a => by
      have := Contents.own_array_ge hv a
      simp only [Focus.own, ArgsTag.own, storeOwn_append, List.nil_append, List.count_append]
      simp only [storeOwn, Cell.own, List.flatMap_cons, List.flatMap_nil, List.append_nil,
        List.count_nil]
      simp only [Val.own, Contents.ofVal] at this ⊢
      omega)
  · trivial

/-- The repeat form (`7.1:39`): its operand is `Copy`, so owns nothing
(helper). -/
theorem msim_repeat (IH : MSimIH M P fuel) (hcc : StoreCC P.decls H) (T : Ty) (e : Expr) (n : Nat)
    (he : (Expr.repeatArray T e n).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.repeatArray T e n)) (eval M (fuel + 1) P H φ (.repeatArray T e n)) := by
  simp only [Expr.pendingSafe] at he
  simp only [eval]
  refine MSim.andThen (F := .repeatArray T n) (fun _ => ⟨rfl, rfl⟩) rfl
    (fun _ _ => MSteps.enter rfl .repeatEnter) (IH H φ e hcc he) ?_
  intro H₁ v _ _
  split
  · rename_i hcopy
    simp only [introVal]
    split
    · intro K tr
      rw [List.append_nil]
      exact MSteps.single (.repeatArray hcopy) (ledger_le_run0 fun a => by
        simp only [Focus.own, Kont.own, stackOwn, List.flatMap_cons, storeOwn_append,
          Val.own_of_copy hcopy, List.count_nil, List.nil_append, List.count_append]
        simp only [storeOwn, Cell.own, List.flatMap_cons, List.flatMap_nil]
        omega)
    · trivial
  · trivial

/-- (D-Index)/(D-Index-Trap) §6.5 and (D-Use-Untrackable-Dynamic-Copy) §6.3,
from the index list's context: the indices are integers (helper). -/
theorem msim_indexRead_args (hp : P.pendingSafe = true) (IH : MSimIH M P fuel)
    (hcc : StoreCC P.decls H) (p : Place) (idx : List Expr) (πs : List (List Nat))
    (he : (Expr.indexRead p idx πs).pendingSafe = true) :
    MSim M P φ (argsConf H φ (.indexRead p πs) [] idx)
      (eval M (fuel + 1) P H φ (.indexRead p idx πs)) := by
  simp only [Expr.pendingSafe, Bool.and_eq_true] at he
  simp only [eval]
  split
  · rename_i r hr; exact evalArgs_msimAbort IH (.indexRead p πs) rfl idx H he.1 he.2 hcc r hr
  · rename_i H₁ vs tr₁ hr
    refine MSim.withTrace (C₂ := argsConf H₁ φ (.indexRead p πs) vs [])
      (fun K tr => by simpa using evalArgs_msimOk hp IH _ idx H [] he.1 hcc _ _ _ hr K tr) ?_
    split
    · trivial
    · trivial
    · rename_i ℓ c sub ρ hd
      obtain ⟨is, his⟩ := dynPlace_ints (fun w h => by rw [hd] at h; cases h)
      split
      · trivial
      · rename_i leaf hleaf
        split
        · trivial
        · rename_i v hv
          split
          · rename_i hcopy
            intro K tr
            simpa using MSteps.single (.indexRead hd hleaf hv hcopy) (ledger_le_run0 fun a => by
              simp only [Focus.own, ArgsTag.own, Val.ints_own (D := P.decls) his, List.append_nil,
                List.count_nil]
              omega)
          · trivial

/-- (D-Index) at an expression in focus (helper). -/
theorem msim_indexRead (hp : P.pendingSafe = true) (IH : MSimIH M P fuel)
    (hcc : StoreCC P.decls H) (p : Place) (idx : List Expr) (πs : List (List Nat))
    (he : (Expr.indexRead p idx πs).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.indexRead p idx πs)) (eval M (fuel + 1) P H φ (.indexRead p idx πs)) :=
  MSim.pre (fun _ _ => MSteps.enterArgs rfl .indexReadEnter) (msim_indexRead_args hp IH hcc p idx πs he)

/-- §6.11's `@drop` at a `Copy` place below a dynamic index (helper). -/
theorem msim_indexDrop (hp : P.pendingSafe = true) (IH : MSimIH M P fuel)
    (hcc : StoreCC P.decls H) (p : Place) (idx : List Expr) (πs : List (List Nat))
    (he : (Expr.indexDrop p idx πs).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.indexDrop p idx πs)) (eval M (fuel + 2) P H φ (.indexDrop p idx πs)) := by
  simp only [Expr.pendingSafe, Bool.and_eq_true] at he
  simp only [eval]
  have hent : ∀ K tr, MSteps M P (evalConf H φ (.indexDrop p idx πs) K tr)
      (argsConf H φ (.indexDrop p πs) [] idx K tr) := fun _ _ => MSteps.enterArgs rfl .indexDropEnter
  split
  · rename_i r hr
    have hne := evalArgs_abort_ne_ok hr
    have : r.andThen (fun H' _ => .ok H' .unit []) = r := by
      cases r <;> simp_all [EvalRes.andThen]
    rw [this]
    exact MSim.pre hent (evalArgs_msimAbort IH (.indexDrop p πs) rfl idx H he.1 he.2 hcc r hr)
  · rename_i H₁ vs tr₁ hr
    rw [EvalRes.withTrace_andThen]
    refine MSim.withTrace (C₂ := argsConf H₁ φ (.indexDrop p πs) vs [])
      (fun K tr => (hent K tr).trans
        (by simpa using evalArgs_msimOk hp IH _ idx H [] he.1 hcc _ _ _ hr K tr)) ?_
    split
    · trivial
    · trivial
    · rename_i ℓ c sub ρ hd
      obtain ⟨is, his⟩ := dynPlace_ints (fun w h => by rw [hd] at h; cases h)
      split
      · trivial
      · rename_i leaf hleaf
        split
        · trivial
        · rename_i v hv
          split
          · rename_i hcopy
            rw [← Contents.mult_toVal _ _ _ hv] at hcopy
            intro K tr
            simpa [EvalRes.andThen] using MSteps.single (.indexDrop hd hleaf hv hcopy)
              (ledger_le_run0 (tr := tr) fun a => by
                simp only [Focus.own, ArgsTag.own, Val.ints_own (D := P.decls) his, List.append_nil,
                  Val.own_unit, List.count_nil]
                omega)
          · trivial

/-- (D-Assign) §6.8 below a dynamic index: the right-hand side is held while
the indices run, which `pendingSafe` keeps from unwinding (helper). -/
theorem msim_indexWrite (hp : P.pendingSafe = true) (IH : MSimIH M P fuel)
    (hcc : StoreCC P.decls H) (p : Place) (idx : List Expr) (πs : List (List Nat)) (e : Expr)
    (he : (Expr.indexWrite p idx πs e).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.indexWrite p idx πs e))
      (eval M (fuel + 1) P H φ (.indexWrite p idx πs e)) := by
  simp only [Expr.pendingSafe, Bool.and_eq_true] at he
  simp only [eval]
  refine MSim.andThen (F := .indexWriteRhs p idx πs) (fun _ => ⟨rfl, rfl⟩) rfl
    (fun _ _ => MSteps.enter rfl .indexWriteEnter) (IH H φ e hcc he.1.1) ?_
  intro H₁ v tr₁ hr₁
  have hx := eval_exact M hp fuel H φ e hcc he.1.1
  rw [hr₁] at hx
  obtain ⟨_, c₁, _, _⟩ := hx
  have hent : ∀ K tr, MSteps M P (.run H₁ φ (.indexWriteRhs p idx πs :: K) (.ret v) tr)
      (argsConf H₁ φ (.indexWrite p πs v) [] idx K tr) := fun _ _ =>
    MSteps.single .indexWriteRhs (ledger_le_run0 fun a => by
      simp only [Focus.own, Kont.own, ArgsTag.own, stackOwn, List.flatMap_cons, Contents.ofVals,
        Contents.ownList, List.append_nil, List.count_append, List.count_nil]
      omega)
  split
  · rename_i r hr
    have hq : r.NoRet ∧ r.NoBrk :=
      ⟨evalArgs_noRet H₁ (fun H' e' hm =>
          (eval_quiet M P fuel H' φ e').1 (Expr.quietList_mem he.2 hm).1) r hr,
        evalArgs_noBrk H₁ (fun H' e' hm =>
          (eval_quiet M P fuel H' φ e').2 (Expr.quietList_mem he.2 hm).2) r hr⟩
    exact MSim.of_quiet hq (evalArgs_abort_ne_ok hr)
  · rename_i H₂ vs tr₂ hr
    have c₂ := (evalArgs_cc hp he.1.2 c₁ hr).1
    refine MSim.withTrace (C₂ := argsConf H₂ φ (.indexWrite p πs v) vs [])
      (fun K tr => (hent K tr).trans
        (by simpa using evalArgs_msimOk hp IH _ idx H₁ [] he.1.2 c₁ _ _ _ hr K tr)) ?_
    split
    · trivial
    · trivial
    · rename_i ℓ c sub ρ hd
      obtain ⟨hc, hsub⟩ := dynPlace_at hd
      obtain ⟨is, his⟩ := dynPlace_ints (fun w h => by rw [hd] at h; cases h)
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
                · rename_i hc'
                  intro K tr
                  exact MSteps.single (.indexWrite hd hold hdrop hw₁ hw₂) (ledger_le_run fun a => by
                    have := assignDyn_count c₂ hc hsub hold hdrop hw₁ hw₂ hc' a
                    simp only [Focus.own, ArgsTag.own, Val.ints_own (D := P.decls) his,
                      List.append_nil, Val.own_unit, List.count_nil]
                    omega)
                · trivial

/-- (D-Match) §6.6: the payload moves into the arm's cells and the shell is
consumed; the arm runs under its `endscope`, which (D-EndScope) closes
(helper). -/
theorem msim_match (hp : P.pendingSafe = true) (IH : MSimIH M P fuel) (hcc : StoreCC P.decls H)
    (scrut : Expr) (arms : List Expr) (he : (Expr.«match» scrut arms).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.«match» scrut arms)) (eval M (fuel + 1) P H φ (.«match» scrut arms)) := by
  simp only [Expr.pendingSafe, Bool.and_eq_true] at he
  simp only [eval]
  refine MSim.andThen (F := .«match» arms) (fun _ => ⟨rfl, rfl⟩) rfl
    (fun _ _ => MSteps.enter rfl .matchEnter) (IH H φ scrut hcc he.1) ?_
  intro H₀ v tr₀ hr₀
  have hx := eval_exact M hp fuel H φ scrut hcc he.1
  rw [hr₀] at hx
  obtain ⟨-, c₀, cv₀, -⟩ := hx
  split
  · rename_i en k i vs
    split
    · trivial
    · rename_i body hbody
      have hv : (Contents.enum en k i (Contents.ofVals vs)).copyClosed P.decls = true := cv₀
      have hpay := Contents.enum_payload hv
      refine MSim.withTrace (C₂ := fun K tr => evalConf (mintParams H₀ vs).1
          { env := (mintParams H₀ vs).2.reverse ++ φ.env, scope := φ.scope ++ (mintParams H₀ vs).2 }
          body (.endscope (mintParams H₀ vs).2 :: K) tr)
        (fun _ _ => MSteps.single (.«match» hbody rfl) (ledger_le_run fun a => by
          have := matchConsume_exact hv a
          rw [storeOwn_mintParams, stackOwn_cons_nil (F := .«match» arms) rfl,
            stackOwn_cons_nil (F := .endscope _) rfl]
          simp only [Focus.own, List.count_append, List.count_nil]
          simp only [Val.own, Contents.ofVal] at this ⊢
          omega)) ?_
      have hcm : StoreCC P.decls (mintParams H₀ vs).1 := c₀.mintParams (hpay 0).2
      have hb := eval_exact M hp fuel _
        { env := (mintParams H₀ vs).2.reverse ++ φ.env, scope := φ.scope ++ (mintParams H₀ vs).2 }
        body hcm (Expr.pendingSafeList_mem he.2 (List.mem_of_getElem? hbody))
      refine MSim.andThen (F := .endscope (mintParams H₀ vs).2) (fun _ => ⟨rfl, rfl⟩) rfl
        (fun _ _ => .refl _)
        (IH _ _ body hcm (Expr.pendingSafeList_mem he.2 (List.mem_of_getElem? hbody))) ?_
      intro H₂ v₂ _ hr₂
      split
      · trivial
      · rename_i H₃ evs hu
        intro K tr
        have hs := Step.endScope (M := M) (P := P) (K := K) (tr := tr) (v := v₂)
          (φ := { env := (mintParams H₀ vs).2.reverse ++ φ.env,
                  scope := φ.scope ++ (mintParams H₀ vs).2 }) (unwindLocs_plain hu)
        rw [Frame.popScope_push] at hs
        exact MSteps.single hs (ledger_le_run fun a => by
          have := plainUnwind_count (unwindLocs_plain hu) a
          rw [stackOwn_cons_nil rfl]
          omega)
  · trivial

/-- (D-Let) §6.7, then (D-EndScope) (helper). -/
theorem msim_letIn (hp : P.pendingSafe = true) (IH : MSimIH M P fuel) (hcc : StoreCC P.decls H)
    (m : Bool) (e₁ e₂ : Expr) (he : (Expr.letIn m e₁ e₂).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.letIn m e₁ e₂)) (eval M (fuel + 1) P H φ (.letIn m e₁ e₂)) := by
  simp only [Expr.pendingSafe, Bool.and_eq_true] at he
  simp only [eval]
  refine MSim.andThen (F := .letIn e₂) (fun _ => ⟨rfl, rfl⟩) rfl
    (fun _ _ => MSteps.enter rfl .letEnter) (IH H φ e₁ hcc he.1) ?_
  intro H₁ v₁ _ hr₁
  have hx := eval_exact M hp fuel H φ e₁ hcc he.1
  rw [hr₁] at hx
  obtain ⟨_, c₁, cv₁, _⟩ := hx
  have hc : StoreCC P.decls (H₁ ++ [.full (Contents.ofVal v₁)]) := c₁.append (StoreCC.single cv₁)
  refine MSim.andThen (F := .endscope [H₁.length]) (fun _ => ⟨rfl, rfl⟩) rfl
    (fun _ _ => MSteps.single .letBind (ledger_le_run0 fun a => by
      rw [stackOwn_cons_nil rfl, stackOwn_cons_nil rfl, storeOwn_append]
      simp only [storeOwn, Cell.own, List.flatMap_cons, List.flatMap_nil, List.append_nil,
        Focus.own, List.count_append, List.count_nil]
      simp only [Val.own]
      omega)) (IH _ _ e₂ hc he.2) ?_
  intro H₂ v₂ _ _
  split
  · trivial
  · rename_i H₃ evs hd
    intro K tr
    have hs := Step.endScope (M := M) (P := P) (K := K) (tr := tr) (v := v₂)
      (φ := { env := H₁.length :: φ.env, scope := φ.scope ++ [H₁.length] })
      (ℓs := [H₁.length]) (plainUnwind_single hd)
    simp only [List.length_singleton, Frame.popScope_let] at hs
    exact MSteps.single hs (ledger_le_run fun a => by
      have := plainUnwind_count (plainUnwind_single hd) a
      rw [stackOwn_cons_nil rfl]
      omega)

/-- (D-Assign) §6.8: `eval`'s copy-closure monitor passed, so the stored
value's identities stay counted (helper). -/
theorem msim_assign (hp : P.pendingSafe = true) (IH : MSimIH M P fuel) (hcc : StoreCC P.decls H)
    (p : Place) (e : Expr) (he : (Expr.assign p e).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.assign p e)) (eval M (fuel + 1) P H φ (.assign p e)) := by
  simp only [Expr.pendingSafe] at he
  simp only [eval]
  refine MSim.andThen (F := .assign p) (fun _ => ⟨rfl, rfl⟩) rfl
    (fun _ _ => MSteps.enter rfl .assignEnter) (IH H φ e hcc he) ?_
  intro H₁ v _ hr₁
  have hx := eval_exact M hp fuel H φ e hcc he
  rw [hr₁] at hx
  obtain ⟨_, c₁, _, _⟩ := hx
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
              · rename_i hc'
                intro K tr
                exact MSteps.single (.assign (rootCell_of hℓ hc) hold hdrop hw)
                  (ledger_le_run fun a => by
                    have := assign_count c₁ hc hold hdrop hw hc' a
                    rw [stackOwn_cons_nil rfl]
                    simp only [Focus.own, Val.own_unit, List.count_nil]
                    omega)
              · trivial

/-- (D-Seq) §6.7: a discarded temporary is ended by its `dropTemp` marker
(helper). -/
theorem msim_seq (hp : P.pendingSafe = true) (IH : MSimIH M P fuel) (hcc : StoreCC P.decls H)
    (e₁ e₂ : Expr) (he : (Expr.seq e₁ e₂).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.seq e₁ e₂)) (eval M (fuel + 1) P H φ (.seq e₁ e₂)) := by
  simp only [Expr.pendingSafe, Bool.and_eq_true] at he
  simp only [eval]
  refine MSim.andThen (F := .seq e₂) (fun _ => ⟨rfl, rfl⟩) rfl
    (fun _ _ => MSteps.enter rfl .seqEnter) (IH H φ e₁ hcc he.1) ?_
  intro H₁ v₁ _ hr₁
  have hx := eval_exact M hp fuel H φ e₁ hcc he.1
  rw [hr₁] at hx
  obtain ⟨_, c₁, _, _⟩ := hx
  split
  · trivial
  · rename_i hm
    split
    · trivial
    · rename_i evs hd
      have hne : v₁.mult P.decls ≠ .copy := by rw [hm]; exact nofun
      exact MSim.withTrace (C₂ := evalConf H₁ φ e₂)
        (fun _ _ => MSteps.single (.seqDrop hne hd) (ledger_le_run fun a => by
          rw [stackOwn_cons_nil rfl]
          simp only [Focus.own, freedIds, List.flatMap_cons, Event.freed, List.count_append,
            List.count_nil]
          omega))
        (IH H₁ φ e₂ c₁ he.2)
  · rename_i hm
    exact MSim.pre (fun _ _ => MSteps.single (.seqCopy hm) (ledger_le_run0 fun a => by
        rw [stackOwn_cons_nil rfl]
        simp only [Focus.own, Val.own_of_copy hm, List.count_nil]
        omega))
      (IH H₁ φ e₂ c₁ he.2)

/-- (D-If-T)/(D-If-F) §6.6 (helper). -/
theorem msim_ite (hp : P.pendingSafe = true) (IH : MSimIH M P fuel) (hcc : StoreCC P.decls H)
    (c e₁ e₂ : Expr) (he : (Expr.ite c e₁ e₂).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.ite c e₁ e₂)) (eval M (fuel + 1) P H φ (.ite c e₁ e₂)) := by
  simp only [Expr.pendingSafe, Bool.and_eq_true] at he
  simp only [eval]
  refine MSim.andThen (F := .ite e₁ e₂) (fun _ => ⟨rfl, rfl⟩) rfl
    (fun _ _ => MSteps.enter rfl .iteEnter) (IH H φ c hcc he.1.1) ?_
  intro H₀ v _ hr₀
  have hx := eval_exact M hp fuel H φ c hcc he.1.1
  rw [hr₀] at hx
  obtain ⟨_, c₀, _, _⟩ := hx
  have hb : ∀ b, ∀ K tr, IdLe ((Config.run H₀ φ (.ite e₁ e₂ :: K) (.ret (.bool b)) tr).ledger P.decls)
      ((Config.run H₀ φ K (.eval (if b then e₁ else e₂)) tr).ledger P.decls) := fun b K tr =>
    ledger_le_run0 fun a => by
      rw [stackOwn_cons_nil rfl]
      simp only [Focus.own, Val.own, Contents.ofVal, Contents.own, List.count_nil]
      omega
  split
  · rename_i b
    split
    · rename_i hb'
      subst hb'
      exact MSim.pre (fun K tr => MSteps.single .iteTrue (hb true K tr)) (IH H₀ φ e₁ c₀ he.1.2)
    · rename_i hb'
      simp only [Bool.not_eq_true] at hb'
      subst hb'
      exact MSim.pre (fun K tr => MSteps.single .iteFalse (hb false K tr)) (IH H₀ φ e₂ c₀ he.2)
  · trivial

/-- (D-Call) §6.9, then (D-Return-Value), or (D-Return)'s value caught at the
`call` frame (helper). -/
theorem msim_call (hp : P.pendingSafe = true) (IH : MSimIH M P fuel) (hcc : StoreCC P.decls H)
    (f : Nat) (args : List Expr) (he : (Expr.call f args).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.call f args)) (eval M (fuel + 1) P H φ (.call f args)) := by
  have hbody : ∀ (f : Nat) (fd : FnDef), P.fns[f]? = some fd → fd.body.pendingSafe = true :=
    fun f fd h => (List.all_eq_true.mp hp) fd (List.mem_of_getElem? h)
  simp only [Expr.pendingSafe, Bool.and_eq_true] at he
  simp only [eval]
  refine msim_argsForm hp IH hcc (t := .call f) rfl he.1 he.2
    (fun _ _ => MSteps.enterArgs rfl .callEnter) (fun H₁ vs tr₁ hr => ?_)
  obtain ⟨c₁, cv₁⟩ := evalArgs_cc hp he.1 hcc hr
  split
  · trivial
  · rename_i fd hfd
    split
    · rename_i hlen
      refine MSim.absorb (fun _ _ => MSteps.single (.call hfd hlen rfl) (ledger_le_run0 fun a => by
          rw [storeOwn_mintParams, stackOwn_cons_nil rfl]
          simp only [Focus.own, ArgsTag.own, List.nil_append, List.count_append, List.count_nil]
          omega))
        (IH _ _ fd.body (c₁.mintParams cv₁) (hbody f fd hfd)) ?_
      intro H₃ v _ hr₃
      split
      · trivial
      · rename_i H₄ evs hu
        intro K tr
        exact MSteps.single (.callReturn (unwindLocs_plain hu)) (ledger_le_run fun a => by
          have := plainUnwind_count (unwindLocs_plain hu) a
          rw [stackOwn_cons_nil rfl]
          omega)
    · trivial

/-- (D-Return) §6.9: the frames it discards hold nothing, by the unwinding
clause's premise (helper). -/
theorem msim_ret (IH : MSimIH M P fuel) (hcc : StoreCC P.decls H)
    (e : Expr) (he : (Expr.ret e).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.ret e)) (eval M (fuel + 1) P H φ (.ret e)) := by
  simp only [Expr.pendingSafe] at he
  simp only [eval]
  refine MSim.andThen (F := .ret) (fun _ => ⟨rfl, rfl⟩) rfl
    (fun _ _ => MSteps.enter rfl .retEnter) (IH H φ e hcc he) ?_
  intro H₁ v _ _
  split
  · trivial
  · rename_i H₂ evs hu
    intro K tr φs K' hK hs
    exact MSteps.single (.ret hK (unwindLocs_plain hu)) (ledger_le_run fun a => by
      have := plainUnwind_count (unwindLocs_plain hu) a
      have := hs a
      rw [stackOwn_cons_nil rfl]
      omega)

/-- (D-Break) §6.10 (helper). -/
theorem msim_brk : MSim M P φ (evalConf H φ .brk) (eval M (fuel + 1) P H φ .brk) := by
  simp only [eval]
  intro K tr φs K' H' evs hK hs hu
  simpa using MSteps.single (.brk hK hu) (ledger_le_run fun a => by
    have := plainUnwind_count hu a
    have := hs a
    simp only [Focus.own, Val.own_unit, List.count_nil]
    omega)

/-- (D-Loop-Enter), (D-Loop-Iter) and (D-Break)'s landing §6.10 (helper). -/
theorem msim_loop (hp : P.pendingSafe = true) (IH : MSimIH M P fuel) (hcc : StoreCC P.decls H)
    (e : Expr) (he : (Expr.loop e).pendingSafe = true) :
    MSim M P φ (evalConf H φ (.loop e)) (eval M (fuel + 1) P H φ (.loop e)) := by
  have hbe : e.pendingSafe = true := by simpa [Expr.pendingSafe] using he
  simp only [eval]
  have hent : ∀ K tr, MSteps M P (evalConf H φ (.loop e) K tr) (evalConf H φ e (.loop e φ :: K) tr) :=
    fun _ _ => MSteps.enter rfl .loopEnter
  have h₁ := IH H φ e hcc hbe
  have hx := eval_exact M hp fuel H φ e hcc hbe
  cases hr : eval M fuel P H φ e with
  | ok H₁ v tr₁ =>
      rw [hr] at h₁ hx
      obtain ⟨_, c₁, _, _⟩ := hx
      cases v with
      | unit => ?_
      | _ => trivial
      simp only
      have hpeel : MSim M P φ (evalConf H₁ φ e ∘ (Kont.loop e φ :: ·))
          (eval M fuel P H₁ φ (.loop e)) :=
        MSim.peel (fun _ _ => .loopEnter) (fun _ _ => trivial) (IH H₁ φ (.loop e) c₁ he)
      refine MSim.withTrace (C₂ := evalConf H₁ φ e ∘ (Kont.loop e φ :: ·)) (fun K tr => ?_) hpeel
      have hit : Step M P (.run H₁ φ (.loop e φ :: K) (.ret .unit) (tr ++ tr₁))
          (.run H₁ φ (.loop e φ :: K) (.eval e) (tr ++ tr₁ ++ [])) :=
        .loopIter (by simp [plainUnwind])
      simp only [List.append_nil] at hit
      exact (hent K tr).trans ((h₁ _ tr).trans (MSteps.single hit (ledger_le_run0 fun a => by
        simp only [Focus.own, Val.own_unit, List.count_nil]
        omega)))
  | broke H₁ sc tr₁ =>
      rw [hr] at h₁
      simp only
      split
      · trivial
      · rename_i H₂ evs hu
        intro K tr
        have := h₁ (.loop e φ :: K) tr φ K H₂ evs rfl
          (by rw [stackOwn_cons_nil rfl]; exact IdLe.refl' _) (unwindLocs_plain hu)
        simp only [List.append_assoc] at this ⊢
        exact (hent K tr).trans this
  | returned H₁ v tr₁ =>
      rw [hr] at h₁
      simp only [MSim] at h₁ ⊢
      intro K tr φs K' hK hs
      exact (hent K tr).trans (h₁ (.loop e φ :: K) tr φs K' hK
        (by rw [stackOwn_cons_nil rfl]; exact hs))
  | panic κ tr₁ => trivial
  | stuck w => trivial
  | outOfFuel => trivial

end forms

/-- **`eval` is simulated losslessly** (helper): for a `pendingSafe` program,
every `pendingSafe` expression from every copy-closed store, at every fuel,
reaches what `Sim` says it reaches by a run along which no step loses an
owned identity. The proof is `eval_sim`'s, form by form, with each step's
ledger closed by the matching exact ledger of `TraceExact.lean`; typing
enters nowhere — `eval`'s monitors are what copy closure needs — and
`pendingSafe` is what keeps an unwind from discarding a held value. -/
theorem eval_msim (M : FloatOps) {P : Program} (hp : P.pendingSafe = true) (fuel : Nat) :
    MSimIH M P fuel := by
  induction fuel using Nat.strongRecOn with
  | ind n ih =>
  intro H φ e hcc he
  cases n with
  | zero => simp [eval, MSim]
  | succ fuel =>
    have IH := ih fuel (Nat.lt_succ_self _)
    cases e with
    | intLit w s n => intro K tr; simpa [eval] using MSteps.toValue .intLit
    | floatLit w l => intro K tr; simpa [eval] using MSteps.toValue .floatLit
    | boolLit b => intro K tr; simpa [eval] using MSteps.toValue .boolLit
    | unitLit => intro K tr; simpa [eval] using MSteps.toValue .unitLit
    | use p => exact msim_use hcc p
    | binop op e₁ e₂ => exact msim_binop hp IH hcc op e₁ e₂ he
    | unop op e => exact msim_unop IH hcc op e he
    | intCast w s e => exact msim_intCast IH hcc w s e he
    | fintrin k e => exact msim_fintrin IH hcc k e he
    | panic msg => simp [eval, MSim]
    | dbg e => exact msim_dbg IH hcc e he
    | mkStruct s args => exact msim_mkStruct hp IH hcc s args he
    | mkEnum e k args => exact msim_mkEnum hp IH hcc e k args he
    | «match» scrut arms => exact msim_match hp IH hcc scrut arms he
    | mkArray T args => exact msim_mkArray hp IH hcc T args he
    | repeatArray T e n => exact msim_repeat IH hcc T e n he
    | indexRead p idx πs => exact msim_indexRead hp IH hcc p idx πs he
    | indexWrite p idx πs e => exact msim_indexWrite hp IH hcc p idx πs e he
    | indexDrop p idx πs =>
        cases fuel with
        | zero => simp [eval, EvalRes.andThen, MSim]
        | succ f => exact msim_indexDrop hp (ih f (by omega)) hcc p idx πs he
    | drop p => exact msim_drop hcc p
    | letIn m e₁ e₂ => exact msim_letIn hp IH hcc m e₁ e₂ he
    | assign p e => exact msim_assign hp IH hcc p e he
    | seq e₁ e₂ => exact msim_seq hp IH hcc e₁ e₂ he
    | ite c e₁ e₂ => exact msim_ite hp IH hcc c e₁ e₂ he
    | call f args => exact msim_call hp IH hcc f args he
    | ret e => exact msim_ret IH hcc e he
    | loop e => exact msim_loop hp IH hcc e he
    | brk => exact msim_brk

/-! ## Over a whole run -/

/-- **A finished run of a `pendingSafe` program is lossless** (helper): where
`run` answers a value, §6's relation reaches that value's terminal
configuration from `Config.init` by a run along which no step loses an owned
identity. -/
theorem run_msteps (M : FloatOps) {P : Program} (hp : P.pendingSafe = true) (fuel : Nat)
    {H : Store} {v : Val} {tr : List Event} (hr : run M P fuel = .ok H v tr) :
    MSteps M P Config.init (.run H Frame.empty [] (.ret v) tr) := by
  have h := eval_msim M hp fuel [] Frame.empty (.call 0 []) (fun ℓ c hc => by simp at hc) rfl
  simp only [run] at hr
  rw [hr] at h
  simpa [Config.init] using h [] []

/-- A store whose every cell is retired owns nothing (helper). -/
theorem storeOwn_of_dead {D : Decls} {H : Store}
    (h : ∀ ℓ, ℓ < H.length → H[ℓ]? = some .dead) : storeOwn D H = [] := by
  unfold storeOwn
  rw [List.flatMap_eq_nil_iff]
  intro c hc
  obtain ⟨ℓ, hℓ, rfl⟩ := List.getElem_of_mem hc
  have := h ℓ hℓ
  rw [List.getElem?_eq_getElem hℓ] at this
  rw [Option.some.inj this]
  rfl

/-- **A finished run ends with an empty store and counts every identity at
most once** (helper): `eval_tidy` retires every cell by the end, and
`eval_conserves` from the empty store bounds what the result and the trace
own by the range of identities minted (`run_trace_once`'s argument). -/
theorem run_final_le (M : FloatOps) (P : Program) (fuel : Nat) {H : Store} {v : Val}
    {tr : List Event} (hr : run M P fuel = .ok H v tr) :
    storeOwn P.decls H = [] ∧ ∀ a, (v.own P.decls).count a + (freedIds P.decls tr).count a ≤ 1 := by
  have ht := eval_tidy M P fuel [] Frame.empty (.call 0 []) ⟨by simp, by simp⟩
  have hc := eval_conserves M (freed_measure P.decls) fuel [] Frame.empty (.call 0 [])
    (fun ℓ c hc => by simp at hc)
  simp only [run] at hr
  rw [hr] at ht hc
  have hs : storeOwn P.decls H = [] :=
    storeOwn_of_dead fun ℓ hℓ => ht.2 ℓ (Nat.zero_le _) hℓ (by simp)
  refine ⟨hs, fun a => ?_⟩
  have h1 := hc.2.2.2 a
  have h2 := range'_count_le_one 0 (H.length - 0) a
  rw [hs] at h1
  simp only [storeOwn, Fresh, List.length_nil, List.flatMap_nil, List.count_nil] at h1 h2
  simp only [freedIds]
  omega

/-- **Every owned value of a finished run ends exactly once** (§7 "No
use-after-drop / no leak of drops", over a whole program; RUE-2478). For a
checked program whose functions are all `pendingSafe` (RUE-2316), take any
configuration `C` §6's relation reaches from `Config.init` and any owned
identity `a` it holds (`Config.held`: in a cell, in focus, or pending on the
control stack). If the run from `C` finishes with a value — `✓v`, a value at
an empty stack — then `a` is ended exactly once in the final trace (a drop, a
discarded temporary's drop, or a consumption: `freedIds`) or is part of the
final value, and not both. So no owned value the run ever holds is lost, and
none is ended twice. A panic carries no claim: §6.12's trap runs no drop
(§5.7's `⊥_panic` edge), so what it abandons is abandoned by design, and
`step_no_double_free` already bounds its trace.

The proof is lossless simulation: `eval_msim` follows `eval_sim` form by
form and shows each step of the run moves an owned identity between the
store, the focus, the stack and the trace without losing it (`MSteps`); by
determinism every configuration the run reaches lies on that run
(`MSteps.of_steps`). `eval_complete` places the run's end at `run`'s answer,
where `eval_tidy` has retired every cell and `eval_conserves` bounds each
count by one. -/
theorem whole_program_exactly_once (M : FloatModel) {P : Program} (h : ProgramTyped P)
    (hp : P.pendingSafe = true) {C : Config} (hC : Steps M.toFloatOps P Config.init C) {a : Nat}
    (ha : a ∈ C.held P.decls) {H : Store} {φ : Frame} {v : Val} {tr : List Event}
    (hT : Steps M.toFloatOps P C (.run H φ [] (.ret v) tr)) :
    (v.own P.decls).count a + (freedIds P.decls tr).count a = 1 := by
  obtain ⟨n, hn⟩ := (eval_complete M h).1 H φ v tr (hC.trans hT)
  have hr := hn (n + 1) (Nat.lt_succ_self n)
  have hm := (run_msteps M.toFloatOps hp (n + 1) hr).of_steps
    (fun _ => Step.terminal trivial) hC
  have hle := hm.le a
  obtain ⟨hs, hub⟩ := run_final_le M.toFloatOps P (n + 1) hr
  have hpos : 0 < (C.held P.decls).count a := List.count_pos_iff.mpr ha
  have hub := hub a
  rw [Config.ledger_count_run] at hle
  simp only [Config.ledger, List.count_append] at hle
  simp only [hs, Focus.own, stackOwn, List.flatMap_nil, List.count_nil] at hle
  omega

end RueCore
