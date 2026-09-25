module

public import RueCore.Step
public import RueCore.Dynamics.Lemmas

@[expose] public section

/-!
# RueCore.Step.Lemmas — the lemmas about `Step.lean`'s definitions (layer L2)

Every theorem `Step.lean` held but its demo witnesses, moved here verbatim and
in source order so that the definitions layer holds definitions only
(RUE-2460; README, "Layers"): the relation's determinism, `step_iff`, the
trichotomy and the stuck-state lemmas, and the lemmas that a monitor only
removes behaviour. The section headings are `Step.lean`'s own, repeated where
a moved theorem sits under one. The demo witnesses — programs run through the
relation — are in `Witnesses.lean`.
-/

namespace RueCore

/-! ## The sanity theorems -/

/-- Every `Step` is the one `step` computes (§6). -/
theorem Step.step_eq {M : FloatOps} {P : Program} {C C' : Config} (h : Step M P C C') :
    step M P C = .next C' := by
  cases h <;> simp_all [step, stepEval, stepArgs, stepRet, OpRes.toStep]

/-- **Determinism** of §6's reduction on the fragment: a configuration takes
at most one step. The rules' left-hand sides fix the focus and the top frame,
and the pairs that share one — (D-Use-Copy)/(D-Use-Move)/(D-Use-Declared-Linear),
(D-Seq)'s two cases, (D-If-T)/(D-If-F), an operator's value and trap rules,
(D-Index)/(D-Index-Trap) — are split by premises that are functions of the
configuration. -/
theorem Step.det {M : FloatOps} {P : Program} {C C₁ C₂ : Config}
    (h₁ : Step M P C C₁) (h₂ : Step M P C C₂) : C₁ = C₂ := by
  have e₁ := h₁.step_eq
  rw [h₂.step_eq] at e₁
  exact (StepOut.next.inj e₁).symm

