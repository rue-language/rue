module

public import RueCore.Step
public import RueCore.Dynamics.Lemmas
public import RueCore.Equivalence.Defs

@[expose] public section

/-!
# RueCore.Step.Lemmas — the lemmas about `Step.lean`'s definitions (layer L2)

Every theorem `Step.lean` held but its demo witnesses, moved here verbatim and
in source order so that the definitions layer holds definitions only
(RUE-2460; README, "Layers"): the relation's determinism, `step_iff`, the
trichotomy and the stuck-state lemmas, and the lemmas that a monitor only
removes behaviour. The section headings are `Step.lean`'s own, repeated where
a moved theorem sits under one. The last section is the simulation between
`eval` and `Step`, parametrized, which `Equivalence`, `TracePrefix` and
`TraceWhole` share (RUE-2517). The demo witnesses — programs run through the
relation — are in `Witnesses.lean`.
-/

namespace RueCore

/-! ## Scope pops -/

/-- (D-EndScope)'s pop restores the activation record (D-Let) or (D-Match)
extended (helper). -/
theorem Activation.unwindScope_push (φ : Activation) (ls : List Nat) :
    ({ env := ls.reverse ++ φ.env, scope := φ.scope ++ ls } : Activation).unwindScope ls.length = φ := by
  cases φ
  simp [Activation.unwindScope]

/-- (D-EndScope) after (D-Let) (helper). -/
theorem Activation.unwindScope_let (φ : Activation) (ℓ : Nat) :
    ({ env := ℓ :: φ.env, scope := φ.scope ++ [ℓ] } : Activation).unwindScope 1 = φ := by
  cases φ
  simp [Activation.unwindScope]

/-! ## The sanity theorems -/

/-- Every `Step` is the one `step` computes (§6). -/
theorem Step.step_eq {M : FloatSig} {P : Program} {C C' : Config} (h : Step M P C C') :
    step M P C = .next C' := by
  cases h <;> simp_all [step, stepEval, stepArgs, stepRet, OpRes.toStep]

/-- **Determinism** of §6's reduction on the fragment: a configuration takes
at most one step. The rules' left-hand sides fix the focus and the top frame,
and the pairs that share one — (D-Use-Copy)/(D-Use-Move)/(D-Use-Declared-Linear),
(D-Seq)'s two cases, (D-If-T)/(D-If-F), an operator's value and trap rules,
(D-Index)/(D-Index-Trap) — are split by premises that are functions of the
configuration. -/
theorem Step.det {M : FloatSig} {P : Program} {C C₁ C₂ : Config}
    (h₁ : Step M P C C₁) (h₂ : Step M P C C₂) : C₁ = C₂ := by
  have e₁ := h₁.step_eq
  rw [h₂.step_eq] at e₁
  exact (StepOut.next.inj e₁).symm

