module

public import RueCore.Dynamics
public import RueCore.Statics.Lemmas

@[expose] public section

/-!
# RueCore.Dynamics.Lemmas — the lemmas about `Dynamics.lean`'s definitions (layer L2)

Every theorem `Dynamics.lean` held, moved here verbatim and in source order so
that the definitions layer holds definitions only (RUE-2460; README, "Layers").
The section headings are `Dynamics.lean`'s own, repeated where a moved
theorem sits under one.
-/

namespace RueCore

/-- `toVals` does not change a list's length, for `mult_toVal`'s array case
(helper). -/
theorem Contents.toVals_length : ∀ (cs : List Contents) (vs : List Val),
    Contents.toVals cs = some vs → vs.length = cs.length
  | [], vs, h => by simp [Contents.toVals] at h; subst h; rfl
  | c :: cs, vs, h => by
      simp only [Contents.toVals] at h
      split at h
      · rename_i v vs' hv hvs
        cases h
        simp [Contents.toVals_length cs vs' hvs]
      · cases h

/-- `Contents.mult` agrees with `Val.mult` on a hole-free contents: §6's
`Step.indexDrop` reads `leaf.mult` on the store's `Contents`, while `eval`'s
dynamic checks (RUE-2400) read `v.mult` on the `Val` a successful read
produces; this is what lets the two land on the same refusal. Serves
RUE-2289 part 2, the `eval ⇒ Step*` simulation. -/
theorem Contents.mult_toVal (D : Decls) (c : Contents) (v : Val) (h : c.toVal = some v) :
    c.mult D = v.mult D := by
  cases c <;> simp [Contents.toVal] at h
  all_goals (first | (subst h; rfl) | skip)
  all_goals (obtain ⟨vs, hvs, rfl⟩ := h)
  all_goals (simp [Contents.mult, Val.mult, Contents.toVals_length _ _ hvs])

/-- The bounds test, read as §6.5 states it (helper). -/
theorem inBoundsIdx_eq_true {i : Int} {n : Nat} :
    inBoundsIdx i n = true ↔ (0 ≤ i ∧ i < (n : Int)) := by
  simp [inBoundsIdx]

/-- A field list's events are its fields' events concatenated, left to right:
the flattening `dropContents_struct_events` states the order with (helper). -/
theorem dropEventsList_eq_flatten (D : Decls) :
    ∀ cs : List Contents, dropEventsList D cs = (cs.map (dropEvents D)).flatten
  | [] => rfl
  | c :: cs => by simp [dropEventsList, dropEventsList_eq_flatten D cs]

end RueCore