/-- A terminal configuration takes no step: `✓n` and `↯κ` are final (§6.12). -/
theorem Step.terminal {M : FloatOps} {P : Program} {C C' : Config}
    (hC : C.Terminal) : ¬ Step M P C C' := by
  intro h
  cases h <;> simp [Config.Terminal] at hC

/-! ## `step` is `Step`, and the enumeration of what a configuration can be -/

/-- `stepEval`'s `next` is a `Step` (helper). -/
theorem stepEval_complete {M : FloatOps} {P : Program} {H : Store} {φ : Frame}
    {K : List Kont} {tr : List Event} {e : Expr} {C' : Config}
    (h : stepEval M P H φ K tr e = .next C') : Step M P (.run H φ K (.eval e) tr) C' := by
  cases e <;> simp only [stepEval] at h
  all_goals (repeat' split at h)
  all_goals (first | (simp at h) | skip)
  all_goals (try subst h)
  all_goals (try simp only [Bool.not_eq_true] at *)
  all_goals (constructor <;> first | assumption | rfl)

/-- `stepArgs`'s `next` is a `Step` (helper). -/
theorem stepArgs_complete {M : FloatOps} {P : Program} {H : Store} {φ : Frame}
    {K : List Kont} {tr : List Event} {vs : List Val} {t : ArgsTag} {C' : Config}
    (h : stepArgs P H φ K tr vs t = .next C') : Step M P (.run H φ K (.args t vs []) tr) C' := by
  cases t <;> simp only [stepArgs] at h
  all_goals (repeat' split at h)
  all_goals (first | (simp at h) | skip)
  all_goals (try subst h)
  all_goals (constructor <;> first | assumption | rfl)

/-- `stepRet`'s `next` is a `Step` (helper). -/
theorem stepRet_complete {M : FloatOps} {P : Program} {H : Store} {φ : Frame}
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
theorem step_iff {M : FloatOps} {P : Program} {C C' : Config} :
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
theorem stepEval_ne_halted {M : FloatOps} {P : Program} {H : Store} {φ : Frame}
    {K : List Kont} {tr : List Event} {e : Expr} : stepEval M P H φ K tr e ≠ .halted := by
  intro h
  cases e <;> simp only [stepEval] at h
  all_goals (repeat' split at h)
  all_goals simp at h

/-- `stepArgs` never answers `halted` (helper). -/
theorem stepArgs_ne_halted {P : Program} {H : Store} {φ : Frame} {K : List Kont}
    {tr : List Event} {vs : List Val} {t : ArgsTag} : stepArgs P H φ K tr vs t ≠ .halted := by
  intro h
  cases t <;> simp only [stepArgs] at h
  all_goals (repeat' split at h)
  all_goals simp at h

/-- `stepRet` never answers `halted` (helper). -/
theorem stepRet_ne_halted {M : FloatOps} {P : Program} {H : Store} {φ : Frame}
    {K : List Kont} {tr : List Event} {v : Val} {k : Kont} : stepRet M P H φ K tr v k ≠ .halted := by
  intro h
  cases k <;> simp only [stepRet, OpRes.toStep] at h
  all_goals (repeat' split at h)
  all_goals simp at h

/-- `step` answers `halted` exactly at the terminal configurations: `✓n` and
`↯κ` (§6.12's (Result-Ok) and (Result-Panic)). -/
theorem step_halted_iff {M : FloatOps} {P : Program} {C : Config} :
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
theorem Config.trichotomy (M : FloatOps) (P : Program) (C : Config) :
    (∃ C', Step M P C C') ∨ C.Terminal ∨ ∃ w, C.Stuck M P w := by
  cases h : step M P C with
  | next C' => exact .inl ⟨C', step_iff.mpr h⟩
  | halted => exact .inr (.inl (step_halted_iff.mp h))
  | stuck w => exact .inr (.inr ⟨w, h⟩)

/-- **Stuck, in `Step`'s own terms** (§6, §7): a configuration is stuck —
not terminal, and no rule of §6 applies to it — exactly when `step` names it
stuck. This is what makes `Config.Stuck` a statement about the relation and not
only about the function's labels. -/
theorem Config.stuck_iff {M : FloatOps} {P : Program} {C : Config} :
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
theorem Config.Stuck.no_step {M : FloatOps} {P : Program} {C C' : Config} {w : Violation}
    (h : C.Stuck M P w) : ¬ Step M P C C' := by
  intro hs
  have := hs.step_eq
  simp [Config.Stuck] at h
  rw [h] at this
  cases this

/-! ## Stuck states are §6's, and the monitors are absent (RUE-2314) -/

/-- `readAt` refuses only with §6's stuck states (helper). -/
theorem Contents.readAt_err : ∀ {c : Contents} {π : List Nat} {w : Violation},
    c.readAt π = .error w → w.isStuckState = true
  | _, [], _, h => by simp [Contents.readAt] at h
  | .hole, _ :: _, _, h => by simp [Contents.readAt] at h; subst h; rfl
  | .struct _ _ cs, f :: π, _, h => by
      simp only [Contents.readAt] at h
      split at h
      · exact Contents.readAt_err h
      · simp at h; subst h; rfl
  | .array _ _ cs, f :: π, _, h => by
      simp only [Contents.readAt] at h
      split at h
      · exact Contents.readAt_err h
      · simp at h; subst h; rfl
  | .int _ _ _, _ :: _, _, h | .float _ _, _ :: _, _, h | .bool _, _ :: _, _, h
  | .unit, _ :: _, _, h | .enum _ _ _ _, _ :: _, _, h => by
      simp [Contents.readAt] at h; subst h; rfl

mutual
/-- `split` refuses only with §6's stuck states (helper). -/
theorem Contents.splitResidue_err (D : Decls) : ∀ {c : Contents} {π : List Nat} {w : Violation},
    c.splitResidue D π = .error w → w.isStuckState = true
  | _, [], _, h => by simp [Contents.splitResidue] at h
  | .struct _ _ cs, f :: π, _, h => by
      simp only [Contents.splitResidue] at h; exact Contents.splitFields_err D h
  | .array _ _ cs, f :: π, _, h => by
      simp only [Contents.splitResidue] at h; exact Contents.splitFields_err D h
  | .hole, _ :: _, _, h => by simp [Contents.splitResidue] at h; subst h; rfl
  | .int _ _ _, _ :: _, _, h | .float _ _, _ :: _, _, h | .bool _, _ :: _, _, h
  | .unit, _ :: _, _, h | .enum _ _ _ _, _ :: _, _, h => by
      simp [Contents.splitResidue] at h; subst h; rfl

/-- `split`'s field step refuses only with §6's stuck states (helper). -/
theorem Contents.splitFields_err (D : Decls) :
    ∀ {cs : List Contents} {f : Nat} {π : List Nat} {w : Violation},
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
theorem dropContents_err (D : Decls) : ∀ {c : Contents} {w : Violation},
    dropContents D c = .error w → w.isStuckState = true
  | .hole, _, h | .int _ _ _, _, h | .float _ _, _, h | .bool _, _, h | .unit, _, h => by
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
theorem dropContentsList_err (D : Decls) : ∀ {cs : List Contents} {w : Violation},
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
theorem dropCell_err {D : Decls} {ℓ : Nat} {c : Contents} {w : Violation}
    (h : dropCell D ℓ c = .error w) : w.isStuckState = true := by
  simp only [dropCell] at h
  split at h
  · simp at h
  · split at h
    · simp at h; subst h; exact dropContents_err D ‹_›
    · simp at h

/-- `plainUnwind` refuses only with §6's stuck states (helper). -/
theorem plainUnwind_err {D : Decls} : ∀ {H : Store} {ls : List Nat} {w : Violation},
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
theorem plainResidue_err {D : Decls} {ℓ : Nat} : ∀ {rs : List Contents} {w : Violation},
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
theorem plainDestructure_err {D : Decls} {ℓ : Nat} {c : Contents} {πs : List Nat} {w : Violation}
    (h : plainDestructure D ℓ c πs = .error w) : w.isStuckState = true := by
  simp only [plainDestructure] at h
  split at h
  · simp at h; subst h; exact Contents.splitResidue_err D ‹_›
  · split at h
    · simp at h; subst h; exact plainResidue_err ‹_›
    · simp at h

/-- `rootCell` refuses only with §6's stuck states (helper). -/
theorem rootCell_err {H : Store} {φ : Frame} {i : Nat} {w : Violation}
    (h : rootCell H φ i = .error w) : w.isStuckState = true := by
  simp only [rootCell] at h
  repeat' split at h
  all_goals simp at h
  all_goals (subst h; rfl)

/-- Resolving a dynamic tail refuses only with §6's stuck states (helper). -/
theorem Contents.resolveDyn_err : ∀ {c : Contents} {is : List Int} {πs : List (List Nat)}
    {w : Violation}, c.resolveDyn is πs = .stuck w → w.isStuckState = true
  | c, [], [], _, h => by cases c <;> simp [Contents.resolveDyn] at h
  | c, i :: is, π :: πs, w, h => by
      cases c
      case array T cs =>
        simp only [Contents.resolveDyn] at h
        split at h
        · split at h
          · simp at h; subst h; rfl
          · split at h
            · simp at h; subst h; exact Contents.readAt_err ‹_›
            · split at h
              · simp at h
              · exact Contents.resolveDyn_err h
        · simp at h
      all_goals (simp [Contents.resolveDyn] at h; subst h; rfl)
  | c, [], _ :: _, _, h | c, _ :: _, [], _, h => by
      cases c <;> simp [Contents.resolveDyn] at h <;> (subst h; rfl)

/-- Navigating a dynamic place refuses only with §6's stuck states (helper). -/
theorem dynPlace_err {H : Store} {φ : Frame} {p : Place} {vs : List Val}
    {πs : List (List Nat)} {w : Violation}
    (h : dynPlace H φ p vs πs = .stuck w) : w.isStuckState = true := by
  simp only [dynPlace] at h
  repeat' split at h
  all_goals simp at h
  all_goals (try (subst h; rfl))
  · subst h; exact Contents.readAt_err ‹_›
  · subst h; exact Contents.resolveDyn_err ‹_›

/-- **§6's stuck states only** (RUE-2314): a configuration `step` finds stuck
is stuck on `useAfterMove`, `useAfterDrop`, `unbound` or `typeConfusion` —
never on `linearLeak`, `linearOverwrite`, `linearDiscard` or `ownedUnderCopy`,
the four monitors `eval` adds and §6.3, §6.5, §6.7 and §6.8 do not have. -/
theorem step_stuck_isStuckState {M : FloatOps} {P : Program} {C : Config} {w : Violation}
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
        | exact Contents.readAt_err ‹_›
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
        | exact Contents.readAt_err ‹_›
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
        | exact Contents.readAt_err ‹_›
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
and 3 of the adequacy proof read every scope exit through this. -/
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

/-- The residue monitor passes only where the plain `drop*` of the residue
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

/-- **The residue monitor only removes behaviour** (RUE-2314): where
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
theorem stepN_steps {M : FloatOps} {P : Program} : ∀ {n : Nat} {C : Config},
    Steps M P C (stepN M P n C)
  | 0, C => .refl C
  | n + 1, C => by
      simp only [stepN]
      split
      · rename_i C' h
        exact .step (step_iff.mpr h) stepN_steps
      · exact .refl C
      · exact .refl C

end RueCore