/-- A terminal configuration takes no step: `✓n` and `↯κ` are final (§6.12). -/
theorem Step.terminal {M : FloatSig} {P : Program} {C C' : Config}
    (hC : C.Terminal) : ¬ Step M P C C' := by
  intro h
  cases h <;> simp [Config.Terminal] at hC

/-! ## `step` is `Step`, and the enumeration of what a configuration can be -/

/-- `stepEval`'s `next` is a `Step` (helper). -/
theorem stepEval_complete {M : FloatSig} {P : Program} {H : Store} {φ : Activation}
    {K : List Kont} {tr : List Event} {e : Expr} {C' : Config}
    (h : stepEval M P H φ K tr e = .next C') : Step M P (.run H φ K (.eval e) tr) C' := by
  cases e <;> simp only [stepEval] at h
  all_goals (repeat' split at h)
  all_goals (first | (simp at h) | skip)
  all_goals (try subst h)
  all_goals (try simp only [Bool.not_eq_true] at *)
  all_goals (constructor <;> first | assumption | rfl)

/-- `stepArgs`'s `next` is a `Step` (helper). -/
theorem stepArgs_complete {M : FloatSig} {P : Program} {H : Store} {φ : Activation}
    {K : List Kont} {tr : List Event} {vs : List Val} {t : ArgsFrame} {C' : Config}
    (h : stepArgs P H φ K tr vs t = .next C') : Step M P (.run H φ K (.args t vs []) tr) C' := by
  cases t <;> simp only [stepArgs] at h
  all_goals (repeat' split at h)
  all_goals (first | (simp at h) | skip)
  all_goals (try subst h)
  all_goals (constructor <;> first | assumption | rfl)

/-- `stepRet`'s `next` is a `Step` (helper). -/
theorem stepRet_complete {M : FloatSig} {P : Program} {H : Store} {φ : Activation}
    {K : List Kont} {tr : List Event} {v : Val} {k : Kont} {C' : Config}
    (h : stepRet M P H φ K tr v k = .next C') : Step M P (.run H φ (k :: K) (.ret v) tr) C' := by
  cases k <;> simp only [stepRet, OpRes.toStep] at h
  all_goals (repeat' split at h)
  all_goals (first | (simp at h) | skip)
  all_goals (try subst h)
  all_goals (constructor <;> first | assumption | rfl)

/-- **`step` is `Step`** (§6): the function computes exactly the relation's
one step. With `Step.step_eq` this is what makes `step`'s other two answers
an enumeration of the configurations that take no step. -/
theorem step_iff {M : FloatSig} {P : Program} {C C' : Config} :
    Step M P C C' ↔ step M P C = .next C' := by
  refine ⟨Step.step_eq, fun h => ?_⟩
  match C, h with
  | .panic _ _, h => simp [step] at h
  | .run H φ K (.eval e) tr, h => exact stepEval_complete h
  | .run H φ K (.args t vs (e :: es)) tr, h =>
      simp only [step, StepOut.next.injEq] at h; subst h; exact .argsPush
  | .run H φ K (.args t vs []) tr, h => exact stepArgs_complete h
  | .run _ _ [] (.ret _) _, h => simp [step] at h
  | .run H φ (k :: K) (.ret v) tr, h => exact stepRet_complete h

/-- `stepEval` never answers `halted` (helper). -/
theorem stepEval_ne_halted {M : FloatSig} {P : Program} {H : Store} {φ : Activation}
    {K : List Kont} {tr : List Event} {e : Expr} : stepEval M P H φ K tr e ≠ .halted := by
  intro h
  cases e <;> simp only [stepEval] at h
  all_goals (repeat' split at h)
  all_goals simp at h

/-- `stepArgs` never answers `halted` (helper). -/
theorem stepArgs_ne_halted {P : Program} {H : Store} {φ : Activation} {K : List Kont}
    {tr : List Event} {vs : List Val} {t : ArgsFrame} : stepArgs P H φ K tr vs t ≠ .halted := by
  intro h
  cases t <;> simp only [stepArgs] at h
  all_goals (repeat' split at h)
  all_goals simp at h

/-- `stepRet` never answers `halted` (helper). -/
theorem stepRet_ne_halted {M : FloatSig} {P : Program} {H : Store} {φ : Activation}
    {K : List Kont} {tr : List Event} {v : Val} {k : Kont} : stepRet M P H φ K tr v k ≠ .halted := by
  intro h
  cases k <;> simp only [stepRet, OpRes.toStep] at h
  all_goals (repeat' split at h)
  all_goals simp at h

/-- `step` answers `halted` exactly at the terminal configurations: `✓n` and
`↯κ` (§6.12's (Result-Ok) and (Result-Panic)). -/
theorem step_halted_iff {M : FloatSig} {P : Program} {C : Config} :
    step M P C = .halted ↔ C.Terminal := by
  match C with
  | .panic _ _ => simp [step, Config.Terminal]
  | .run H φ K (.eval e) tr => simp only [step, Config.Terminal, iff_false]; exact stepEval_ne_halted
  | .run H φ K (.args t vs (e :: es)) tr => simp [step, Config.Terminal]
  | .run H φ K (.args t vs []) tr =>
      simp only [step, Config.Terminal, iff_false]; exact stepArgs_ne_halted
  | .run _ _ [] (.ret _) _ => simp [step, Config.Terminal]
  | .run H φ (k :: K) (.ret v) tr =>
      simp only [step, Config.Terminal, iff_false]; exact stepRet_ne_halted

/-- **Every configuration is terminal, steps, or is stuck** (§6, §7's
phrasing of progress): the three cases are exclusive (`step` is a function)
and exhaustive, and a stuck one is named. -/
theorem Config.trichotomy (M : FloatSig) (P : Program) (C : Config) :
    (∃ C', Step M P C C') ∨ C.Terminal ∨ ∃ w, C.Stuck M P w := by
  cases h : step M P C with
  | next C' => exact .inl ⟨C', step_iff.mpr h⟩
  | halted => exact .inr (.inl (step_halted_iff.mp h))
  | stuck w => exact .inr (.inr ⟨w, h⟩)

/-- **Stuck, in `Step`'s own terms** (§6, §7): a configuration is stuck —
not terminal, and no rule of §6 applies to it — exactly when `step` names it
stuck. This is what makes `Config.Stuck` a statement about the relation and not
only about the function's labels. -/
theorem Config.stuck_iff {M : FloatSig} {P : Program} {C : Config} :
    (¬ C.Terminal ∧ ∀ C', ¬ Step M P C C') ↔ ∃ w, C.Stuck M P w := by
  cases h : step M P C with
  | next C' =>
      refine iff_of_false (fun ⟨_, hn⟩ => hn C' (step_iff.mpr h)) ?_
      intro ⟨w, hw⟩; simp [Config.Stuck, h] at hw
  | halted =>
      refine iff_of_false (fun ⟨hT, _⟩ => hT (step_halted_iff.mp h)) ?_
      intro ⟨w, hw⟩; simp [Config.Stuck, h] at hw
  | stuck w =>
      refine iff_of_true ⟨fun hT => ?_, fun C' hs => ?_⟩ ⟨w, h⟩
      · rw [step_halted_iff.mpr hT] at h; cases h
      · rw [step_iff.mp hs] at h; cases h

/-- A stuck configuration takes no step (§6). -/
theorem Config.Stuck.no_step {M : FloatSig} {P : Program} {C C' : Config} {w : Refusal}
    (h : C.Stuck M P w) : ¬ Step M P C C' := by
  intro hs
  have := hs.step_eq
  simp [Config.Stuck] at h
  rw [h] at this
  cases this

/-! ## Stuck states are §6's, and the monitors are absent (RUE-2314) -/

/-- `getAt` refuses only with §6's stuck states (helper). -/
theorem Contents.getAt_err : ∀ {c : Contents} {π : List Nat} {w : Refusal},
    c.getAt π = .error w → w.isStuckState = true
  | _, [], _, h => by simp [Contents.getAt] at h
  | .movedOut, _ :: _, _, h => by simp [Contents.getAt] at h; subst h; rfl
  | .struct _ _ cs, f :: π, _, h => by
      simp only [Contents.getAt] at h
      split at h
      · exact Contents.getAt_err h
      · simp at h; subst h; rfl
  | .array _ _ cs, f :: π, _, h => by
      simp only [Contents.getAt] at h
      split at h
      · exact Contents.getAt_err h
      · simp at h; subst h; rfl
  | .int _ _ _, _ :: _, _, h | .float _ _, _ :: _, _, h | .bool _, _ :: _, _, h
  | .unit, _ :: _, _, h | .enum _ _ _ _, _ :: _, _, h => by
      simp [Contents.getAt] at h; subst h; rfl

mutual
/-- `split` refuses only with §6's stuck states (helper). -/
theorem Contents.splitResidue_err (D : Decls) : ∀ {c : Contents} {π : List Nat} {w : Refusal},
    c.splitResidue D π = .error w → w.isStuckState = true
  | _, [], _, h => by simp [Contents.splitResidue] at h
  | .struct _ _ cs, f :: π, _, h => by
      simp only [Contents.splitResidue] at h; exact Contents.splitFields_err D h
  | .array _ _ cs, f :: π, _, h => by
      simp only [Contents.splitResidue] at h; exact Contents.splitFields_err D h
  | .movedOut, _ :: _, _, h => by simp [Contents.splitResidue] at h; subst h; rfl
  | .int _ _ _, _ :: _, _, h | .float _ _, _ :: _, _, h | .bool _, _ :: _, _, h
  | .unit, _ :: _, _, h | .enum _ _ _ _, _ :: _, _, h => by
      simp [Contents.splitResidue] at h; subst h; rfl

/-- `split`'s field step refuses only with §6's stuck states (helper). -/
theorem Contents.splitFields_err (D : Decls) :
    ∀ {cs : List Contents} {f : Nat} {π : List Nat} {w : Refusal},
    Contents.splitFields D cs f π = .error w → w.isStuckState = true
  | [], _, _, _, h => by simp [Contents.splitFields] at h; subst h; rfl
  | c :: _, 0, π, _, h => by
      simp only [Contents.splitFields] at h
      split at h
      · simp at h; subst h; exact Contents.splitResidue_err D ‹_›
      · simp at h
  | _ :: cs, f + 1, π, _, h => by
      simp only [Contents.splitFields] at h
      split at h
      · simp at h; subst h; exact Contents.splitFields_err D ‹_›
      · simp at h
end

mutual
/-- §6.11's `drop` refuses only with §6's stuck states (helper). -/
theorem dropContents_err (D : Decls) : ∀ {c : Contents} {w : Refusal},
    dropContents D c = .error w → w.isStuckState = true
  | .movedOut, _, h | .int _ _ _, _, h | .float _ _, _, h | .bool _, _, h | .unit, _, h => by
      simp [dropContents] at h
  | .struct s i cs, _, h => by
      simp only [dropContents] at h
      split at h
      · simp at h; subst h; rfl
      · split at h
        · simp at h; subst h; exact dropContentsList_err D ‹_›
        · simp at h
  | .enum _ _ _ cs, _, h => by simp only [dropContents] at h; exact dropContentsList_err D h
  | .array _ _ cs, _, h => by simp only [dropContents] at h; exact dropContentsList_err D h

/-- `drop*` refuses only with §6's stuck states (helper). -/
theorem dropContentsList_err (D : Decls) : ∀ {cs : List Contents} {w : Refusal},
    dropContentsList D cs = .error w → w.isStuckState = true
  | [], _, h => by simp [dropContentsList] at h
  | c :: cs, _, h => by
      simp only [dropContentsList] at h
      split at h
      · simp at h; subst h; exact dropContents_err D ‹_›
      · split at h
        · simp at h; subst h; exact dropContentsList_err D ‹_›
        · simp at h
end

/-- A binding's drop refuses only with §6's stuck states (helper). -/
theorem dropCell_err {D : Decls} {ℓ : Nat} {c : Contents} {w : Refusal}
    (h : dropCell D ℓ c = .error w) : w.isStuckState = true := by
  simp only [dropCell] at h
  split at h
  · simp at h
  · split at h
    · simp at h; subst h; exact dropContents_err D ‹_›
    · simp at h

/-- `plainUnwind` refuses only with §6's stuck states (helper). -/
theorem plainUnwind_err {D : Decls} : ∀ {H : Store} {ls : List Nat} {w : Refusal},
    plainUnwind D H ls = .error w → w.isStuckState = true
  | _, [], _, h => by simp [plainUnwind] at h
  | H, ℓ :: rest, _, h => by
      simp only [plainUnwind, plainDropRetire] at h
      split at h
      · rename_i heq
        split at heq
        · simp at heq; subst heq; simp at h; subst h; rfl
        · simp at heq; subst heq; simp at h; subst h; rfl
        · split at heq
          · simp at heq; subst heq; simp at h; subst h; exact dropCell_err ‹_›
          · simp at heq
      · split at h
        · simp at h; subst h; exact plainUnwind_err ‹_›
        · simp at h

/-- The residue's plain `drop*` refuses only with §6's stuck states (helper). -/
theorem plainResidue_err {D : Decls} {ℓ : Nat} : ∀ {rs : List Contents} {w : Refusal},
    plainResidue D ℓ rs = .error w → w.isStuckState = true
  | [], _, h => by simp [plainResidue] at h
  | r :: rs, _, h => by
      simp only [plainResidue] at h
      split at h
      · simp at h; subst h; exact dropContents_err D ‹_›
      · split at h
        · simp at h; subst h; exact plainResidue_err ‹_›
        · simp at h

/-- `plainDestructure` refuses only with §6's stuck states (helper). -/
theorem plainDestructure_err {D : Decls} {ℓ : Nat} {c : Contents} {πs : List Nat} {w : Refusal}
    (h : plainDestructure D ℓ c πs = .error w) : w.isStuckState = true := by
  simp only [plainDestructure] at h
  split at h
  · simp at h; subst h; exact Contents.splitResidue_err D ‹_›
  · split at h
    · simp at h; subst h; exact plainResidue_err ‹_›
    · simp at h

/-- `rootCell` refuses only with §6's stuck states (helper). -/
theorem rootCell_err {H : Store} {φ : Activation} {i : Nat} {w : Refusal}
    (h : rootCell H φ i = .error w) : w.isStuckState = true := by
  simp only [rootCell] at h
  repeat' split at h
  all_goals simp at h
  all_goals (subst h; rfl)

/-- Resolving a dynamic tail refuses only with §6's stuck states (helper). -/
theorem Contents.resolveDyn_err : ∀ {c : Contents} {is : List Int} {πs : List (List Nat)}
    {w : Refusal}, c.resolveDyn is πs = .refused w → w.isStuckState = true
  | c, [], [], _, h => by cases c <;> simp [Contents.resolveDyn] at h
  | c, i :: is, π :: πs, w, h => by
      cases c
      case array T cs =>
        simp only [Contents.resolveDyn] at h
        split at h
        · split at h
          · simp at h; subst h; rfl
          · split at h
            · simp at h; subst h; exact Contents.getAt_err ‹_›
            · split at h
              · simp at h
              · exact Contents.resolveDyn_err h
        · simp at h
      all_goals (simp [Contents.resolveDyn] at h; subst h; rfl)
  | c, [], _ :: _, _, h | c, _ :: _, [], _, h => by
      cases c <;> simp [Contents.resolveDyn] at h <;> (subst h; rfl)

/-- Navigating a dynamic place refuses only with §6's stuck states (helper). -/
theorem dynPlace_err {H : Store} {φ : Activation} {p : Place} {vs : List Val}
    {πs : List (List Nat)} {w : Refusal}
    (h : dynPlace H φ p vs πs = .refused w) : w.isStuckState = true := by
  simp only [dynPlace] at h
  repeat' split at h
  all_goals simp at h
  all_goals (try (subst h; rfl))
  · subst h; exact Contents.getAt_err ‹_›
  · subst h; exact Contents.resolveDyn_err ‹_›

/-- **§6's stuck states only** (RUE-2314): a configuration `step` finds stuck
is stuck on `useAfterMove`, `useAfterDrop`, `unbound` or `typeConfusion` —
never on `linearLeak`, `linearOverwrite`, `linearDiscard` or `ownedUnderCopy`,
the four monitors `eval` adds and §6.3, §6.5, §6.7 and §6.8 do not have. -/
theorem step_stuck_isStuckState {M : FloatSig} {P : Program} {C : Config} {w : Refusal}
    (h : C.Stuck M P w) : w.isStuckState = true := by
  simp only [Config.Stuck] at h
  match C, h with
  | .panic _ _, h => simp [step] at h
  | .run _ _ [] (.ret _) _, h => simp [step] at h
  | .run H φ K (.args t vs (e :: es)) tr, h => simp [step] at h
  | .run H φ K (.eval e) tr, h =>
      simp only [step] at h
      cases e <;> simp only [stepEval] at h
      all_goals (repeat' split at h)
      all_goals simp at h
      all_goals (try (subst h; rfl))
      all_goals subst h
      all_goals first
        | exact rootCell_err ‹_›
        | exact Contents.getAt_err ‹_›
        | exact plainDestructure_err ‹_›
        | exact dropCell_err ‹_›
        | exact plainUnwind_err ‹_›
  | .run H φ K (.args t vs []) tr, h =>
      simp only [step] at h
      cases t <;> simp only [stepArgs] at h
      all_goals (repeat' split at h)
      all_goals simp at h
      all_goals (try (subst h; rfl))
      all_goals subst h
      all_goals first
        | exact dynPlace_err ‹_›
        | exact Contents.getAt_err ‹_›
        | exact dropCell_err ‹_›
  | .run H φ (k :: K) (.ret v) tr, h =>
      simp only [step] at h
      cases k <;> simp only [stepRet, OpRes.toStep] at h
      all_goals (repeat' split at h)
      all_goals simp at h
      all_goals (try (subst h; rfl))
      all_goals subst h
      all_goals first
        | exact rootCell_err ‹_›
        | exact Contents.getAt_err ‹_›
        | exact dropCell_err ‹_›
        | exact dropContents_err _ ‹_›
        | exact plainUnwind_err ‹_›

/-! ## Where a monitor passes, the plain drop agrees -/

/-- Where `eval`'s leak monitor lets a scope exit through, §6's monitor-free
drop-retire does the same thing (§6.1's `drop-retire`) (helper). -/
theorem dropRetire_plain {D : Decls} {H : Store} {ℓ : Nat} {r : Store × List Event}
    (h : dropRetire D H ℓ = .ok r) : plainDropRetire D H ℓ = .ok r := by
  simp only [dropRetire] at h
  simp only [plainDropRetire]
  split at h
  · simp at h
  · simp at h
  · rename_i heq
    rw [heq]
    split at h
    · simp at h
    · exact h

/-- **The leak monitor only removes behaviour** (RUE-2314): where
`unwindLocs` — `run-scope-drops` with `eval`'s monitor — succeeds, §6's
monitor-free `plainUnwind` succeeds with the same store and trace. Parts 2
and 3 of the semantic-equivalence proof read every scope exit through this. -/
theorem unwindLocs_plain {D : Decls} : ∀ {H : Store} {ls : List Nat} {r : Store × List Event},
    unwindLocs D H ls = .ok r → plainUnwind D H ls = .ok r
  | _, [], _, h => by simpa [unwindLocs, plainUnwind] using h
  | H, ℓ :: rest, r, h => by
      simp only [unwindLocs] at h
      simp only [plainUnwind]
      split at h
      · simp at h
      · rename_i H₁ evs heq
        rw [dropRetire_plain heq]
        split at h
        · simp at h
        · rename_i heq'
          simp only [unwindLocs_plain heq']
          exact h

/-- The linear-leak monitor passes only where the plain `drop*` of the residue
succeeds with the same trace (helper). -/
theorem dropResidue_plain {D : Decls} {ℓ : Nat} : ∀ {rs : List Contents} {evs : List Event},
    dropResidue D ℓ rs = .ok evs → plainResidue D ℓ rs = .ok evs
  | [], _, h => by simpa [dropResidue, plainResidue] using h
  | r :: rs, evs, h => by
      simp only [dropResidue] at h
      simp only [plainResidue]
      split at h
      · simp at h
      · split at h
        · simp at h
        · rename_i heq
          rw [heq]
          split at h
          · simp at h
          · rename_i heq'
            simp only [dropResidue_plain heq']
            exact h

/-- **The linear-leak monitor only removes behaviour** (RUE-2314): where
`eval`'s monitored `destructure` succeeds, §6.3's monitor-free
`destructure` succeeds with the same leaf and trace. -/
theorem destructure_plain {D : Decls} {ℓ : Nat} {c : Contents} {πs : List Nat}
    {r : Contents × List Event} (h : c.destructure D ℓ πs = .ok r) :
    plainDestructure D ℓ c πs = .ok r := by
  simp only [Contents.destructure] at h
  simp only [plainDestructure]
  split at h
  · simp at h
  · rename_i leaf rs heq
    rw [heq]
    simp only
    split at h
    · simp at h
    · rename_i heq'
      rw [dropResidue_plain heq']
      exact h

/-! ## Running the relation -/

/-- Whatever `stepN` reaches, `→*` reaches (§6.12's `→*`), so a run of the
function is a derivation of the relation (helper). -/
theorem stepN_steps {M : FloatSig} {P : Program} : ∀ {n : Nat} {C : Config},
    Steps M P C (stepN M P n C)
  | 0, C => .refl C
  | n + 1, C => by
      simp only [stepN]
      split
      · rename_i C' h
        exact .step (step_iff.mpr h) stepN_steps
      · exact .refl C
      · exact .refl C

/-- **A property every step keeps holds along `→*`** (§6.12): the one induction
over `Steps` that the step-invariant theorems share (helper). -/
theorem Steps.invariant {M : FloatSig} {P : Program} {I : Config → Prop}
    (hstep : ∀ {C C'}, Step M P C C' → I C → I C') :
    ∀ {C C'}, Steps M P C C' → I C → I C' := by
  intro C C' hs
  induction hs with
  | refl => exact id
  | step h₁ _ ih => exact fun hC => ih (hstep h₁ hC)


/-! ## The simulation, parametrized (RUE-2517)

One relation between an `eval` result and §6's `→*`, `SimBy`, parametrized by
a per-step invariant `I` (its runs are `ISteps`), by what an unwind asks of the
frames it leaves (`G`), and by whether a panic is simulated (`pan`):
`Equivalence.lean`'s `Sim` has all three `True`. `LongBy` is the long run that
exhausted fuel pays for, with an invariant on its end (`Long`: none). -/

/-- `→*` along which every step keeps the per-step invariant `I` (helper). -/
inductive ISteps (M : FloatSig) (P : Program) (I : Config → Config → Prop) :
    Config → Config → Prop where
  | refl (C : Config) : ISteps M P I C C
  | step {C₁ C₂ C₃ : Config} : Step M P C₁ C₂ → I C₁ C₂ → ISteps M P I C₂ C₃ →
      ISteps M P I C₁ C₃

/-- The configuration family of an expression in focus: `⟨H ; φ ; K ; E[e]⟩`
for every context `K` and every trace `tr` already produced (§6.1, §6.2). -/
abbrev evalConf (H : Store) (φ : Activation) (e : Expr) : List Kont → List Event → Config :=
  fun K tr => .run H φ K (.eval e) tr

/-- Whether a configuration has an expression in focus (helper). -/
def Config.evalFocus : Config → Prop
  | .run _ _ _ (.eval _) _ => True
  | _ => False

/-- A frame `toCall` and `toLoop` look through: every frame but `call` and
`loop` (helper). -/
def Kont.Transparent (F : Kont) : Prop :=
  ∀ K, Kont.toCall (F :: K) = Kont.toCall K ∧ Kont.toLoop (F :: K) = Kont.toLoop K

/-- **The simulation relation, parametrized** (RUE-2517): what `eval`'s result
`r` promises about `→*` from the configuration family `C`, context `K` and
prior trace `tr`, with every run keeping the per-step invariant `I`. A value
reaches `E[v]` in `φ` (§6.2's (Search)); a panic reaches `↯κ` from every
context ((Panic-Lift) §6.2) when `pan` holds; an unwinding `return` reaches
the nearest caller ((D-Return) §6.9) and an unwinding `break` the nearest
loop's context ((D-Break) §6.10), from every context whose discarded frames
`G` accepts. `Equivalence.lean`'s `Sim` is `I`, `G` and `pan` all `True`. -/
def SimBy (M : FloatSig) (P : Program) (I : Config → Config → Prop)
    (G : List Kont → List Kont → Prop) (pan : Prop) (φ : Activation)
    (C : List Kont → List Event → Config) : EvalRes → Prop
  | .ok H v tr' => ∀ K tr, ISteps M P I (C K tr) (.run H φ K (.ret v) (tr ++ tr'))
  | .panic k tr' => pan → ∀ K tr, ISteps M P I (C K tr) (.panic k (tr ++ tr'))
  | .returned H v tr' => ∀ K tr φs K', Kont.toCall K = some (φs, K') → G K K' →
      ISteps M P I (C K tr) (.run H φs K' (.ret v) (tr ++ tr'))
  | .broke H sc tr' => ∀ K tr φs K' H' evs, Kont.toLoop K = some (φs, K') → G K K' →
      plainUnwind P.decls H (sc.drop φs.scope.length).reverse = .ok (H', evs) →
      ISteps M P I (C K tr) (.run H' φs K' (.ret .unit) (tr ++ tr' ++ evs))
  | .refused _ | .outOfFuel => True

/-- A run of **at least** `n` steps from every member of a configuration
family, whose end `Q` accepts given the trace before it (helper). -/
def LongBy (M : FloatSig) (P : Program) (Q : List Event → Config → Prop)
    (C : List Kont → List Event → Config) (n : Nat) : Prop :=
  ∀ K tr, ∃ m D, n ≤ m ∧ StepsN M P m (C K tr) D ∧ Q tr D

section simulation
variable {M : FloatSig} {P : Program} {I : Config → Config → Prop}
  {G : List Kont → List Kont → Prop} {pan : Prop}

/-- Runs under an invariant compose (§6.12) (helper). -/
theorem ISteps.trans {C₁ C₂ C₃ : Config} (h₁ : ISteps M P I C₁ C₂) (h₂ : ISteps M P I C₂ C₃) :
    ISteps M P I C₁ C₃ := by
  induction h₁ with
  | refl => exact h₂
  | step s hi _ ih => exact .step s hi (ih h₂)

/-- One step that keeps the invariant is a run (helper). -/
theorem ISteps.single {C₁ C₂ : Config} (s : Step M P C₁ C₂) (hi : I C₁ C₂) : ISteps M P I C₁ C₂ :=
  .step s hi (.refl _)

/-- One step is a run with the `True` invariant (helper). -/
theorem ISteps.one {C₁ C₂ : Config} (s : Step M P C₁ C₂) : ISteps M P (fun _ _ => True) C₁ C₂ :=
  .step s trivial (.refl _)

/-- A run under an invariant is a run (helper). -/
theorem ISteps.toSteps {C₁ C₂ : Config} (h : ISteps M P I C₁ C₂) : Steps M P C₁ C₂ := by
  induction h with
  | refl C => exact .refl C
  | step s _ _ ih => exact .step s ih

/-- **Peeling a step by determinism** (`Step.det`, §6): a run from `C` that
ends at a configuration with no expression in focus passes through `C`'s one
successor (helper). -/
theorem ISteps.peel {C C' D : Config} (hs : Step M P C C') (h : ISteps M P I C D)
    (hC : C.evalFocus) (hD : ¬ D.evalFocus) : ISteps M P I C' D := by
  cases h with
  | refl => exact absurd hC hD
  | step s _ rest => rw [Step.det hs s]; exact rest

/-- A run into the family carries its simulation back (helper). -/
theorem SimBy.pre {φ : Activation} {C C₂ : List Kont → List Event → Config} {r : EvalRes}
    (hpre : ∀ K tr, ISteps M P I (C K tr) (C₂ K tr)) (h : SimBy M P I G pan φ C₂ r) :
    SimBy M P I G pan φ C r := by
  cases r <;> simp only [SimBy] at h ⊢
  · intro K tr; exact (hpre K tr).trans (h K tr)
  · intro K tr φs K' hK hG; exact (hpre K tr).trans (h K tr φs K' hK hG)
  · intro K tr φs K' H' evs hK hG hu; exact (hpre K tr).trans (h K tr φs K' H' evs hK hG hu)
  · intro hp K tr; exact (hpre K tr).trans (h hp K tr)

/-- A run into the family that emits `tr₁` carries its simulation back to
the result with `tr₁` prefixed (§6.12's accumulating output) (helper). -/
theorem SimBy.withTrace {φ : Activation} {C C₂ : List Kont → List Event → Config} {r : EvalRes}
    {tr₁ : List Event} (hpre : ∀ K tr, ISteps M P I (C K tr) (C₂ K (tr ++ tr₁)))
    (h : SimBy M P I G pan φ C₂ r) : SimBy M P I G pan φ C (r.withTrace tr₁) := by
  cases r <;> simp only [SimBy, EvalRes.withTrace] at h ⊢
  · intro K tr; have := h K (tr ++ tr₁); simp only [List.append_assoc] at this
    exact (hpre K tr).trans this
  · intro K tr φs K' hK hG; have := h K (tr ++ tr₁) φs K' hK hG
    simp only [List.append_assoc] at this
    exact (hpre K tr).trans this
  · intro K tr φs K' H' evs hK hG hu; have := h K (tr ++ tr₁) φs K' H' evs hK hG hu
    simp only [List.append_assoc] at this ⊢
    exact (hpre K tr).trans this
  · intro hp K tr; have := h hp K (tr ++ tr₁); simp only [List.append_assoc] at this
    exact (hpre K tr).trans this

/-- **§6.2's (Search), once**: `eval`'s `bind` is an evaluation-state run
pushing a frame `F`, the operand run under `F`, and the context's simulation
from the operand's value. A `return` or a `break` passes through `F` because
`F` is neither a call frame nor a loop boundary and `G` does not see it
(`hG`), and a panic because (Panic-Lift) discards every context (helper). -/
theorem SimBy.bind {φ φ₁ : Activation} {C C₁ : List Kont → List Event → Config} {F : Kont}
    (hF : F.Transparent) (hC : ∀ K tr, ISteps M P I (C K tr) (C₁ (F :: K) tr))
    {r : EvalRes} (h₁ : SimBy M P I G pan φ₁ C₁ r) {k : Store → Val → EvalRes}
    (hk : ∀ H₁ v tr₁, r = .ok H₁ v tr₁ →
      SimBy M P I G pan φ (fun K tr => .run H₁ φ₁ (F :: K) (.ret v) tr) (k H₁ v))
    (hG : ∀ K K', G K K' → G (F :: K) K' := by intros; trivial) :
    SimBy M P I G pan φ C (r.bind k) := by
  cases r with
  | ok H₁ v tr₁ =>
      simp only [EvalRes.bind]
      exact SimBy.withTrace (fun K tr => (hC K tr).trans (h₁ (F :: K) tr)) (hk H₁ v tr₁ rfl)
  | returned H₁ v tr₁ =>
      simp only [EvalRes.bind, SimBy] at h₁ ⊢
      intro K tr φs K' hK hg
      exact (hC K tr).trans (h₁ (F :: K) tr φs K' (by rw [(hF K).1]; exact hK) (hG K K' hg))
  | broke H₁ sc tr₁ =>
      simp only [EvalRes.bind, SimBy] at h₁ ⊢
      intro K tr φs K' H' evs hK hg hu
      exact (hC K tr).trans
        (h₁ (F :: K) tr φs K' H' evs (by rw [(hF K).2]; exact hK) (hG K K' hg) hu)
  | panic κ tr₁ =>
      simp only [EvalRes.bind, SimBy] at h₁ ⊢
      intro hp K tr
      exact (hC K tr).trans (h₁ hp (F :: K) tr)
  | refused w => simp [EvalRes.bind, SimBy]
  | outOfFuel => simp [EvalRes.bind, SimBy]

/-- A result that is not a value passes through a transparent frame unchanged
(helper). -/
theorem SimBy.lift {φ φ₁ : Activation} {C C₁ : List Kont → List Event → Config} {F : Kont}
    (hF : F.Transparent) (hC : ∀ K tr, ISteps M P I (C K tr) (C₁ (F :: K) tr))
    {r : EvalRes} (h₁ : SimBy M P I G pan φ₁ C₁ r) (hr : ∀ H v tr, r ≠ .ok H v tr)
    (hG : ∀ K K', G K K' → G (F :: K) K' := by intros; trivial) :
    SimBy M P I G pan φ C r := by
  have := SimBy.bind (φ := φ) (k := fun _ _ => .outOfFuel) hF hC h₁
    (fun H v tr h => absurd h (hr H v tr)) hG
  cases r <;> simp_all [EvalRes.bind]

/-- §6.9's call boundary: the body's `returned` is caught at the `call φ`
frame, which is what `bindCall` turns into a value, and `G` accepts the frame
it discards (helper). -/
theorem SimBy.absorb {φ φ₁ : Activation} {C C₁ : List Kont → List Event → Config}
    (hC : ∀ K tr, ISteps M P I (C K tr) (C₁ (.call φ :: K) tr))
    {r : EvalRes} (h₁ : SimBy M P I G pan φ₁ C₁ r) {k : Store → Val → EvalRes}
    (hk : ∀ H₁ v tr₁, r = .ok H₁ v tr₁ →
      SimBy M P I G pan φ (fun K tr => .run H₁ φ₁ (.call φ :: K) (.ret v) tr) (k H₁ v))
    (hG : ∀ K, G (.call φ :: K) K := by intros; trivial) :
    SimBy M P I G pan φ C (r.bindCall k) := by
  cases r with
  | ok H₁ v tr₁ =>
      simp only [EvalRes.bindCall]
      exact SimBy.withTrace (fun K tr => (hC K tr).trans (h₁ (.call φ :: K) tr)) (hk H₁ v tr₁ rfl)
  | returned H₁ v tr₁ =>
      simp only [EvalRes.bindCall, SimBy] at h₁ ⊢
      intro K tr
      exact (hC K tr).trans (h₁ (.call φ :: K) tr φ K rfl (hG K))
  | panic κ tr₁ =>
      simp only [EvalRes.bindCall, SimBy] at h₁ ⊢
      intro hp K tr
      exact (hC K tr).trans (h₁ hp (.call φ :: K) tr)
  | _ => simp [EvalRes.bindCall, SimBy]

/-- Where no target has an expression in focus, a first step of the family
can be peeled off by determinism (helper). -/
theorem SimBy.peel {φ : Activation} {C C₂ : List Kont → List Event → Config} {r : EvalRes}
    (hs : ∀ K tr, Step M P (C K tr) (C₂ K tr)) (hC : ∀ K tr, (C K tr).evalFocus)
    (h : SimBy M P I G pan φ C r) : SimBy M P I G pan φ C₂ r := by
  cases r <;> simp only [SimBy] at h ⊢
  · intro K tr; exact (h K tr).peel (hs K tr) (hC K tr) (by simp [Config.evalFocus])
  · intro K tr φs K' hK hG
    exact (h K tr φs K' hK hG).peel (hs K tr) (hC K tr) (by simp [Config.evalFocus])
  · intro K tr φs K' H' evs hK hG hu
    exact (h K tr φs K' H' evs hK hG hu).peel (hs K tr) (hC K tr) (by simp [Config.evalFocus])
  · intro hp K tr; exact (h hp K tr).peel (hs K tr) (hC K tr) (by simp [Config.evalFocus])

/-- (D-Break) §6.10: its one step keeps the invariant wherever `G` accepts the
frames it discards (helper). -/
theorem simBy_brk {fuel : Nat} {H : Store} {φ : Activation}
    (hI : ∀ K tr φs K' H' evs, Kont.toLoop K = some (φs, K') → G K K' →
      plainUnwind P.decls H (φ.scope.drop φs.scope.length).reverse = .ok (H', evs) →
      I (.run H φ K (.eval .brk) tr) (.run H' φs K' (.ret .unit) (tr ++ evs)) := by
      intros; trivial) :
    SimBy M P I G pan φ (evalConf H φ .brk) (eval M (fuel + 1) P H φ .brk) := by
  simp only [eval]
  intro K tr φs K' H' evs hK hG hu
  simpa using ISteps.single (.brk hK hu) (hI K tr φs K' H' evs hK hG hu)

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

/-- A family with long runs has shorter ones (helper). -/
theorem LongBy.mono {Q : List Event → Config → Prop} {C : List Kont → List Event → Config}
    {m n : Nat} (hmn : m ≤ n) (h : LongBy M P Q C n) : LongBy M P Q C m := by
  intro K tr
  obtain ⟨k, D, hk, hD, hq⟩ := h K tr
  exact ⟨k, D, Nat.le_trans hmn hk, hD, hq⟩

/-- A run into a family with long runs is at least as long, and its end is
accepted wherever the family's is (helper). -/
theorem LongBy.pre {Q Q₂ : List Event → Config → Prop} {C C₂ : List Kont → List Event → Config}
    {n : Nat} (hpre : ∀ K tr, ∃ tr', Steps M P (C K tr) (C₂ K tr') ∧ ∀ D, Q₂ tr' D → Q tr D)
    (h : LongBy M P Q₂ C₂ n) : LongBy M P Q C n := by
  intro K tr
  obtain ⟨tr', hs, hQ⟩ := hpre K tr
  obtain ⟨j, hj⟩ := hs.toN
  obtain ⟨k, D, hk, hD, hq⟩ := h K tr'
  exact ⟨j + k, D, by omega, hj.trans hD, hQ D hq⟩

/-- The same with one step first: the run is one step longer (helper). -/
theorem LongBy.pre1 {Q Q₂ : List Event → Config → Prop} {C C₂ : List Kont → List Event → Config}
    {n : Nat}
    (hpre : ∀ K tr, ∃ C' tr', Step M P (C K tr) C' ∧ Steps M P C' (C₂ K tr') ∧
      ∀ D, Q₂ tr' D → Q tr D)
    (h : LongBy M P Q₂ C₂ n) : LongBy M P Q C (n + 1) := by
  intro K tr
  obtain ⟨C', tr', s, hs, hQ⟩ := hpre K tr
  obtain ⟨j, hj⟩ := hs.toN
  obtain ⟨k, D, hk, hD, hq⟩ := h K tr'
  exact ⟨j + k + 1, D, by omega, .step s (hj.trans hD), hQ D hq⟩

end simulation

end RueCore
