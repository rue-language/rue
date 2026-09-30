module

public import RueCore.Dynamics
public import RueCore.Statics.Lemmas

@[expose] public section

/-!
# RueCore.Dynamics.Lemmas — the lemmas about `Dynamics.lean`'s definitions (layer L2)

Every theorem `Dynamics.lean` held, moved here verbatim and in source order so
that the definitions layer holds definitions only (RUE-2460; README, "Layers"),
and, after them, the evaluation-context interface below.
The section headings are `Dynamics.lean`'s own, repeated where a moved
theorem sits under one.

The last section is §6.2's evaluation contexts stated once for every
invariant a fuel induction over `eval` carries (`EvalInv`, RUE-2518): it is
here because every proof module imports this one.
-/

namespace RueCore

/-- `toVals` does not change a list's length, for `qual_toVal`'s array case
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

/-- `Contents.qual` agrees with `Val.qual` on a moved-out-free contents: §6's
`Step.indexDrop` reads `leaf.qual` on the store's `Contents`, while `eval`'s
dynamic checks (RUE-2400) read `v.qual` on the `Val` a successful read
produces; this is what lets the two land on the same refusal. Serves
RUE-2289 part 2, the `eval ⇒ Step*` simulation. -/
theorem Contents.qual_toVal (D : Decls) (c : Contents) (v : Val) (h : c.toVal = some v) :
    c.qual D = v.qual D := by
  cases c <;> simp [Contents.toVal] at h
  all_goals (first | (subst h; rfl) | skip)
  all_goals (obtain ⟨vs, hvs, rfl⟩ := h)
  all_goals (simp [Contents.qual, Val.qual, Contents.toVals_length _ _ hvs])

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

/-! ## §6.2's evaluation contexts, once for every invariant (RUE-2518)

Every fuel induction over `eval` proves some promise about a result, and
every one of them meets the same two operations on the way: `EvalRes.bind`
(§6.2's search through one operand) and `evalArgs` (the search through an
argument list). `EvalInv` names what such a promise must satisfy for both to
keep it, and `EvalInv.bind` and `EvalInv.args` prove, once, that they do: the
Felleisen-Hieb shape, where a form that runs its operands in order and then
computes owes only its computation. -/

/-- A prefixed trace composes (helper). -/
theorem EvalRes.withTrace_append (r : EvalRes) (tr₁ tr₂ : List Event) :
    (r.withTrace tr₂).withTrace tr₁ = r.withTrace (tr₁ ++ tr₂) := by
  cases r <;> simp [EvalRes.withTrace]

/-- `ofVals` distributes over concatenation (helper). -/
theorem Contents.ofVals_append :
    ∀ (vs ws : List Val), Contents.ofVals (vs ++ ws) = Contents.ofVals vs ++ Contents.ofVals ws
  | [], _ => rfl
  | v :: vs, ws => by simp [Contents.ofVals, Contents.ofVals_append vs ws]

/-- An argument list's outcome, sequenced into the context that consumes its
values: a failed argument is the whole form's outcome, and the values go to
`c` with the arguments' trace prefixed — the shape every argument-list form of
`eval` has (helper). -/
def ArgsRes.bind (a : ArgsRes) (c : Store → List Val → EvalRes) : EvalRes :=
  match a with
  | .abort r => r
  | .ok H vs tr => EvalRes.withTrace tr <| c H vs

/-- An argument list is its first argument, `bind`, then the rest (helper). -/
theorem ArgsRes.bind_evalArgs_cons (ev : Store → Expr → EvalRes) (H : Store) (e : Expr)
    (es : List Expr) (c : Store → List Val → EvalRes) :
    (evalArgs ev H (e :: es)).bind c
      = (ev H e).bind fun H₁ v => (evalArgs ev H₁ es).bind fun H₂ vs => c H₂ (v :: vs) := by
  simp only [evalArgs]
  cases ev H e with
  | ok H₁ v tr =>
      cases h : evalArgs ev H₁ es <;> simp [h, ArgsRes.bind, EvalRes.bind, EvalRes.withTrace_append]
  | _ => rfl

/-- **An invariant of evaluation, closed under §6.2's search** (helper).
`Pre H vs` is what a start holds of its store `H`, with the values `vs` of the
operands already run held; `Post H vs r` is the promise about the result `r`
of a run from there. `pre` and `seq` are the search's step: an operand run
from `H`, holding nothing, that produced a value, leaves a start holding that
value too, and a result from that start keeps the promise from `H` once the
operand's trace is prefixed. `hold` is the rest of the search: an operand's
result that the form passes on unchanged keeps the promise with the held
values abandoned. -/
structure EvalInv where
  Pre : Store → List Val → Prop
  Post : Store → List Val → EvalRes → Prop
  pre : ∀ {H vs H₁ v tr}, Pre H vs → Post H [] (.ok H₁ v tr) → Pre H₁ (vs ++ [v])
  seq : ∀ {H vs H₁ v tr r}, Pre H vs → Post H [] (.ok H₁ v tr) → Post H₁ (vs ++ [v]) r →
    Post H vs (r.withTrace tr)
  hold : ∀ {H vs r}, Pre H vs → Post H [] r → Post H vs r

/-- **§6.2's search through one operand keeps any `EvalInv`** (helper): the
operand's promise, and the context's from the store its value left, holding
that value. -/
theorem EvalInv.bind (I : EvalInv) {H : Store} {vs : List Val} {r : EvalRes}
    {k : Store → Val → EvalRes} (hp : I.Pre H vs) (hr : I.Post H [] r)
    (hk : ∀ H₁ v tr, r = .ok H₁ v tr → I.Pre H₁ (vs ++ [v]) → I.Post H₁ (vs ++ [v]) (k H₁ v)) :
    I.Post H vs (r.bind k) := by
  cases r with
  | ok H₁ v tr => exact I.seq hp hr (hk H₁ v tr rfl (I.pre hp hr))
  | _ => exact I.hold hp hr

/-- **§6.2's search through an argument list keeps any `EvalInv`** (helper):
operands in order, then compute. Each argument keeps the promise from wherever
it starts, and the computation keeps it holding every value. -/
theorem EvalInv.args (I : EvalInv) {ev : Store → Expr → EvalRes}
    (hev : ∀ H vs e, I.Pre H vs → I.Post H [] (ev H e)) :
    ∀ (es : List Expr) {H : Store} {vs : List Val} {c : Store → List Val → EvalRes},
      I.Pre H vs → (∀ H₁ vs', I.Pre H₁ (vs ++ vs') → I.Post H₁ (vs ++ vs') (c H₁ vs')) →
      I.Post H vs ((evalArgs ev H es).bind c)
  | [], H, vs, c, hp, hc => by
      have h := hc H [] (by simpa using hp)
      simp only [List.append_nil] at h
      simp only [evalArgs, ArgsRes.bind]
      cases hr : c H [] <;> rw [hr] at h <;> simpa [EvalRes.withTrace] using h
  | e :: es, H, vs, c, hp, hc => by
      rw [ArgsRes.bind_evalArgs_cons]
      exact I.bind hp (hev H vs e hp) fun H₁ v _ _ hp₁ =>
        EvalInv.args I hev es hp₁ fun H₂ vs' hp₂ => by
          simpa using hc H₂ (v :: vs') (by simpa using hp₂)

end RueCore
