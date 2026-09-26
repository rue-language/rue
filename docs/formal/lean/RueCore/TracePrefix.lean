module

public import RueCore.TraceOrder

@[expose] public section

/-!
# RueCore.TracePrefix — no double free on every prefix of a run (§7)

`no_double_free` (`Trace.lean`) bounds the trace of a run that `eval`
finishes within its fuel. A program that never finishes — a `loop` that
drops a value on every turn — has no such run: at every fuel `run` answers
`outOfFuel`, which carries no trace, so the theorem says nothing about it.
Yet "no destructor runs twice on one value" is a **safety property**
(Alpern & Schneider; `FIELD.md`): a violation of it shows in a finite prefix
of the run, so it should hold of every prefix (R1 of `REDTEAM-LOG.md`,
RUE-2477).

`step_no_double_free` states it that way, over §6's relation: for a checked
program, every configuration reachable from `Config.init` — the run so far,
finished or not — has a trace that frees no identity twice and runs no
destructor twice on one (`freedIds`, `dtorIds`, the vocabulary of
`no_double_free`).

## The invariant: exhausted fuel keeps a ledger

`eval_conserves` is a conservation law over *finished* evaluations: the
result's store, value and trace own at most what the start owned plus what
was minted. A trap is the one outcome that keeps a trace without a store, and
the law says of it only that the trace owns at most the start plus some
range of fresh identities (`Cons`'s `panic` clause).

`eval_steps_of_outOfFuel` (`Adequacy.lean`) turns exhausted fuel into a run of
§6's relation at least that long. This module strengthens it by that same
ledger (`LongC`): if `eval` exhausts `fuel` from a copy-closed store, §6's
relation has a run of at least `fuel` steps from the expression in focus,
and the trace that run appends satisfies the law as a trap's trace would —
it owns at most the start plus a range of fresh identities. The proof follows
`eval_steps_of_outOfFuel` form by form; where that proof walks through an
operand that finished (`eval_sim`'s `ok` clause), this one adds the operand's
ledger from `eval_conserves`, and where a step of its own emits events — a
temporary's drop (D-Seq), a `match`'s consumption and payload cells, a call's
parameter cells — it adds the same ledger `eval_conserves` uses for that step.
Typing is not read: like `eval_conserves`, the law holds of every program,
at every fuel `eval` does not refuse.

From `Config.init` the start owns nothing, so such a trace names each
identity at most once. And a trace only grows along §6's relation
(`reachable_drop_order`), so a count bound on a later configuration's trace
bounds every earlier one. So for a reachable `C`, reached in `k` steps,
`run` at fuel `k` decides: exhausted fuel gives a run of at least `k` steps
whose end bounds `C` (`Step.det`); a value or a panic is reached by §6's
relation (`run_sim`), past `C`, and `run_trace_once` bounds it; a refusal is
what typing excludes (`no_violation`), and nothing else is an answer of
`run`'s.
-/

namespace RueCore

section prefixLedger
variable {M : FloatOps} {P : Program} {F : Event → List Nat}

/-! ## Runs, counted from one start -/

/-- A counted run splits at every shorter length (helper). -/
theorem StepsN.split {m : Nat} {C D : Config} (h : StepsN M P m C D) :
    ∀ {k : Nat}, k ≤ m → ∃ E, StepsN M P k C E ∧ StepsN M P (m - k) E D := by
  induction h with
  | refl C => intro k hk; obtain rfl := Nat.le_zero.mp hk; exact ⟨C, .refl C, .refl C⟩
  | @step n C₁ C₂ C₃ s _ ih =>
      intro k hk
      cases k with
      | zero => exact ⟨C₁, .refl C₁, by simpa using StepsN.step s ‹_›⟩
      | succ k =>
          obtain ⟨E, hE, hr⟩ := ih (Nat.le_of_succ_le_succ hk)
          exact ⟨E, .step s hE, by simpa [Nat.succ_sub_succ] using hr⟩

/-- Of two runs from one start, the shorter one's end reaches the longer
one's (`Step.det`, §6) (helper). -/
theorem StepsN.reaches {k m : Nat} {C D E : Config} (hk : StepsN M P k C D)
    (hm : StepsN M P m C E) (hle : k ≤ m) : Steps M P D E := by
  obtain ⟨D', hD', hr⟩ := hm.split hle
  rw [StepsN.det hk hD']
  exact hr.toSteps

/-- **A trace only grows** along §6's relation from a reachable configuration
(helper). -/
theorem Steps.trace_ext {C D : Config} (hC : Steps M P Config.init C) (h : Steps M P C D) :
    ∃ evs, D.trace = C.trace ++ evs := by
  induction h with
  | refl => exact ⟨[], by simp⟩
  | @step C₁ C₂ C₃ s _ ih =>
      obtain ⟨e₁, h₁, -⟩ := reachable_drop_order hC s
      obtain ⟨e₂, h₂⟩ := ih (hC.trans (Steps.single s))
      exact ⟨e₁ ++ e₂, by rw [h₂, h₁, List.append_assoc]⟩

/-! ## The ledger a run keeps while `eval` exhausts its fuel -/

/-- A run of **at least** `n` steps from every member of a configuration family,
whose end has appended to the trace only what the conservation law allows a
trap from store `H` holding `X` (`Cons`'s `panic` clause): at most what `H` and
`X` own, plus a range of fresh identities (helper). -/
def LongC (M : FloatOps) (P : Program) (F : Event → List Nat) (H : Store) (X : List Nat)
    (C : List Kont → List Event → Config) (n : Nat) : Prop :=
  ∀ K tr, ∃ m D, n ≤ m ∧ StepsN M P m (C K tr) D ∧
    ∃ δ, D.trace = tr ++ δ ∧ Cons P.decls F H X (.panic .user δ)

/-- A family with long runs has shorter ones (helper). -/
theorem LongC.mono {H : Store} {X : List Nat} {C : List Kont → List Event → Config} {m n : Nat}
    (hmn : m ≤ n) (h : LongC M P F H X C n) : LongC M P F H X C m := by
  intro K tr
  obtain ⟨k, D, hk, hD, δ, hδ, hc⟩ := h K tr
  exact ⟨k, D, Nat.le_trans hmn hk, hD, δ, hδ, hc⟩

/-- Holding more does not break the ledger (helper). -/
theorem LongC.weaken {H : Store} {X Y : List Nat} {C : List Kont → List Event → Config} {n : Nat}
    (hXY : ∀ a, X.count a ≤ Y.count a) (h : LongC M P F H X C n) : LongC M P F H Y C n := by
  intro K tr
  obtain ⟨k, D, hk, hD, δ, hδ, hc⟩ := h K tr
  exact ⟨k, D, hk, hD, δ, hδ, Cons.weaken hc hXY⟩

/-- A run into a family with long runs, emitting `t` and moving the ledger from
`H`, `X` to `H₁`, `Y` as `Cons.prefix` allows, has long runs too (helper). -/
theorem LongC.pre {H H₁ : Store} {X Y : List Nat} {C C₂ : List Kont → List Event → Config}
    {n : Nat} {t : List Event} (hle : H.length ≤ H₁.length)
    (hI : ∀ a, (storeOwn P.decls H₁).count a + Y.count a + (t.flatMap F).count a
      ≤ (storeOwn P.decls H).count a + X.count a + (Fresh H H₁).count a)
    (hpre : ∀ K tr, Steps M P (C K tr) (C₂ K (tr ++ t))) (h : LongC M P F H₁ Y C₂ n) :
    LongC M P F H X C n := by
  intro K tr
  obtain ⟨j, hj⟩ := (hpre K tr).toN
  obtain ⟨k, D, hk, hD, δ, hδ, hc⟩ := h K (tr ++ t)
  refine ⟨j + k, D, by omega, hj.trans hD, t ++ δ, by rw [hδ, List.append_assoc], ?_⟩
  exact Cons.prefix (r := .panic .user δ) hle hI hc

/-- The same with one step first: the run is one step longer (helper). -/
theorem LongC.pre1 {H H₁ : Store} {X Y : List Nat} {C C₂ : List Kont → List Event → Config}
    {n : Nat} {t : List Event} (hle : H.length ≤ H₁.length)
    (hI : ∀ a, (storeOwn P.decls H₁).count a + Y.count a + (t.flatMap F).count a
      ≤ (storeOwn P.decls H).count a + X.count a + (Fresh H H₁).count a)
    (hpre : ∀ K tr, ∃ C', Step M P (C K tr) C' ∧ Steps M P C' (C₂ K (tr ++ t)))
    (h : LongC M P F H₁ Y C₂ n) : LongC M P F H X C (n + 1) := by
  intro K tr
  obtain ⟨C', s, hs⟩ := hpre K tr
  obtain ⟨j, hj⟩ := hs.toN
  obtain ⟨k, D, hk, hD, δ, hδ, hc⟩ := h K (tr ++ t)
  refine ⟨1 + j + k, D, by omega, ?_, t ++ δ, by rw [hδ, List.append_assoc], ?_⟩
  · have := StepsN.step s (hj.trans hD)
    rwa [show j + k + 1 = 1 + j + k by omega] at this
  · exact Cons.prefix (r := .panic .user δ) hle hI hc

/-- One step that emits nothing and keeps the store, into a family with long
runs holding at most as much (helper). -/
theorem LongC.step1 {H : Store} {X Y : List Nat} {C C₂ : List Kont → List Event → Config}
    {n : Nat} (hXY : ∀ a, Y.count a ≤ X.count a)
    (hpre : ∀ K tr, Step M P (C K tr) (C₂ K tr)) (h : LongC M P F H Y C₂ n) :
    LongC M P F H X C (n + 1) :=
  LongC.pre1 (t := []) (Nat.le_refl _) (fun a => by have := hXY a; simp; omega)
    (fun K tr => ⟨_, hpre K tr, by simpa using Steps.refl _⟩) h

/-- **§6.2's (Search), counted, with its ledger**: the twin of `Long.andThen`.
If `eval` spent its fuel on the operand, the operand's run is the long one;
if the operand finished, `Sim` gives the run to its value and `Cons` its
ledger, and the context's run is the long one (helper). -/
theorem LongC.andThen {H : Store} {X : List Nat} {φ₁ : Frame}
    {C C₁ : List Kont → List Event → Config} {Fr : Kont} {fuel : Nat}
    (hC : ∀ K tr, Step M P (C K tr) (C₁ (Fr :: K) tr))
    {r : EvalRes} (hsim : Sim M P φ₁ C₁ r) (hcons : Cons P.decls F H X r)
    (h₁ : r = .outOfFuel → LongC M P F H X C₁ fuel)
    {k : Store → Val → EvalRes}
    (hk : ∀ H₁ v tr₁, r = .ok H₁ v tr₁ → StoreCC P.decls H₁ →
      (Contents.ofVal v).copyClosed P.decls = true → k H₁ v = .outOfFuel →
      LongC M P F H₁ (v.own P.decls) (fun K tr => .run H₁ φ₁ (Fr :: K) (.ret v) tr) fuel) :
    r.andThen k = .outOfFuel → LongC M P F H X C (fuel + 1) := by
  intro hr
  cases r with
  | ok H₁ v tr₁ =>
      simp only [EvalRes.andThen, EvalRes.withTrace_outOfFuel_iff] at hr
      obtain ⟨l, c, cv, i⟩ := hcons
      exact LongC.pre1 (t := tr₁) l (fun a => by have := i a; omega)
        (fun K tr => ⟨_, hC K tr, hsim (Fr :: K) tr⟩) (hk H₁ v tr₁ rfl c cv hr)
  | outOfFuel =>
      have h := h₁ rfl
      exact LongC.step1 (C₂ := fun K tr => C₁ (Fr :: K) tr) (fun a => Nat.le_refl _) hC
        (fun K tr => h (Fr :: K) tr)
  | _ => simp [EvalRes.andThen] at hr

/-- The induction hypothesis: at fuel `fuel`, exhaustion from a copy-closed
store is a run of at least `fuel` steps with the ledger (helper). -/
def LongCIH (M : FloatOps) (P : Program) (F : Event → List Nat) (fuel : Nat) : Prop :=
  ∀ H φ e, StoreCC P.decls H → eval M fuel P H φ e = .outOfFuel →
    LongC M P F H [] (evalConf H φ e) fuel

/-- The owned identities of a list with one more value (helper). -/
theorem Contents.ownList_ofVals_snoc (D : Decls) (vs : List Val) (v : Val) :
    Contents.ownList D (Contents.ofVals (vs ++ [v]))
      = Contents.ownList D (Contents.ofVals vs) ++ v.own D := by
  induction vs with
  | nil => simp [Contents.ofVals, Contents.ownList, Val.own]
  | cons w ws ih =>
      simp only [List.cons_append, Contents.ownList_ofVals_cons, ih, List.append_assoc]

/-- **Argument lists, counted, with their ledger** (§6.2's `…( v̄, E, ē )`): the
values already built are held (`X`) while the next element runs (helper). -/
theorem evalArgs_longc {fuel : Nat} {φ : Frame} (hF : TraceMeasure P.decls F)
    (IH : LongCIH M P F fuel) (t : ArgsTag) :
    ∀ (es : List Expr) (H : Store) (vs₀ : List Val), StoreCC P.decls H →
    evalArgs (fun H e => eval M fuel P H φ e) H es = .abort .outOfFuel →
      LongC M P F H (Contents.ownList P.decls (Contents.ofVals vs₀)) (argsConf H φ t vs₀ es)
        (fuel + 1)
  | [], H, vs₀, _, h => by simp [evalArgs] at h
  | e :: es, H, vs₀, hcc, h => by
      have hpush : ∀ K tr, Step M P (argsConf H φ t vs₀ (e :: es) K tr)
          (evalConf H φ e (.args t vs₀ es :: K) tr) := fun K tr => .argsPush
      have hcons := eval_conserves M hF fuel H φ e hcc
      cases he : eval M fuel P H φ e with
      | ok H₁ v tr₁ =>
          rw [he] at hcons
          obtain ⟨l, c, _, i⟩ := hcons
          simp only [evalArgs, he] at h
          split at h
          · simp at h
          · rename_i r' h₂
            simp only [ArgsRes.abort.injEq, EvalRes.withTrace_outOfFuel_iff] at h
            subst h
            have ih := evalArgs_longc hF IH t es H₁ (vs₀ ++ [v]) c h₂
            have hs := eval_sim M P fuel H φ e
            rw [he] at hs
            refine LongC.mono (Nat.le_refl _) (LongC.pre (t := tr₁) l (fun a => ?_)
              (fun K tr => .step (hpush K tr) ((hs _ tr).trans (Steps.single .argsPlug))) ih)
            have := i a
            rw [Contents.ownList_ofVals_snoc, List.count_append]
            simp only [List.count_nil] at this
            omega
      | outOfFuel =>
          exact LongC.step1 (C₂ := fun K tr => evalConf H φ e (.args t vs₀ es :: K) tr)
            (Y := []) (fun a => by simp) hpush (fun K tr => IH H φ e hcc he _ tr)
      | _ => simp [evalArgs, he] at h

end prefixLedger

/-! ## The ledger, per form -/

section longcForms
variable {M : FloatOps} {P : Program} {F : Event → List Nat} {fuel : Nat} {H : Store} {φ : Frame}

/-- Close a context `k H v = .outOfFuel` whose context never spends fuel,
in `LongC.andThen`'s form (helper). -/
macro "never_oofc" : tactic => `(tactic| (
  intro _ _ _ _ _ _ h
  first
  | exact absurd h (OpRes.toRes_ne_outOfFuel _ _)
  | exact absurd h introVal_ne_outOfFuel
  | (try simp only [] at h
     (repeat' split at h) <;> first
       | simp at h
       | exact absurd h (OpRes.toRes_ne_outOfFuel _ _)
       | exact absurd h introVal_ne_outOfFuel)))

/-- A form with one operand and a context that never spends fuel (helper). -/
theorem longc_one (hF : TraceMeasure P.decls F) (IH : LongCIH M P F fuel)
    (hcc : StoreCC P.decls H) {e e' : Expr} {Fr : Kont} {k : Store → Val → EvalRes}
    (hent : ∀ K tr, Step M P (evalConf H φ e' K tr) (evalConf H φ e (Fr :: K) tr))
    (hk : ∀ H₁ v, k H₁ v ≠ .outOfFuel) :
    (eval M fuel P H φ e).andThen k = .outOfFuel →
      LongC M P F H [] (evalConf H φ e') (fuel + 1) :=
  LongC.andThen (C₁ := evalConf H φ e) (Fr := Fr) hent (eval_sim M P fuel H φ e)
    (eval_conserves M hF fuel H φ e hcc) (IH H φ e hcc)
    (fun _ _ _ _ _ _ h => absurd h (hk _ _))

/-- §6.4's binary operators, with the ledger (helper). -/
theorem longc_binop (hF : TraceMeasure P.decls F) (IH : LongCIH M P F fuel)
    (hcc : StoreCC P.decls H) (op : BinOp) (e₁ e₂ : Expr) :
    eval M (fuel + 1) P H φ (.binop op e₁ e₂) = .outOfFuel →
      LongC M P F H [] (evalConf H φ (.binop op e₁ e₂)) (fuel + 1) := by
  simp only [eval]
  refine LongC.andThen (C₁ := evalConf H φ e₁) (Fr := .binopL op e₂) (fun _ _ => .binopEnter)
    (eval_sim M P fuel H φ e₁) (eval_conserves M hF fuel H φ e₁ hcc) (IH H φ e₁ hcc) ?_
  intro H₁ v₁ _ _ hc₁ _ hk
  refine LongC.mono (Nat.le_succ _) (LongC.andThen (C₁ := evalConf H₁ φ e₂) (Fr := .binopR op v₁)
    (fun _ _ => .binopMid) (eval_sim M P fuel H₁ φ e₂)
    ((eval_conserves M hF fuel H₁ φ e₂ hc₁).weaken (by simp))
    (fun h => (IH H₁ φ e₂ hc₁ h).weaken (by simp)) ?_ hk)
  never_oofc

/-- (D-Let) §6.7, with the ledger: the binding's cell holds what the value
owned (helper). -/
theorem longc_letIn (hF : TraceMeasure P.decls F) (IH : LongCIH M P F fuel)
    (hcc : StoreCC P.decls H) (m : Bool) (e₁ e₂ : Expr) :
    eval M (fuel + 1) P H φ (.letIn m e₁ e₂) = .outOfFuel →
      LongC M P F H [] (evalConf H φ (.letIn m e₁ e₂)) (fuel + 1) := by
  simp only [eval]
  refine LongC.andThen (C₁ := evalConf H φ e₁) (Fr := .letIn e₂) (fun _ _ => .letEnter)
    (eval_sim M P fuel H φ e₁) (eval_conserves M hF fuel H φ e₁ hcc) (IH H φ e₁ hcc) ?_
  intro H₁ v₁ _ _ hc₁ hv₁ hk
  have hc₂ : StoreCC P.decls (H₁ ++ [.full (Contents.ofVal v₁)]) :=
    hc₁.append (StoreCC.single hv₁)
  have hL := LongC.andThen (M := M) (P := P) (F := F) (X := [])
    (C := fun K tr => .run H₁ φ (.letIn e₂ :: K) (.ret v₁) tr)
    (C₁ := evalConf (H₁ ++ [.full (Contents.ofVal v₁)])
      { env := H₁.length :: φ.env, scope := φ.scope ++ [H₁.length] } e₂)
    (Fr := .endscope [H₁.length]) (fun _ _ => .letBind) (eval_sim M P fuel _ _ e₂)
    (eval_conserves M hF fuel _ _ e₂ hc₂) (IH _ _ e₂ hc₂) (by never_oofc) hk
  refine LongC.mono (Nat.le_succ _) (LongC.pre (t := []) (H₁ := H₁ ++ [.full (Contents.ofVal v₁)])
    (Y := []) (by simp) (fun a => ?_) (fun K tr => by simpa using Steps.refl _) hL)
  rw [storeOwn_append]
  simp [storeOwn, Cell.own, List.count_append]

/-- (D-Match) §6.6, with the ledger: the consumed shell and the payload cells
own what the scrutinee owned (helper). -/
theorem longc_match (hF : TraceMeasure P.decls F) (IH : LongCIH M P F fuel)
    (hcc : StoreCC P.decls H) (scrut : Expr) (arms : List Expr) :
    eval M (fuel + 1) P H φ (.«match» scrut arms) = .outOfFuel →
      LongC M P F H [] (evalConf H φ (.«match» scrut arms)) (fuel + 1) := by
  simp only [eval]
  refine LongC.andThen (C₁ := evalConf H φ scrut) (Fr := .«match» arms) (fun _ _ => .matchEnter)
    (eval_sim M P fuel H φ scrut) (eval_conserves M hF fuel H φ scrut hcc) (IH H φ scrut hcc) ?_
  intro H₀ v _ _ hc₀ hv hk
  try simp only [] at hk
  split at hk
  · rename_i e k i vs _
    split at hk
    · simp at hk
    · rename_i body hbody
      rw [EvalRes.withTrace_outOfFuel_iff] at hk
      have hbo : eval M fuel P (mintParams H₀ vs).1
          { env := (mintParams H₀ vs).2.reverse ++ φ.env, scope := φ.scope ++ (mintParams H₀ vs).2 }
          body = .outOfFuel := by
        revert hk
        cases eval M fuel P (mintParams H₀ vs).1
            { env := (mintParams H₀ vs).2.reverse ++ φ.env, scope := φ.scope ++ (mintParams H₀ vs).2 }
            body with
        | ok H₂ v₂ tr₂ => simp only [EvalRes.andThen]; split <;> simp [EvalRes.withTrace]
        | _ => simp [EvalRes.andThen]
      have hpay := Contents.enum_payload hv
      have hcm := hc₀.mintParams (hpay 0).2
      refine LongC.mono (Nat.le_succ _) (LongC.pre1 (t := matchConsume P.decls e k i vs)
        (H₁ := (mintParams H₀ vs).1) (Y := [])
        (C₂ := fun K tr => evalConf (mintParams H₀ vs).1
          { env := (mintParams H₀ vs).2.reverse ++ φ.env, scope := φ.scope ++ (mintParams H₀ vs).2 }
          body (.endscope (mintParams H₀ vs).2 :: K) tr) ?_ ?_
        (fun K tr => ⟨_, .«match» hbody rfl, .refl _⟩)
        (fun K tr => IH _ _ body hcm hbo _ tr))
      · rw [mintParams_length]; omega
      · intro a
        have := matchConsume_measure hF hv a
        rw [storeOwn_mintParams]
        simp only [List.count_append, List.count_nil]
        show _ ≤ _ + ((Contents.enum e k i (Contents.ofVals vs)).own P.decls).count a + _
        omega
  · simp at hk

/-- (D-Seq) §6.7, with the ledger: the temporary's drop ends what it owned
(helper). -/
theorem longc_seq (hF : TraceMeasure P.decls F) (IH : LongCIH M P F fuel)
    (hcc : StoreCC P.decls H) (e₁ e₂ : Expr) :
    eval M (fuel + 1) P H φ (.seq e₁ e₂) = .outOfFuel →
      LongC M P F H [] (evalConf H φ (.seq e₁ e₂)) (fuel + 1) := by
  simp only [eval]
  refine LongC.andThen (C₁ := evalConf H φ e₁) (Fr := .seq e₂) (fun _ _ => .seqEnter)
    (eval_sim M P fuel H φ e₁) (eval_conserves M hF fuel H φ e₁ hcc) (IH H φ e₁ hcc) ?_
  intro H₁ v₁ _ _ hc₁ hv₁ hk
  try simp only [] at hk
  split at hk
  · simp at hk
  · rename_i hm
    split at hk
    · simp at hk
    · rename_i evs hd
      rw [EvalRes.withTrace_outOfFuel_iff] at hk
      have hne : v₁.mult P.decls ≠ .copy := by rw [hm]; exact nofun
      refine LongC.mono (Nat.le_succ _) (LongC.pre1 (t := .dropTemp v₁ :: evs) (Nat.le_refl _)
        (Y := []) (fun a => ?_) (C₂ := evalConf H₁ φ e₂)
        (fun K tr => ⟨_, .seqDrop hne hd, .refl _⟩) (IH H₁ φ e₂ hc₁ hk))
      have := hF.temp hv₁ hd a
      simp only [List.flatMap_cons, List.count_append, List.count_nil, Fresh.self] at *
      omega
  · rename_i hm
    exact LongC.mono (Nat.le_succ _) (LongC.step1 (C₂ := evalConf H₁ φ e₂) (Y := [])
      (fun a => by simp) (fun K tr => .seqCopy hm) (IH H₁ φ e₂ hc₁ hk))

/-- (D-If-T)/(D-If-F) §6.6, with the ledger (helper). -/
theorem longc_ite (hF : TraceMeasure P.decls F) (IH : LongCIH M P F fuel)
    (hcc : StoreCC P.decls H) (c e₁ e₂ : Expr) :
    eval M (fuel + 1) P H φ (.ite c e₁ e₂) = .outOfFuel →
      LongC M P F H [] (evalConf H φ (.ite c e₁ e₂)) (fuel + 1) := by
  simp only [eval]
  refine LongC.andThen (C₁ := evalConf H φ c) (Fr := .ite e₁ e₂) (fun _ _ => .iteEnter)
    (eval_sim M P fuel H φ c) (eval_conserves M hF fuel H φ c hcc) (IH H φ c hcc) ?_
  intro H₀ v _ _ hc₀ _ hk
  try simp only [] at hk
  split at hk
  · rename_i b
    split at hk
    · rename_i hb
      subst hb
      exact LongC.mono (Nat.le_succ _) (LongC.step1 (C₂ := evalConf H₀ φ e₁) (Y := [])
        (fun a => by simp) (fun K tr => .iteTrue) (IH H₀ φ e₁ hc₀ hk))
    · rename_i hb
      simp only [Bool.not_eq_true] at hb
      subst hb
      exact LongC.mono (Nat.le_succ _) (LongC.step1 (C₂ := evalConf H₀ φ e₂) (Y := [])
        (fun a => by simp) (fun K tr => .iteFalse) (IH H₀ φ e₂ hc₀ hk))
  · simp at hk

/-- An argument-list form whose list spent the fuel, from its enter step
(helper). -/
theorem longc_argsForm (hF : TraceMeasure P.decls F) {t : ArgsTag} {es : List Expr} {e : Expr}
    (IH : LongCIH M P F fuel) (hcc : StoreCC P.decls H)
    (hent : ∀ K tr, Step M P (evalConf H φ e K tr) (argsConf H φ t [] es K tr))
    (h : evalArgs (fun H e => eval M fuel P H φ e) H es = .abort .outOfFuel) :
    LongC M P F H [] (evalConf H φ e) (fuel + 1) :=
  LongC.mono (Nat.le_succ _) (LongC.step1 (Y := []) (fun a => by simp) hent
    (by simpa [Contents.ofVals, Contents.ownList] using evalArgs_longc hF IH t es H [] hcc h))

/-- (D-Struct) §6.5, with the ledger (helper). -/
theorem longc_mkStruct (hF : TraceMeasure P.decls F) (IH : LongCIH M P F fuel)
    (hcc : StoreCC P.decls H) (s : Nat) (args : List Expr) :
    eval M (fuel + 1) P H φ (.mkStruct s args) = .outOfFuel →
      LongC M P F H [] (evalConf H φ (.mkStruct s args)) (fuel + 1) := by
  simp only [eval]
  intro h
  split at h
  · subst h; exact longc_argsForm hF IH hcc (fun _ _ => .structEnter) ‹_›
  · rw [EvalRes.withTrace_outOfFuel_iff] at h
    (repeat' split at h) <;> first | simp at h | exact absurd h introVal_ne_outOfFuel

/-- (D-Enum-Intro) §6.6, with the ledger (helper). -/
theorem longc_mkEnum (hF : TraceMeasure P.decls F) (IH : LongCIH M P F fuel)
    (hcc : StoreCC P.decls H) (e k : Nat) (args : List Expr) :
    eval M (fuel + 1) P H φ (.mkEnum e k args) = .outOfFuel →
      LongC M P F H [] (evalConf H φ (.mkEnum e k args)) (fuel + 1) := by
  simp only [eval]
  intro h
  split at h
  · subst h; exact longc_argsForm hF IH hcc (fun _ _ => .enumEnter) ‹_›
  · rw [EvalRes.withTrace_outOfFuel_iff] at h
    (repeat' split at h) <;> first | simp at h | exact absurd h introVal_ne_outOfFuel

/-- (D-Array) §6.5, with the ledger (helper). -/
theorem longc_mkArray (hF : TraceMeasure P.decls F) (IH : LongCIH M P F fuel)
    (hcc : StoreCC P.decls H) (T : Ty) (args : List Expr) :
    eval M (fuel + 1) P H φ (.mkArray T args) = .outOfFuel →
      LongC M P F H [] (evalConf H φ (.mkArray T args)) (fuel + 1) := by
  simp only [eval]
  intro h
  split at h
  · subst h; exact longc_argsForm hF IH hcc (fun _ _ => .arrayEnter) ‹_›
  · rw [EvalRes.withTrace_outOfFuel_iff] at h
    exact absurd h introVal_ne_outOfFuel

/-- (D-Index) §6.5, with the ledger (helper). -/
theorem longc_indexRead (hF : TraceMeasure P.decls F) (IH : LongCIH M P F fuel)
    (hcc : StoreCC P.decls H) (p : Place) (idx : List Expr) (πs : List (List Nat)) :
    eval M (fuel + 1) P H φ (.indexRead p idx πs) = .outOfFuel →
      LongC M P F H [] (evalConf H φ (.indexRead p idx πs)) (fuel + 1) := by
  simp only [eval]
  intro h
  split at h
  · subst h; exact longc_argsForm hF IH hcc (fun _ _ => .indexReadEnter) ‹_›
  · rw [EvalRes.withTrace_outOfFuel_iff] at h
    (repeat' split at h) <;> simp at h

/-- `@drop` at a dynamic place, with the ledger: `eval` re-dispatches it to the
read at one less fuel (helper). -/
theorem longc_indexDrop (hF : TraceMeasure P.decls F) (IH : LongCIH M P F fuel)
    (hcc : StoreCC P.decls H) (p : Place) (idx : List Expr) (πs : List (List Nat)) :
    eval M (fuel + 2) P H φ (.indexDrop p idx πs) = .outOfFuel →
      LongC M P F H [] (evalConf H φ (.indexDrop p idx πs)) (fuel + 2) := by
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
    exact LongC.step1 (C₂ := argsConf H φ (.indexDrop p πs) [] idx) (Y := [])
      (fun a => by simp) (fun _ _ => .indexDropEnter)
      (by simpa [Contents.ofVals, Contents.ownList] using evalArgs_longc hF IH _ idx H [] hcc ‹_›)
  · rw [EvalRes.withTrace_outOfFuel_iff] at hr
    (repeat' split at hr) <;> simp at hr

/-- `@drop` at a dynamic place at the smallest fuel (helper). -/
theorem longc_indexDrop_one (p : Place) (idx : List Expr) (πs : List (List Nat)) :
    LongC M P F H [] (evalConf H φ (.indexDrop p idx πs)) 1 := by
  intro K tr
  exact ⟨1, _, Nat.le_refl _, .step .indexDropEnter (.refl _), [], by simp [Config.trace],
    ⟨0, fun a => by simp⟩⟩

/-- (D-Assign) below a dynamic index, with the ledger: the right-hand side's
value is held while the indices run (helper). -/
theorem longc_indexWrite (hF : TraceMeasure P.decls F) (IH : LongCIH M P F fuel)
    (hcc : StoreCC P.decls H) (p : Place) (idx : List Expr) (πs : List (List Nat)) (e : Expr) :
    eval M (fuel + 1) P H φ (.indexWrite p idx πs e) = .outOfFuel →
      LongC M P F H [] (evalConf H φ (.indexWrite p idx πs e)) (fuel + 1) := by
  simp only [eval]
  refine LongC.andThen (C₁ := evalConf H φ e) (Fr := .indexWriteRhs p idx πs)
    (fun _ _ => .indexWriteEnter) (eval_sim M P fuel H φ e) (eval_conserves M hF fuel H φ e hcc)
    (IH H φ e hcc) ?_
  intro H₁ v _ _ hc₁ _ hk
  try simp only [] at hk
  split at hk
  · subst hk
    have hA := evalArgs_longc hF IH (.indexWrite p πs v) idx H₁ [] hc₁ ‹_›
    simp only [Contents.ofVals, Contents.ownList] at hA
    exact LongC.mono (Nat.le_succ _) (LongC.pre (t := []) (Y := []) (Nat.le_refl _)
      (fun a => by simp) (fun K tr => by simpa using Steps.single .indexWriteRhs) hA)
  · rw [EvalRes.withTrace_outOfFuel_iff] at hk
    (repeat' split at hk) <;> simp at hk

/-- (D-Call) §6.9, with the ledger: the arguments' values move into the
parameter cells (helper). -/
theorem longc_call (hF : TraceMeasure P.decls F) (IH : LongCIH M P F fuel)
    (hcc : StoreCC P.decls H) (f : Nat) (args : List Expr) :
    eval M (fuel + 1) P H φ (.call f args) = .outOfFuel →
      LongC M P F H [] (evalConf H φ (.call f args)) (fuel + 1) := by
  simp only [eval]
  intro h
  split at h
  · subst h; exact longc_argsForm hF IH hcc (fun _ _ => .callEnter) ‹_›
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
        have ka := evalArgs_cons (fun H'' e' hc' => eval_conserves M hF fuel H'' φ e' hc') H args hcc
        rw [hr] at ka
        obtain ⟨l₁, c₁, cv₁, i₁⟩ := ka
        have hcm := c₁.mintParams cv₁
        refine LongC.pre1 (t := tr₁) (H₁ := (mintParams H₁ vs).1) (Y := [])
          (C₂ := fun K tr => evalConf _ _ fd.body (.call φ :: K) tr) ?_ ?_
          (fun K tr => ⟨_, .callEnter,
            (evalArgs_ok_steps hr K tr).trans (Steps.single (.call hfd hlen rfl))⟩)
          (fun K tr => IH _ _ _ hcm hb _ tr)
        · rw [mintParams_length]; omega
        · intro a
          have := i₁ a
          have := Fresh.count_trans l₁ (by rw [mintParams_length]; omega : H₁.length ≤ (mintParams H₁ vs).1.length) a
          rw [storeOwn_mintParams]
          simp only [List.count_append, List.count_nil] at *
          omega
      · simp at h

/-- (D-Loop-Enter) and (D-Loop-Iter) §6.10, with the ledger: a turn that
finishes has its ledger from `eval_conserves`, and the loop's re-evaluation is
the long run, its (D-Loop-Enter) peeled off by determinism (helper). -/
theorem longc_loop (hF : TraceMeasure P.decls F) (IH : LongCIH M P F fuel)
    (hcc : StoreCC P.decls H) (e : Expr) :
    eval M (fuel + 1) P H φ (.loop e) = .outOfFuel →
      LongC M P F H [] (evalConf H φ (.loop e)) (fuel + 1) := by
  simp only [eval]
  have hent : ∀ K tr, Step M P (evalConf H φ (.loop e) K tr) (evalConf H φ e (.loop e φ :: K) tr) :=
    fun _ _ => .loopEnter
  have hcons := eval_conserves M hF fuel H φ e hcc
  cases hr : eval M fuel P H φ e with
  | ok H₁ v tr₁ =>
      intro h
      cases v with
      | unit => ?_
      | _ => simp at h
      simp only [EvalRes.withTrace_outOfFuel_iff] at h
      rw [hr] at hcons
      obtain ⟨l, c, _, i⟩ := hcons
      have hs := eval_sim M P fuel H φ e
      rw [hr] at hs
      cases fuel with
      | zero => simp [eval] at hr
      | succ f =>
          have hL := IH H₁ φ (.loop e) c h
          have hL' : LongC M P F H₁ [] (fun K tr => evalConf H₁ φ e (.loop e φ :: K) tr) f := by
            intro K tr
            obtain ⟨m, D, hm, hD, δ, hδ, hc⟩ := hL K tr
            obtain ⟨m', rfl⟩ : ∃ m', m = m' + 1 := ⟨m - 1, by omega⟩
            exact ⟨m', D, by omega, hD.peel .loopEnter, δ, hδ, hc⟩
          have hit : LongC M P F H₁ [] (fun K tr => .run H₁ φ (.loop e φ :: K) (.ret .unit) tr)
              (f + 1) :=
            LongC.pre1 (t := []) (Nat.le_refl _) (fun a => by simp)
              (fun K tr => ⟨_, .loopIter (by simp [plainUnwind]), .refl _⟩) hL'
          exact LongC.pre1 (t := tr₁) l (fun a => by have := i a; simp at *; omega)
            (fun K tr => ⟨_, hent K tr, hs _ tr⟩) hit
  | broke H₁ sc tr₁ =>
      intro h
      simp only [] at h
      split at h <;> simp at h
  | outOfFuel =>
      intro _
      exact LongC.step1 (C₂ := fun K tr => evalConf H φ e (.loop e φ :: K) tr) (Y := [])
        (fun a => Nat.le_refl _) hent (fun K tr => IH H φ e hcc hr _ tr)
  | returned => simp
  | panic => simp
  | stuck => simp

end longcForms

/-- **Exhausted fuel keeps the ledger** (RUE-2477): if `eval` exhausts `fuel`
on an expression from a copy-closed store, then from that expression in
focus, under any context and after any trace, §6's relation has a run of at
least `fuel` steps whose appended trace owns, under any projection the law
counts, at most what the store owned plus a range of fresh identities — the
promise the conservation law makes of a trap. No typing hypothesis. The proof
is `eval_steps_of_outOfFuel`'s, form by form, with `eval_conserves`'s ledger
added wherever an operand finished. -/
theorem eval_longc (M : FloatOps) {P : Program} {F : Event → List Nat}
    (hF : TraceMeasure P.decls F) (fuel : Nat) : LongCIH M P F fuel := by
  induction fuel using Nat.strongRecOn with
  | ind n ih =>
  intro H φ e hcc
  cases n with
  | zero => intro _ K tr; exact ⟨0, _, Nat.le_refl _, .refl _, [], by simp [Config.trace],
      ⟨0, fun a => by simp⟩⟩
  | succ fuel =>
    have IH := ih fuel (Nat.lt_succ_self _)
    cases e with
    | intLit | floatLit | boolLit | unitLit | use | drop | panic | brk =>
        intro h; exact absurd h (eval_leaf_ne_outOfFuel trivial)
    | binop op e₁ e₂ => exact longc_binop hF IH hcc op e₁ e₂
    | unop op e =>
        simp only [eval]
        exact longc_one hF IH hcc (fun _ _ => .unopEnter)
          (fun _ _ => OpRes.toRes_ne_outOfFuel _ _)
    | intCast w s e =>
        simp only [eval]
        exact longc_one hF IH hcc (fun _ _ => .intCastEnter)
          (fun _ _ => OpRes.toRes_ne_outOfFuel _ _)
    | fintrin k e =>
        simp only [eval]
        exact longc_one hF IH hcc (fun _ _ => .fintrinEnter)
          (fun _ _ => OpRes.toRes_ne_outOfFuel _ _)
    | dbg e =>
        simp only [eval]
        exact longc_one hF IH hcc (fun _ _ => .dbgEnter)
          (fun _ _ => by split <;> simp)
    | mkStruct s args => exact longc_mkStruct hF IH hcc s args
    | mkEnum e k args => exact longc_mkEnum hF IH hcc e k args
    | «match» scrut arms => exact longc_match hF IH hcc scrut arms
    | mkArray T args => exact longc_mkArray hF IH hcc T args
    | repeatArray T e n =>
        simp only [eval]
        exact longc_one hF IH hcc (fun _ _ => .repeatEnter)
          (fun _ _ => by split <;> first | exact introVal_ne_outOfFuel | simp)
    | indexRead p idx πs => exact longc_indexRead hF IH hcc p idx πs
    | indexWrite p idx πs e => exact longc_indexWrite hF IH hcc p idx πs e
    | indexDrop p idx πs =>
        cases fuel with
        | zero => intro _; exact longc_indexDrop_one p idx πs
        | succ f => exact longc_indexDrop hF (ih f (by omega)) hcc p idx πs
    | letIn m e₁ e₂ => exact longc_letIn hF IH hcc m e₁ e₂
    | assign p e =>
        simp only [eval]
        exact longc_one hF IH hcc (fun _ _ => .assignEnter)
          (fun _ _ => by (repeat' split) <;> simp)
    | seq e₁ e₂ => exact longc_seq hF IH hcc e₁ e₂
    | ite c e₁ e₂ => exact longc_ite hF IH hcc c e₁ e₂
    | call f args => exact longc_call hF IH hcc f args
    | ret e =>
        simp only [eval]
        exact longc_one hF IH hcc (fun _ _ => .retEnter)
          (fun _ _ => by split <;> simp)
    | loop e => exact longc_loop hF IH hcc e

/-! ## Every prefix of a run -/

/-- **Every prefix of a run counts each identity at most once**, under any
projection the conservation law counts, for every program no fuel makes
`run` refuse (helper). A configuration reached in `k` steps is before the end
of `run`'s answer at fuel `k`: before a value or a panic `run_sim` reaches,
whose trace `run_trace_once` bounds, or before the end of a run of at least
`k` steps whose ledger `eval_longc` keeps; and a trace only grows along the
way. -/
theorem steps_trace_once (M : FloatOps) {P : Program} {F : Event → List Nat}
    (hF : TraceMeasure P.decls F) (hns : ∀ fuel w, run M P fuel ≠ .stuck w)
    {C : Config} (hC : Steps M P Config.init C) : ∀ a, (C.trace.flatMap F).count a ≤ 1 := by
  intro a
  obtain ⟨k, hk⟩ := hC.toN
  -- A bound on the trace of a configuration `C` reaches: it bounds `C`'s.
  have back : ∀ D, Steps M P C D → (D.trace.flatMap F).count a ≤ 1 →
      (C.trace.flatMap F).count a ≤ 1 := by
    intro D hD hb
    obtain ⟨evs, he⟩ := Steps.trace_ext hC hD
    rw [he, List.flatMap_append, List.count_append] at hb
    omega
  -- A configuration with no successor that the run reaches is past `C`.
  have final : ∀ T, Steps M P Config.init T → (∀ C', ¬ Step M P T C') → Steps M P C T := by
    intro T hT hfin
    obtain ⟨j, hj⟩ := hT.toN
    exact hk.reaches hj (StepsN.bound hj hfin hk)
  cases hr : run M P k with
  | ok H v tr =>
      have hT := (run_sim M P k).1 H v tr hr
      refine back _ (final _ hT (fun C' => Step.terminal (by simp [Config.Terminal]))) ?_
      have := run_trace_once M hF k a
      rw [hr] at this
      simpa [Config.trace, EvalRes.trace] using this
  | panic κ tr =>
      have hT := (run_sim M P k).2 κ tr hr
      refine back _ (final _ hT (fun C' => Step.terminal (by simp [Config.Terminal]))) ?_
      have := run_trace_once M hF k a
      rw [hr] at this
      simpa [Config.trace, EvalRes.trace] using this
  | outOfFuel =>
      obtain ⟨m, D, hm, hD, δ, hδ, N, hN⟩ :=
        eval_longc M hF k [] Frame.empty (.call 0 []) (fun ℓ c h => by simp at h) hr [] []
      refine back D (hk.reaches hD hm) ?_
      have := hN a
      have := range'_count_le_one 0 N a
      rw [hδ]
      simp only [storeOwn, List.flatMap_nil, List.count_nil, List.length_nil, List.nil_append] at *
      omega
  | stuck w => exact absurd hr (hns k w)
  | returned H v tr => exact absurd hr (run_ne_returned M H v tr)
  | broke H sc tr => exact absurd hr (run_ne_broke M H sc tr)

/-- **No double free, on every prefix of a run** (§7 "No double-free", as a
safety property; RUE-2477). For a checked program, every configuration §6's
relation reaches from `Config.init` — the run so far, whether or not it ever
finishes — has a trace that frees no identity twice (`freedIds`) and runs no
destructor twice on one (`dtorIds`). A program that diverges is covered: its
trace is bounded at every step, where `no_double_free`, over `run`'s answer,
sees only `outOfFuel` and an empty trace. -/
theorem step_no_double_free (M : FloatModel) {P : Program} (h : ProgramTyped P) {C : Config}
    (hC : Steps M.toFloatOps P Config.init C) :
    (∀ a, (freedIds P.decls C.trace).count a ≤ 1) ∧ (∀ a, (dtorIds C.trace).count a ≤ 1) :=
  ⟨steps_trace_once M.toFloatOps (freed_measure P.decls) (no_violation M h) hC,
    steps_trace_once M.toFloatOps (dtor_measure h.wf.decls.dtorNotCopy) (no_violation M h) hC⟩

/-- `no_double_free` for a finished run is a corollary (RUE-2477): a value or a
panic `run` answers is reached by §6's relation (`eval_sound`), so its trace is
a reachable configuration's; exhausted fuel carries the empty trace; and a
checked run is never refused (helper). -/
theorem no_double_free_of_step (M : FloatModel) {P : Program} (h : ProgramTyped P) (fuel : Nat) :
    (∀ w, run M.toFloatOps P fuel ≠ .stuck w) ∧
      (∀ a, (freedIds P.decls (run M.toFloatOps P fuel).trace).count a ≤ 1) ∧
      (∀ a, (dtorIds (run M.toFloatOps P fuel).trace).count a ≤ 1) := by
  have hs := eval_sound M h fuel
  have key : ∃ C, Steps M.toFloatOps P Config.init C ∧ C.trace = (run M.toFloatOps P fuel).trace := by
    cases hr : run M.toFloatOps P fuel with
    | ok H v tr => exact ⟨_, hs.2.1 H v tr hr, by simp [Config.trace, EvalRes.trace]⟩
    | panic κ tr => exact ⟨_, hs.2.2 κ tr hr, by simp [Config.trace, EvalRes.trace]⟩
    | outOfFuel => exact ⟨_, .refl _, by simp [Config.init, Config.trace, EvalRes.trace]⟩
    | stuck w => exact absurd hr (hs.1 w)
    | returned H v tr => exact absurd hr (run_ne_returned _ H v tr)
    | broke H sc tr => exact absurd hr (run_ne_broke _ H sc tr)
  obtain ⟨C, hC, htr⟩ := key
  rw [← htr]
  exact ⟨hs.1, (step_no_double_free M h hC).1, (step_no_double_free M h hC).2⟩

end RueCore
