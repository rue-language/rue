module

public import RueCore.Step.Lemmas

@[expose] public section

/-!
# RueCore.Retire — no program reaches a retired cell (layer L2)

`useAfterDrop` is the refusal `eval` raises when it reaches a retired (`†`)
cell (§6.1), and `step` names the same stuck state. This module proves that
neither is ever reached from a program's start, **checked or not**
(RUE-2496): `run_no_use_after_drop` over the interpreter at every fuel and
float model, and `step_no_use_after_drop` over §6's relation from
`Config.init`. Neither has a typing hypothesis. `no_use_after_drop`
(`Soundness.lean`) states the first for checked programs, as a corollary of
§7's invariant; these say its `ProgramTyped` hypothesis is redundant for runs
from the start. They do not say the guard is dead code: from an *open*
configuration — a frame naming a cell that is already `†` — the guard does
fire (`Examples.lean`), and the Spec's counter-example `Sharp.retired_cell`
is one such configuration, not reached from `Config.init`.

**The invariant** is structural, not a typing fact. `†` enters the store in
two ways only: as the reserved slot of a minted value identity (`introVal`),
which no binding names, and when a scope teardown retires the cells its
scope record owes a drop (`dropRetire`, `unwindLocs`, and `Step`'s plain
versions). Every binding cell is minted fresh at the end of the store and
bound at once, and a scope record lists each cell once. So:

* over `eval` (`Retire.eval_live`): if every cell the frame's environment and
  scope record name is live, and the record owes each cell once
  (`LiveFrame`), then an evaluation that yields a value retired no cell that
  was live before it; an unwinding `return` retired at most the frame's
  scope record; a `break` retired nothing live before it and hands its loop
  the frame's record extended by distinct fresh cells, still live; and no
  refusal is `useAfterDrop` (`LivePost`). The induction is on fuel, one case
  per `eval` rule, and the empty frame of `run` starts it.
* over `Step` (`Retire.step_live`): every configuration keeps the stack's
  shape (`Shape`: an `endscope` marker or a loop boundary sits under a frame
  that extends its own at the end of the scope record, and every frame's
  environment is its record reversed), and every cell owed a drop by the
  frame in force or by a suspended caller is live and owed once
  (`StackLive`). `step` keeps it and never answers `stuck .useAfterDrop`
  under it, and `Config.init` has it.

A scope teardown walks only cells the invariant says are live and distinct,
so it never meets `†` and never retires a cell twice; a lookup through the
environment finds a live cell; and every other helper a rule calls refuses
with some other violation, never `useAfterDrop`.

The theorems quantify over any `FloatOps`, not only a `FloatModel`: they hold
even for float operations satisfying none of the laws. The argument uses that
in the fragment only a frame's environment names a cell. Loans (§5.4, outside
the fragment, Phase D, RUE-2238) add references as a second way to name one,
and there the property is expected to rest on the statics again.
-/

namespace RueCore.Retire

/-! ## The invariant over `eval` -/

/-- Cell `ℓ` of the store is live: it holds contents, not the retired marker
`†` (helper). -/
def Live (H : Store) (ℓ : Nat) : Prop := ∃ c, H[ℓ]? = some (.full c)

/-- A live cell is in the store (helper). -/
theorem Live.lt {H : Store} {ℓ : Nat} (h : Live H ℓ) : ℓ < H.length := by
  obtain ⟨c, hc⟩ := h
  cases Nat.lt_or_ge ℓ H.length with
  | inl hl => exact hl
  | inr hg => rw [List.getElem?_eq_none hg] at hc; cases hc

/-- A live cell is not retired (helper). -/
theorem Live.ne_dead {H : Store} {ℓ : Nat} (h : Live H ℓ) : H[ℓ]? ≠ some .dead := by
  obtain ⟨c, hc⟩ := h
  rw [hc]; intro h'; cases h'

/-- Writing contents into any cell keeps every live cell live (helper). -/
theorem Live.set_full {H : Store} {ℓ : Nat} (h : Live H ℓ) (ℓ' : Nat) (c : Contents) :
    Live (H.set ℓ' (.full c)) ℓ := by
  cases Nat.decEq ℓ' ℓ with
  | isTrue he => subst he; exact ⟨c, List.getElem?_set_self h.lt⟩
  | isFalse hn => rw [Live, List.getElem?_set_ne hn]; exact h

/-- Appending to the store keeps every live cell live (helper). -/
theorem Live.append {H : Store} {ℓ : Nat} (h : Live H ℓ) (H' : Store) : Live (H ++ H') ℓ := by
  rw [Live, List.getElem?_append_left h.lt]; exact h

/-- The store grew: it is no shorter, and no live cell was retired (helper). -/
def Grow (H H' : Store) : Prop := H.length ≤ H'.length ∧ ∀ ℓ, Live H ℓ → Live H' ℓ

/-- An unchanged store has grown (helper). -/
theorem Grow.refl (H : Store) : Grow H H := ⟨Nat.le_refl _, fun _ h => h⟩

/-- Growth composes (helper). -/
theorem Grow.trans {H H₁ H₂ : Store} (h₁ : Grow H H₁) (h₂ : Grow H₁ H₂) : Grow H H₂ :=
  ⟨Nat.le_trans h₁.1 h₂.1, fun ℓ h => h₂.2 ℓ (h₁.2 ℓ h)⟩

/-- Allocation appends, so it grows the store: a binding cell (§6.7, §6.9) or a
value identity's reserved `†` slot (`introVal`) (helper). -/
theorem Grow.append (H H' : Store) : Grow H (H ++ H') :=
  ⟨by simp, fun _ h => h.append H'⟩

/-- Writing contents into a cell — a move's `⊘`, an assignment, an `@drop` — grows
the store (helper). -/
theorem Grow.set_full (H : Store) (ℓ : Nat) (c : Contents) : Grow H (H.set ℓ (.full c)) :=
  ⟨by simp, fun _ h => h.set_full ℓ c⟩

/-- A cell at or above the old store's length is not one of its live cells
(helper). -/
theorem Live.ne_of_le {H : Store} {ℓ m : Nat} (h : Live H ℓ) (hm : H.length ≤ m) : ℓ ≠ m := by
  intro he; subst he; exact Nat.lt_irrefl _ (Nat.lt_of_lt_of_le h.lt hm)

/-- **The frame invariant**: every cell the environment names, and every cell
the scope record owes a drop, is live, and the record owes each at most once
(helper). -/
def LiveFrame (H : Store) (φ : Frame) : Prop :=
  (∀ ℓ ∈ φ.env, Live H ℓ) ∧ (∀ ℓ ∈ φ.scope, Live H ℓ) ∧ φ.scope.Nodup

/-- The frame invariant survives growth (helper). -/
theorem LiveFrame.grow {H H' : Store} {φ : Frame} (h : LiveFrame H φ) (hg : Grow H H') :
    LiveFrame H' φ :=
  ⟨fun ℓ hℓ => hg.2 ℓ (h.1 ℓ hℓ), fun ℓ hℓ => hg.2 ℓ (h.2.1 ℓ hℓ), h.2.2⟩

/-- **What an evaluation keeps**, by outcome (helper). A value retires no cell
that was live before it (only the cells it minted itself). An unwinding
`return` may retire the frame's scope record, and nothing else live before it.
A `break` retires nothing live before it, and the scope record it carries is
the frame's own, extended by distinct cells it minted, still live. And no
refusal is `useAfterDrop`. -/
def LivePost (H : Store) (φ : Frame) : EvalRes → Prop
  | .ok H' _ _ => Grow H H'
  | .returned H' _ _ => H.length ≤ H'.length ∧ ∀ ℓ, ℓ ∉ φ.scope → Live H ℓ → Live H' ℓ
  | .broke H' sc _ => Grow H H' ∧ ∃ xs, sc = φ.scope ++ xs ∧ xs.Nodup ∧
      ∀ ℓ ∈ xs, H.length ≤ ℓ ∧ Live H' ℓ
  | .panic _ _ => True
  | .stuck w => w ≠ .useAfterDrop
  | .outOfFuel => True

/-- Prefixing a trace changes no store (helper). -/
theorem LivePost.withTrace {H : Store} {φ : Frame} {r : EvalRes} (h : LivePost H φ r)
    (tr : List Event) : LivePost H φ (r.withTrace tr) := by
  cases r <;> exact h

/-- An evaluation that started later, in a grown store, keeps the promise
relative to the earlier store (helper). -/
theorem LivePost.lift {H H₁ : Store} {φ : Frame} {r : EvalRes} (hg : Grow H H₁)
    (h : LivePost H₁ φ r) : LivePost H φ r := by
  cases r with
  | ok H' v tr => exact hg.trans h
  | returned H' v tr => exact ⟨Nat.le_trans hg.1 h.1, fun ℓ hn hl => h.2 ℓ hn (hg.2 ℓ hl)⟩
  | broke H' sc tr =>
      obtain ⟨hg', xs, hsc, hnd, hxs⟩ := h
      exact ⟨hg.trans hg', xs, hsc, hnd, fun ℓ hℓ => ⟨Nat.le_trans hg.1 (hxs ℓ hℓ).1, (hxs ℓ hℓ).2⟩⟩
  | panic k tr => trivial
  | stuck w => exact h
  | outOfFuel => trivial

/-- §6.2's search keeps the promise (helper). -/
theorem LivePost.andThen {H : Store} {φ : Frame} {r : EvalRes} {k : Store → Val → EvalRes}
    (h : LivePost H φ r) (hk : ∀ H₁ v, Grow H H₁ → LivePost H₁ φ (k H₁ v)) :
    LivePost H φ (r.andThen k) := by
  cases r with
  | ok H₁ v tr => exact ((hk H₁ v h).lift h).withTrace tr
  | _ => exact h

/-- A scope opened on top of the frame — a `let`'s cell, a `match` arm's
payload cells — keeps the promise when the body's non-value outcomes pass
through it (helper). -/
theorem LivePost.scoped {H H₁ : Store} {φ φ' : Frame} {ys : List Nat} {r : EvalRes}
    {k : Store → Val → EvalRes}
    (hg : Grow H H₁) (hys : ∀ ℓ ∈ ys, H.length ≤ ℓ ∧ Live H₁ ℓ) (hnd : ys.Nodup)
    (hsc : φ'.scope = φ.scope ++ ys) (h : LivePost H₁ φ' r)
    (hk : ∀ H₂ v, Grow H₁ H₂ → LivePost H φ (k H₂ v)) :
    LivePost H φ (r.andThen k) := by
  cases r with
  | ok H₂ v tr => exact (hk H₂ v h).withTrace tr
  | returned H' v tr =>
      refine ⟨Nat.le_trans hg.1 h.1, fun ℓ hn hl => h.2 ℓ ?_ (hg.2 ℓ hl)⟩
      rw [hsc]
      intro hm
      rcases List.mem_append.mp hm with hm | hm
      · exact hn hm
      · exact hl.ne_of_le (hys ℓ hm).1 rfl
  | broke H' sc tr =>
      obtain ⟨hg', xs, hsc', hnd', hxs⟩ := h
      refine ⟨hg.trans hg', ys ++ xs, by rw [hsc', hsc, List.append_assoc], ?_, ?_⟩
      · refine List.nodup_append.mpr ⟨hnd, hnd', fun a ha b hb => ?_⟩
        exact ((hys a ha).2).ne_of_le (hxs b hb).1
      · intro ℓ hℓ
        rcases List.mem_append.mp hℓ with hm | hm
        · exact ⟨(hys ℓ hm).1, hg'.2 ℓ (hys ℓ hm).2⟩
        · exact ⟨Nat.le_trans hg.1 (hxs ℓ hm).1, (hxs ℓ hm).2⟩
  | panic k tr => trivial
  | stuck w => exact h
  | outOfFuel => trivial

/-! ### The helpers never refuse with `useAfterDrop` -/

/-- `H(ℓ)@π` refuses only with `useAfterMove` or `typeConfusion` (helper). -/
theorem Contents.readAt_ne_uad : ∀ (π : List Nat) {c : Contents} {w : Violation},
    c.readAt π = .error w → w ≠ .useAfterDrop
  | [], c, w, h => by simp [Contents.readAt] at h
  | f :: π, c, w, h => by
      cases c with
      | hole => simp [Contents.readAt] at h; subst h; simp
      | struct s i cs =>
          simp only [Contents.readAt] at h
          split at h
          · exact Contents.readAt_ne_uad π h
          · cases h; simp
      | array T i cs =>
          simp only [Contents.readAt] at h
          split at h
          · exact Contents.readAt_ne_uad π h
          · cases h; simp
      | _ => simp [Contents.readAt] at h; subst h; simp

/-- Resolving a dynamic tail never refuses with `useAfterDrop` (helper). -/
theorem Contents.resolveDyn_ne_uad : ∀ (is : List Int) (πs : List (List Nat)) {c : Contents}
    {w : Violation}, c.resolveDyn is πs = .stuck w → w ≠ .useAfterDrop
  | is, πs, c, w, h => by
      unfold Contents.resolveDyn at h
      split at h
      · cases h
      · split at h
        · split at h
          · cases h; simp
          · split at h
            · cases h; exact Contents.readAt_ne_uad _ (by assumption)
            · split at h
              · cases h
              · exact Contents.resolveDyn_ne_uad _ _ h
        · cases h
      · cases h; simp

/-- Navigating a dynamic place from a frame whose environment names live cells
never refuses with `useAfterDrop` (helper). -/
theorem dynPlace_ne_uad {H : Store} {φ : Frame} {p : Place} {vs : List Val}
    {πs : List (List Nat)} {w : Violation} (hφ : ∀ ℓ ∈ φ.env, Live H ℓ)
    (h : dynPlace H φ p vs πs = .stuck w) : w ≠ .useAfterDrop := by
  unfold dynPlace at h
  split at h
  · cases h; simp
  · split at h
    · cases h; simp
    · rename_i ℓ hρ
      split at h
      · cases h; simp
      · rename_i hd
        exact absurd hd ((hφ ℓ (List.mem_of_getElem? hρ)).ne_dead)
      · split at h
        · cases h; exact Contents.readAt_ne_uad _ (by assumption)
        · split at h
          · cases h
          · cases h
          · cases h; exact Contents.resolveDyn_ne_uad _ _ (by assumption)

mutual
/-- §6.11's walk refuses only with `unbound` (helper). -/
theorem dropContents_ne_uad {D : Decls} : ∀ {c : Contents} {w : Violation},
    dropContents D c = .error w → w ≠ .useAfterDrop
  | .hole, _, h | .int _ _ _, _, h | .float _ _, _, h | .bool _, _, h | .unit, _, h => by
      simp [dropContents] at h
  | .struct s i cs, w, h => by
      simp only [dropContents] at h
      split at h
      · cases h; simp
      · split at h
        · cases h; exact dropContentsList_ne_uad (by assumption)
        · cases h
  | .enum _ _ _ cs, _, h => by simp only [dropContents] at h; exact dropContentsList_ne_uad h
  | .array _ _ cs, _, h => by simp only [dropContents] at h; exact dropContentsList_ne_uad h

/-- The same over a list (helper). -/
theorem dropContentsList_ne_uad {D : Decls} : ∀ {cs : List Contents} {w : Violation},
    dropContentsList D cs = .error w → w ≠ .useAfterDrop
  | [], _, h => by simp [dropContentsList] at h
  | c :: cs, _, h => by
      simp only [dropContentsList] at h
      split at h
      · cases h; exact dropContents_ne_uad (by assumption)
      · split at h
        · cases h; exact dropContentsList_ne_uad (by assumption)
        · cases h
end

/-- A binding's drop never refuses with `useAfterDrop` (helper). -/
theorem dropCell_ne_uad {D : Decls} {ℓ : Nat} {c : Contents} {w : Violation}
    (h : dropCell D ℓ c = .error w) : w ≠ .useAfterDrop := by
  unfold dropCell at h
  split at h
  · cases h
  · split at h
    · cases h; exact dropContents_ne_uad (by assumption)
    · cases h

mutual
/-- §6.3's `split` never refuses with `useAfterDrop` (helper). -/
theorem Contents.splitResidue_ne_uad {D : Decls} : ∀ {c : Contents} {π : List Nat}
    {w : Violation}, c.splitResidue D π = .error w → w ≠ .useAfterDrop
  | c, [], _, h => by simp [Contents.splitResidue] at h
  | .struct _ _ cs, f :: π, _, h => by
      simp only [Contents.splitResidue] at h; exact Contents.splitFields_ne_uad h
  | .array _ _ cs, f :: π, _, h => by
      simp only [Contents.splitResidue] at h; exact Contents.splitFields_ne_uad h
  | .hole, _ :: _, _, h => by simp [Contents.splitResidue] at h; subst h; simp
  | .int _ _ _, _ :: _, _, h | .float _ _, _ :: _, _, h | .bool _, _ :: _, _, h
  | .unit, _ :: _, _, h | .enum _ _ _ _, _ :: _, _, h => by
      simp [Contents.splitResidue] at h; subst h; simp

/-- The same at one node's members (helper). -/
theorem Contents.splitFields_ne_uad {D : Decls} : ∀ {cs : List Contents} {f : Nat}
    {π : List Nat} {w : Violation}, Contents.splitFields D cs f π = .error w →
      w ≠ .useAfterDrop
  | [], _, _, _, h => by simp [Contents.splitFields] at h; subst h; simp
  | c :: cs, 0, π, _, h => by
      simp only [Contents.splitFields] at h
      split at h
      · cases h; exact Contents.splitResidue_ne_uad (by assumption)
      · cases h
  | c :: cs, f + 1, π, _, h => by
      simp only [Contents.splitFields] at h
      split at h
      · cases h; exact Contents.splitFields_ne_uad (by assumption)
      · cases h
end

/-- The residue's `drop*` never refuses with `useAfterDrop` (helper). -/
theorem dropResidue_ne_uad {D : Decls} {ℓ : Nat} : ∀ {rs : List Contents} {w : Violation},
    dropResidue D ℓ rs = .error w → w ≠ .useAfterDrop
  | [], _, h => by simp [dropResidue] at h
  | r :: rs, _, h => by
      simp only [dropResidue] at h
      split at h
      · cases h; simp
      · split at h
        · cases h; exact dropContents_ne_uad (by assumption)
        · split at h
          · cases h; exact dropResidue_ne_uad (by assumption)
          · cases h

/-- §6.3's destructure never refuses with `useAfterDrop` (helper). -/
theorem Contents.destructure_ne_uad {D : Decls} {ℓ : Nat} {c : Contents} {πs : List Nat}
    {w : Violation} (h : c.destructure D ℓ πs = .error w) : w ≠ .useAfterDrop := by
  unfold Contents.destructure at h
  split at h
  · cases h; exact Contents.splitResidue_ne_uad (by assumption)
  · split at h
    · cases h; exact dropResidue_ne_uad (by assumption)
    · cases h

/-! ### Retiring cells -/

/-- `drop-retire` of a live cell never meets `†`, and retires exactly that
cell (helper). -/
theorem dropRetire_live {D : Decls} {H : Store} {ℓ : Nat} (hl : Live H ℓ) :
    (∀ w, dropRetire D H ℓ = .error w → w ≠ .useAfterDrop) ∧
      ∀ H' evs, dropRetire D H ℓ = .ok (H', evs) → H' = H.set ℓ .dead := by
  obtain ⟨c, hc⟩ := hl
  unfold dropRetire
  rw [hc]
  refine ⟨fun w h => ?_, fun H' evs h => ?_⟩
  · simp only [] at h
    split at h
    · cases h; simp
    · split at h
      · cases h; exact dropCell_ne_uad (by assumption)
      · cases h
  · simp only [] at h
    split at h
    · cases h
    · split at h
      · cases h
      · cases h; rfl

/-- What `run-scope-drops` over distinct live cells does: it never meets
`†`, keeps the store's length, and leaves every other cell as it was
(helper). -/
def UnwindPost (H : Store) (ls : List Nat) : Except Violation (Store × List Event) → Prop
  | .error w => w ≠ .useAfterDrop
  | .ok (H', _) => H'.length = H.length ∧ ∀ ℓ, ℓ ∉ ls → H'[ℓ]? = H[ℓ]?

/-- `run-scope-drops` over distinct live cells keeps `UnwindPost` (helper). -/
theorem unwindLocs_live {D : Decls} : ∀ {H : Store} {ls : List Nat}, ls.Nodup →
    (∀ ℓ ∈ ls, Live H ℓ) → UnwindPost H ls (unwindLocs D H ls)
  | H, [], _, _ => ⟨rfl, fun _ _ => rfl⟩
  | H, ℓ :: rest, hnd, hl => by
      obtain ⟨hnotin, hnd'⟩ := List.nodup_cons.mp hnd
      have hℓ := hl ℓ (List.mem_cons_self ..)
      obtain ⟨herr, hok⟩ := dropRetire_live (D := D) hℓ
      simp only [unwindLocs]
      split
      · rename_i w hw; exact herr w hw
      · rename_i H₁ evs hr
        have hH₁ := hok H₁ evs hr
        have hl₁ : ∀ m ∈ rest, Live H₁ m := by
          intro m hm
          have hne : ℓ ≠ m := fun he => hnotin (he ▸ hm)
          rw [hH₁, Live, List.getElem?_set_ne hne]
          exact hl m (List.mem_cons_of_mem _ hm)
        have ih := unwindLocs_live (D := D) hnd' hl₁
        split
        · rename_i w hw; rw [hw] at ih; exact ih
        · rename_i H₂ evs' hr'
          rw [hr'] at ih
          obtain ⟨hlen, heq⟩ := ih
          refine ⟨by rw [hlen, hH₁, List.length_set], fun m hm => ?_⟩
          have hne : ℓ ≠ m := fun he => hm (he ▸ List.mem_cons_self ..)
          rw [heq m (fun h' => hm (List.mem_cons_of_mem _ h')), hH₁, List.getElem?_set_ne hne]

/-- A scope record read newest-first owes each cell once, as it did oldest-first
(helper). -/
theorem nodup_reverse {l : List Nat} (h : l.Nodup) : l.reverse.Nodup :=
  List.pairwise_reverse.mpr (h.imp (fun h' he => h' he.symm))

/-- Retiring a record's cells leaves every live cell outside it live (helper). -/
theorem UnwindPost.grow {H H' : Store} {ls : List Nat} {evs : List Event}
    (h : UnwindPost H ls (.ok (H', evs))) :
    H.length ≤ H'.length ∧ ∀ ℓ, ℓ ∉ ls → Live H ℓ → Live H' ℓ :=
  ⟨Nat.le_of_eq h.1.symm, fun ℓ hn hl => by rw [Live, h.2 ℓ hn]; exact hl⟩

/-! ### Minting -/

/-- (D-Call)'s and (D-Match)'s minting: the new cells are live, distinct, and
above the old store, and nothing live before is touched (helper). -/
theorem mintParams_live : ∀ (H : Store) (vs : List Val),
    Grow H (mintParams H vs).1 ∧ (mintParams H vs).2.Nodup ∧
      ∀ ℓ ∈ (mintParams H vs).2, H.length ≤ ℓ ∧ Live (mintParams H vs).1 ℓ
  | H, [] => ⟨Grow.refl H, List.nodup_nil, fun _ h => by simp [mintParams] at h⟩
  | H, v :: vs => by
      obtain ⟨hg, hnd, hl⟩ := mintParams_live (H ++ [Cell.full (Contents.ofVal v)]) vs
      simp only [mintParams]
      refine ⟨(Grow.append H _).trans hg, List.nodup_cons.mpr ⟨fun hm => ?_, hnd⟩, ?_⟩
      · have := (hl _ hm).1; simp at this; exact Nat.not_succ_le_self _ this
      · intro ℓ hℓ
        rcases List.mem_cons.mp hℓ with he | hm
        · subst he
          refine ⟨Nat.le_refl _, hg.2 _ ⟨Contents.ofVal v, ?_⟩⟩
          simp
        · have := hl ℓ hm
          exact ⟨Nat.le_trans (by simp) this.1, this.2⟩

/-! ### The operators and the value introductions -/

/-- §6.4's operators touch no cell (helper). -/
theorem OpRes.toRes_live {H : Store} {φ : Frame} (o : OpRes) : LivePost H φ (o.toRes H) := by
  cases o with
  | val v => exact Grow.refl H
  | trap k => trivial
  | confused => simp [OpRes.toRes, LivePost]

/-- Minting a value identity appends a `†` slot no binding names (helper). -/
theorem introVal_live {D : Decls} {H : Store} {φ : Frame} (mk : Nat → Val) :
    LivePost H φ (introVal D H mk) := by
  unfold introVal
  split
  · exact Grow.append H _
  · simp [LivePost]

/-- The promise over an argument list (helper). -/
def ArgsLive (H : Store) (φ : Frame) : ArgsRes → Prop
  | .ok H' _ _ => Grow H H'
  | .abort r => LivePost H φ r

/-- An argument list keeps the promise, argument by argument (§6.2's left-to-right
search) (helper). -/
theorem evalArgs_live {φ : Frame} {ev : Store → Expr → EvalRes}
    (hev : ∀ H e, LiveFrame H φ → LivePost H φ (ev H e)) :
    ∀ (H : Store) (es : List Expr), LiveFrame H φ → ArgsLive H φ (evalArgs ev H es)
  | H, [], _ => Grow.refl H
  | H, e :: es, hf => by
      simp only [evalArgs]
      have h₁ := hev H e hf
      cases hr : ev H e with
      | ok H₁ v tr =>
          rw [hr] at h₁
          have h₂ := evalArgs_live hev H₁ es (hf.grow h₁)
          dsimp only
          cases hra : evalArgs ev H₁ es with
          | ok H₂ vs tr₂ => rw [hra] at h₂; exact h₁.trans h₂
          | abort r => rw [hra] at h₂; exact (h₂.lift h₁).withTrace tr
      | _ => rw [hr] at h₁; exact h₁

/-! ### The invariant, by induction on fuel -/

/-- A cell the environment names is live (helper). -/
theorem LiveFrame.root {H : Store} {φ : Frame} {i ℓ : Nat} (h : LiveFrame H φ)
    (hρ : φ.env[i]? = some ℓ) : Live H ℓ :=
  h.1 ℓ (List.mem_of_getElem? hρ)

/-- **The invariant over `eval`**: from a frame whose cells are live and owed
once, every evaluation keeps `LivePost`, at every fuel (helper). -/
theorem eval_live (M : FloatOps) (P : Program) :
    ∀ (fuel : Nat) (H : Store) (φ : Frame) (e : Expr), LiveFrame H φ →
      LivePost H φ (eval M fuel P H φ e) := by
  intro fuel
  induction fuel with
  | zero => intro H φ e _; simp only [eval]; trivial
  | succ n ih =>
    intro H φ e hf
    have hargs := fun (H' : Store) (es : List Expr) (hf' : LiveFrame H' φ) =>
      evalArgs_live (fun H'' e' hf'' => ih H'' φ e' hf'') H' es hf'
    cases e with
    | intLit w sg m => exact Grow.refl H
    | floatLit w l => exact Grow.refl H
    | boolLit b => exact Grow.refl H
    | unitLit => exact Grow.refl H
    | use p =>
        simp only [eval]
        split
        · simp [LivePost]
        · rename_i ℓ hρ
          split
          · simp [LivePost]
          · rename_i hd; exact absurd hd (hf.root hρ).ne_dead
          · split
            · split
              · exact Contents.readAt_ne_uad _ (by assumption)
              · split
                · exact Contents.destructure_ne_uad (by assumption)
                · split
                  · simp [LivePost]
                  · split
                    · simp [LivePost]
                    · exact Grow.set_full H ℓ _
            · split
              · exact Contents.readAt_ne_uad _ (by assumption)
              · split
                · simp [LivePost]
                · split
                  · exact Grow.refl H
                  · split
                    · simp [LivePost]
                    · exact Grow.set_full H ℓ _
    | binop op e₁ e₂ =>
        simp only [eval]
        refine (ih H φ e₁ hf).andThen (fun H₁ v₁ hg₁ => ?_)
        refine (ih H₁ φ e₂ (hf.grow hg₁)).andThen (fun H₂ v₂ _ => ?_)
        exact OpRes.toRes_live _
    | unop op e₁ =>
        simp only [eval]
        exact (ih H φ e₁ hf).andThen (fun H₁ v₁ _ => OpRes.toRes_live _)
    | intCast w sg e₁ =>
        simp only [eval]
        exact (ih H φ e₁ hf).andThen (fun H₁ v₁ _ => OpRes.toRes_live _)
    | fintrin k e₁ =>
        simp only [eval]
        exact (ih H φ e₁ hf).andThen (fun H₁ v₁ _ => OpRes.toRes_live _)
    | panic msg => simp only [eval]; trivial
    | dbg e₁ =>
        simp only [eval]
        refine (ih H φ e₁ hf).andThen (fun H₁ v₁ _ => ?_)
        split
        · exact Grow.refl H₁
        · simp [LivePost]
    | mkStruct s args =>
        simp only [eval]
        have ka := hargs H args hf
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          refine LivePost.withTrace (LivePost.lift ka ?_) tr
          split
          · simp [LivePost]
          · split
            · exact introVal_live _
            · simp [LivePost]
    | mkEnum e k args =>
        simp only [eval]
        have ka := hargs H args hf
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          refine LivePost.withTrace (LivePost.lift ka ?_) tr
          split
          · simp [LivePost]
          · split
            · simp [LivePost]
            · split
              · exact introVal_live _
              · simp [LivePost]
    | «match» scrut arms =>
        simp only [eval]
        refine (ih H φ scrut hf).andThen (fun H₀ v hg₀ => ?_)
        have hf₀ := hf.grow hg₀
        cases v with
        | enum e k i vs =>
          dsimp only
          split
          · simp [LivePost]
          · rename_i body harm
            obtain ⟨hgm, hndm, hlm⟩ := mintParams_live H₀ vs
            refine LivePost.withTrace ?_ _
            refine LivePost.scoped
              (φ' := { env := (mintParams H₀ vs).2.reverse ++ φ.env,
                       scope := φ.scope ++ (mintParams H₀ vs).2 }) hgm hlm hndm rfl
              (ih _ _ body ⟨?_, ?_, ?_⟩) (fun H₂ v₂ hg₂ => ?_)
            · intro ℓ hℓ
              rcases List.mem_append.mp hℓ with hm | hm
              · exact (hlm ℓ (List.mem_reverse.mp hm)).2
              · exact hgm.2 ℓ (hf₀.1 ℓ hm)
            · intro ℓ hℓ
              rcases List.mem_append.mp hℓ with hm | hm
              · exact hgm.2 ℓ (hf₀.2.1 ℓ hm)
              · exact (hlm ℓ hm).2
            · refine List.nodup_append.mpr ⟨hf₀.2.2, hndm, fun a ha b hb => ?_⟩
              exact (hf₀.2.1 a ha).ne_of_le (hlm b hb).1
            · have hu := unwindLocs_live (D := P.decls) (H := H₂)
                (nodup_reverse hndm)
                (fun ℓ hℓ => hg₂.2 ℓ (hlm ℓ (List.mem_reverse.mp hℓ)).2)
              split
              · rename_i w hw; rw [hw] at hu; exact hu
              · rename_i H₃ evs hw
                rw [hw] at hu
                obtain ⟨hlen, hkeep⟩ := hu.grow
                refine ⟨Nat.le_trans hgm.1 (Nat.le_trans hg₂.1 hlen), fun ℓ hl => ?_⟩
                refine hkeep ℓ (fun hm => ?_) (hg₂.2 ℓ (hgm.2 ℓ hl))
                exact hl.ne_of_le (hlm ℓ (List.mem_reverse.mp hm)).1 rfl
        | _ => simp [LivePost]
    | mkArray T args =>
        simp only [eval]
        have ka := hargs H args hf
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          exact LivePost.withTrace (LivePost.lift ka (introVal_live _)) tr
    | repeatArray T e₁ m =>
        simp only [eval]
        refine (ih H φ e₁ hf).andThen (fun H₁ v₁ _ => ?_)
        split
        · exact introVal_live _
        · simp [LivePost]
    | indexRead p idx πs =>
        simp only [eval]
        have ka := hargs H idx hf
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          refine LivePost.withTrace (LivePost.lift ka ?_) tr
          split
          · exact dynPlace_ne_uad (hf.grow ka).1 (by assumption)
          · trivial
          · split
            · exact Contents.readAt_ne_uad _ (by assumption)
            · split
              · simp [LivePost]
              · split
                · exact Grow.refl H₁
                · simp [LivePost]
    | indexWrite p idx πs e₁ =>
        simp only [eval]
        refine (ih H φ e₁ hf).andThen (fun H₁ v hg₁ => ?_)
        have ka := hargs H₁ idx (hf.grow hg₁)
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₂ vs tr hra
          rw [hra] at ka
          refine LivePost.withTrace (LivePost.lift ka ?_) tr
          split
          · exact dynPlace_ne_uad ((hf.grow hg₁).grow ka).1 (by assumption)
          · trivial
          · rename_i ℓ c sub ρ hdp
            split
            · exact Contents.readAt_ne_uad _ (by assumption)
            · split
              · simp [LivePost]
              · split
                · exact dropCell_ne_uad (by assumption)
                · split
                  · simp [LivePost]
                  · split
                    · simp [LivePost]
                    · split
                      · exact Grow.set_full H₂ ℓ _
                      · simp [LivePost]
    | indexDrop p idx πs =>
        simp only [eval]
        exact (ih H φ _ hf).andThen (fun H₁ v₁ _ => Grow.refl H₁)
    | drop p =>
        simp only [eval]
        split
        · simp [LivePost]
        · rename_i ℓ hρ
          split
          · simp [LivePost]
          · rename_i hd; exact absurd hd (hf.root hρ).ne_dead
          · split
            · split
              · exact Contents.readAt_ne_uad _ (by assumption)
              · split
                · exact Contents.destructure_ne_uad (by assumption)
                · split
                  · simp [LivePost]
                  · split
                    · exact dropCell_ne_uad (by assumption)
                    · split
                      · simp [LivePost]
                      · exact Grow.set_full H ℓ _
            · split
              · exact Contents.readAt_ne_uad _ (by assumption)
              · split
                · simp [LivePost]
                · split
                  · exact dropCell_ne_uad (by assumption)
                  · split
                    · exact Grow.refl H
                    · split
                      · simp [LivePost]
                      · exact Grow.set_full H ℓ _
    | letIn m e₁ e₂ =>
        simp only [eval]
        refine (ih H φ e₁ hf).andThen (fun H₁ v₁ hg₁ => ?_)
        have hf₁ := hf.grow hg₁
        have hg₁' := Grow.append H₁ [Cell.full (Contents.ofVal v₁)]
        have hnew : Live (H₁ ++ [Cell.full (Contents.ofVal v₁)]) H₁.length :=
          ⟨Contents.ofVal v₁, by simp⟩
        refine LivePost.scoped (ys := [H₁.length])
          (φ' := { env := H₁.length :: φ.env, scope := φ.scope ++ [H₁.length] }) hg₁'
          (fun ℓ hℓ => by
            rw [List.mem_singleton.mp hℓ]; exact ⟨Nat.le_refl _, hnew⟩)
          (List.nodup_cons.mpr ⟨List.not_mem_nil, List.nodup_nil⟩) rfl
          (ih _ _ e₂ ⟨?_, ?_, ?_⟩) (fun H₂ v₂ hg₂ => ?_)
        · intro ℓ hℓ
          rcases List.mem_cons.mp hℓ with he | hm
          · rw [he]; exact hnew
          · exact hg₁'.2 ℓ (hf₁.1 ℓ hm)
        · intro ℓ hℓ
          rcases List.mem_append.mp hℓ with hm | hm
          · exact hg₁'.2 ℓ (hf₁.2.1 ℓ hm)
          · rw [List.mem_singleton.mp hm]; exact hnew
        · refine List.nodup_append.mpr ⟨hf₁.2.2, List.nodup_cons.mpr
            ⟨List.not_mem_nil, List.nodup_nil⟩, fun a ha b hb => ?_⟩
          rw [List.mem_singleton.mp hb]
          exact (hf₁.2.1 a ha).ne_of_le (Nat.le_refl _)
        · have hl₂ := hg₂.2 _ hnew
          obtain ⟨herr, hok⟩ := dropRetire_live (D := P.decls) hl₂
          split
          · rename_i w hw; exact herr w hw
          · rename_i H₃ evs hw
            have hH₃ := hok H₃ evs hw
            refine ⟨?_, fun ℓ hl => ?_⟩
            · rw [hH₃, List.length_set]; exact Nat.le_trans (by simp) hg₂.1
            · rw [hH₃, Live, List.getElem?_set_ne (hl.ne_of_le (Nat.le_refl _)).symm]
              exact hg₂.2 ℓ (hg₁'.2 ℓ hl)
    | assign p e₁ =>
        simp only [eval]
        refine (ih H φ e₁ hf).andThen (fun H₁ v hg₁ => ?_)
        have hf₁ := hf.grow hg₁
        split
        · simp [LivePost]
        · rename_i ℓ hρ
          split
          · simp [LivePost]
          · rename_i hd; exact absurd hd (hf₁.root hρ).ne_dead
          · split
            · exact Contents.readAt_ne_uad _ (by assumption)
            · split
              · simp [LivePost]
              · split
                · exact dropCell_ne_uad (by assumption)
                · split
                  · simp [LivePost]
                  · split
                    · exact Grow.set_full H₁ ℓ _
                    · simp [LivePost]
    | seq e₁ e₂ =>
        simp only [eval]
        refine (ih H φ e₁ hf).andThen (fun H₁ v₁ hg₁ => ?_)
        split
        · simp [LivePost]
        · split
          · exact dropContents_ne_uad (by assumption)
          · exact (ih H₁ φ e₂ (hf.grow hg₁)).withTrace _
        · exact ih H₁ φ e₂ (hf.grow hg₁)
    | ite c e₁ e₂ =>
        simp only [eval]
        refine (ih H φ c hf).andThen (fun H₀ v₀ hg₀ => ?_)
        split
        · split
          · exact ih H₀ φ e₁ (hf.grow hg₀)
          · exact ih H₀ φ e₂ (hf.grow hg₀)
        · simp [LivePost]
    | call f args =>
        simp only [eval]
        have ka := hargs H args hf
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          refine LivePost.withTrace (LivePost.lift ka ?_) tr
          split
          · simp [LivePost]
          · rename_i fd hfd
            split
            · obtain ⟨hgm, hndm, hlm⟩ := mintParams_live H₁ vs
              have hb := ih _ { env := (mintParams H₁ vs).2.reverse, scope := (mintParams H₁ vs).2 }
                fd.body ⟨fun ℓ hℓ => (hlm ℓ (List.mem_reverse.mp hℓ)).2,
                  fun ℓ hℓ => (hlm ℓ hℓ).2, hndm⟩
              revert hb
              generalize eval M n P (mintParams H₁ vs).1
                { env := (mintParams H₁ vs).2.reverse, scope := (mintParams H₁ vs).2 } fd.body = r
              intro hb
              cases r with
              | ok H₃ v tr₃ =>
                  simp only [EvalRes.absorb]
                  refine LivePost.withTrace ?_ tr₃
                  simp only [runAllScopeDrops]
                  have hu := unwindLocs_live (D := P.decls) (H := H₃)
                    (nodup_reverse hndm)
                    (fun ℓ hℓ => hb.2 ℓ (hlm ℓ (List.mem_reverse.mp hℓ)).2)
                  split
                  · rename_i w hw; rw [hw] at hu; exact hu
                  · rename_i H₄ evs hw
                    rw [hw] at hu
                    obtain ⟨hlen, hkeep⟩ := hu.grow
                    refine ⟨Nat.le_trans hgm.1 (Nat.le_trans hb.1 hlen), fun ℓ hl => ?_⟩
                    refine hkeep ℓ (fun hm => ?_) (hb.2 ℓ (hgm.2 ℓ hl))
                    exact hl.ne_of_le (hlm ℓ (List.mem_reverse.mp hm)).1 rfl
              | returned H₃ v tr₃ =>
                  simp only [EvalRes.absorb]
                  refine ⟨Nat.le_trans hgm.1 hb.1, fun ℓ hl => ?_⟩
                  exact hb.2 ℓ (fun hm => hl.ne_of_le (hlm ℓ hm).1 rfl) (hgm.2 ℓ hl)
              | broke H₃ sc tr₃ => simp [EvalRes.absorb, LivePost]
              | panic k tr₃ => trivial
              | stuck w => exact hb
              | outOfFuel => trivial
            · simp [LivePost]
    | ret e₁ =>
        simp only [eval]
        refine (ih H φ e₁ hf).andThen (fun H₁ v hg₁ => ?_)
        have hf₁ := hf.grow hg₁
        simp only [runAllScopeDrops]
        have hu := unwindLocs_live (D := P.decls) (H := H₁) (nodup_reverse hf₁.2.2)
          (fun ℓ hℓ => hf₁.2.1 ℓ (List.mem_reverse.mp hℓ))
        split
        · rename_i w hw; rw [hw] at hu; exact hu
        · rename_i H₂ evs hw
          rw [hw] at hu
          obtain ⟨hlen, hkeep⟩ := hu.grow
          exact ⟨hlen, fun ℓ hn hl => hkeep ℓ (fun hm => hn (List.mem_reverse.mp hm)) hl⟩
    | loop e₁ =>
        simp only [eval]
        have hb := ih H φ e₁ hf
        split
        · rename_i H₁ tr hr
          rw [hr] at hb
          exact ((ih H₁ φ (.loop e₁) (hf.grow hb)).lift hb).withTrace tr
        · simp [LivePost]
        · rename_i H₁ sc tr hr
          rw [hr] at hb
          obtain ⟨hg, xs, hsc, hnd, hxs⟩ := hb
          have hdrop : sc.drop φ.scope.length = xs := by rw [hsc, List.drop_left]
          rw [hdrop]
          have hu := unwindLocs_live (D := P.decls) (H := H₁) (nodup_reverse hnd)
            (fun ℓ hℓ => (hxs ℓ (List.mem_reverse.mp hℓ)).2)
          split
          · rename_i w hw; rw [hw] at hu; exact hu
          · rename_i H₂ evs hw
            rw [hw] at hu
            obtain ⟨hlen, hkeep⟩ := hu.grow
            refine ⟨Nat.le_trans hg.1 hlen, fun ℓ hl => ?_⟩
            refine hkeep ℓ (fun hm => ?_) (hg.2 ℓ hl)
            exact hl.ne_of_le (hxs ℓ (List.mem_reverse.mp hm)).1 rfl
        · exact hb
    | brk =>
        simp only [eval]
        exact ⟨Grow.refl H, [], (List.append_nil _).symm, List.nodup_nil,
          fun _ h => absurd h List.not_mem_nil⟩

/-! ## The invariant over §6's relation -/

/-- The plain `drop-retire` of a live cell never meets `†`, and retires
exactly that cell (helper). -/
theorem plainDropRetire_live {D : Decls} {H : Store} {ℓ : Nat} (hl : Live H ℓ) :
    (∀ w, plainDropRetire D H ℓ = .error w → w ≠ .useAfterDrop) ∧
      ∀ H' evs, plainDropRetire D H ℓ = .ok (H', evs) → H' = H.set ℓ .dead := by
  obtain ⟨c, hc⟩ := hl
  unfold plainDropRetire
  rw [hc]
  refine ⟨fun w h => ?_, fun H' evs h => ?_⟩
  · simp only [] at h
    split at h
    · cases h; exact dropCell_ne_uad (by assumption)
    · cases h
  · simp only [] at h
    split at h
    · cases h
    · cases h; rfl

/-- The plain `run-scope-drops` over distinct live cells keeps `UnwindPost`
(helper). -/
theorem plainUnwind_live {D : Decls} : ∀ {H : Store} {ls : List Nat}, ls.Nodup →
    (∀ ℓ ∈ ls, Live H ℓ) → UnwindPost H ls (plainUnwind D H ls)
  | H, [], _, _ => ⟨rfl, fun _ _ => rfl⟩
  | H, ℓ :: rest, hnd, hl => by
      obtain ⟨hnotin, hnd'⟩ := List.nodup_cons.mp hnd
      have hℓ := hl ℓ (List.mem_cons_self ..)
      obtain ⟨herr, hok⟩ := plainDropRetire_live (D := D) hℓ
      simp only [plainUnwind]
      split
      · rename_i w hw; exact herr w hw
      · rename_i H₁ evs hr
        have hH₁ := hok H₁ evs hr
        have hl₁ : ∀ m ∈ rest, Live H₁ m := by
          intro m hm
          have hne : ℓ ≠ m := fun he => hnotin (he ▸ hm)
          rw [hH₁, Live, List.getElem?_set_ne hne]
          exact hl m (List.mem_cons_of_mem _ hm)
        have ih := plainUnwind_live (D := D) hnd' hl₁
        split
        · rename_i w hw; rw [hw] at ih; exact ih
        · rename_i H₂ evs' hr'
          rw [hr'] at ih
          obtain ⟨hlen, heq⟩ := ih
          refine ⟨by rw [hlen, hH₁, List.length_set], fun m hm => ?_⟩
          have hne : ℓ ≠ m := fun he => hm (he ▸ List.mem_cons_self ..)
          rw [heq m (fun h' => hm (List.mem_cons_of_mem _ h')), hH₁, List.getElem?_set_ne hne]

/-- The plain residue walk never refuses with `useAfterDrop` (helper). -/
theorem plainResidue_ne_uad {D : Decls} {ℓ : Nat} : ∀ {rs : List Contents} {w : Violation},
    plainResidue D ℓ rs = .error w → w ≠ .useAfterDrop
  | [], _, h => by simp [plainResidue] at h
  | r :: rs, _, h => by
      simp only [plainResidue] at h
      split at h
      · cases h; exact dropContents_ne_uad (by assumption)
      · split at h
        · cases h; exact plainResidue_ne_uad (by assumption)
        · cases h

/-- The plain destructure never refuses with `useAfterDrop` (helper). -/
theorem plainDestructure_ne_uad {D : Decls} {ℓ : Nat} {c : Contents} {πs : List Nat}
    {w : Violation} (h : plainDestructure D ℓ c πs = .error w) : w ≠ .useAfterDrop := by
  unfold plainDestructure at h
  split at h
  · cases h; exact Contents.splitResidue_ne_uad (by assumption)
  · split at h
    · cases h; exact plainResidue_ne_uad (by assumption)
    · cases h

/-- **The stack's shape**: what each frame of the control stack says about the
frame in force above it. An `endscope ℓ̄` marker and a loop boundary sit
under a frame that extends theirs by cells at the end of its scope record
(and the front of its environment); a call boundary and the stack's bottom
sit under a frame whose environment is its scope record reversed; every other
frame is an evaluation context of the same frame (helper). -/
def Shape : Frame → List Kont → Prop
  | φ, [] => φ.env = φ.scope.reverse
  | φ, .endscope ℓs :: K => ∃ φ₀ : Frame,
      φ = { env := ℓs.reverse ++ φ₀.env, scope := φ₀.scope ++ ℓs } ∧ Shape φ₀ K
  | φ, .loop _ φs :: K => ∃ xs : List Nat,
      φ = { env := xs.reverse ++ φs.env, scope := φs.scope ++ xs } ∧ Shape φs K
  | φ, .call φs :: K => φ.env = φ.scope.reverse ∧ Shape φs K
  | φ, .binopL _ _ :: K | φ, .binopR _ _ :: K | φ, .unop _ :: K | φ, .intCast _ _ :: K
  | φ, .fintrin _ :: K | φ, .dbg :: K | φ, .args _ _ _ :: K | φ, .repeatArray _ _ :: K
  | φ, .indexWriteRhs _ _ _ :: K | φ, .«match» _ :: K | φ, .letIn _ :: K | φ, .seq _ :: K
  | φ, .ite _ _ :: K | φ, .assign _ :: K | φ, .ret :: K => Shape φ K

/-- The cells the suspended callers' frames owe a drop (helper). -/
def callerCells : List Kont → List Nat
  | [] => []
  | .call φs :: K => callerCells K ++ φs.scope
  | _ :: K => callerCells K

/-- **The configuration invariant**: the stack has its shape, and every cell a
frame on it owes a drop — the frame in force and every suspended caller — is
live and owed once (helper). -/
def StackLive (H : Store) (φ : Frame) (K : List Kont) : Prop :=
  Shape φ K ∧ (callerCells K ++ φ.scope).Nodup ∧ ∀ ℓ ∈ callerCells K ++ φ.scope, Live H ℓ

/-- The invariant at a configuration; a trap `↯κ` has no store left to check
(helper). -/
def ConfigLive : Config → Prop
  | .run H φ K _ _ => StackLive H φ K
  | .panic _ _ => True

/-- Every frame on a well-shaped stack has its scope record, reversed, as its
environment (helper). -/
theorem Shape.env : ∀ {φ : Frame} {K : List Kont}, Shape φ K → φ.env = φ.scope.reverse
  | φ, [], h => h
  | φ, .endscope ℓs :: K, h => by
      obtain ⟨φ₀, rfl, h₀⟩ := h
      simp [Shape.env h₀]
  | φ, .loop _ φs :: K, h => by
      obtain ⟨xs, rfl, h₀⟩ := h
      simp [Shape.env h₀]
  | φ, .call φs :: K, h => h.1
  | φ, .binopL _ _ :: K, h | φ, .binopR _ _ :: K, h | φ, .unop _ :: K, h
  | φ, .intCast _ _ :: K, h | φ, .fintrin _ :: K, h | φ, .dbg :: K, h
  | φ, .args _ _ _ :: K, h | φ, .repeatArray _ _ :: K, h | φ, .indexWriteRhs _ _ _ :: K, h
  | φ, .«match» _ :: K, h | φ, .letIn _ :: K, h | φ, .seq _ :: K, h | φ, .ite _ _ :: K, h
  | φ, .assign _ :: K, h | φ, .ret :: K, h => Shape.env (K := K) h

/-- (D-Return)'s search: the caller's frame is well shaped, and the cells the
suspended callers owe are its own and those below it (helper). -/
theorem Shape.toCall : ∀ {φ : Frame} {K : List Kont} {φs : Frame} {K' : List Kont},
    Shape φ K → Kont.toCall K = some (φs, K') →
      Shape φs K' ∧ callerCells K = callerCells K' ++ φs.scope
  | _, [], _, _, _, h => by simp [Kont.toCall] at h
  | _, .call φc :: K, _, _, hs, h => by
      simp only [Kont.toCall, Option.some.injEq, Prod.mk.injEq] at h
      obtain ⟨rfl, rfl⟩ := h
      exact ⟨hs.2, rfl⟩
  | _, .endscope ℓs :: K, _, _, hs, h => by
      obtain ⟨φ₀, _, h₀⟩ := hs
      exact Shape.toCall (K := K) h₀ h
  | _, .loop _ φl :: K, _, _, hs, h => by
      obtain ⟨xs, _, h₀⟩ := hs
      exact Shape.toCall (K := K) h₀ h
  | _, .binopL _ _ :: K, _, _, hs, h | _, .binopR _ _ :: K, _, _, hs, h
  | _, .unop _ :: K, _, _, hs, h | _, .intCast _ _ :: K, _, _, hs, h
  | _, .fintrin _ :: K, _, _, hs, h | _, .dbg :: K, _, _, hs, h
  | _, .args _ _ _ :: K, _, _, hs, h | _, .repeatArray _ _ :: K, _, _, hs, h
  | _, .indexWriteRhs _ _ _ :: K, _, _, hs, h | _, .«match» _ :: K, _, _, hs, h
  | _, .letIn _ :: K, _, _, hs, h | _, .seq _ :: K, _, _, hs, h
  | _, .ite _ _ :: K, _, _, hs, h | _, .assign _ :: K, _, _, hs, h
  | _, .ret :: K, _, _, hs, h => Shape.toCall (K := K) hs h

/-- (D-Break)'s search: the loop's frame is well shaped, no caller is crossed, and
the frame in force extends the loop's at the end of its scope record (helper). -/
theorem Shape.toLoop : ∀ {φ : Frame} {K : List Kont} {φs : Frame} {K' : List Kont},
    Shape φ K → Kont.toLoop K = some (φs, K') →
      Shape φs K' ∧ callerCells K = callerCells K' ∧ ∃ xs, φ.scope = φs.scope ++ xs
  | _, [], _, _, _, h => by simp [Kont.toLoop] at h
  | _, .call φc :: K, _, _, hs, h => by simp [Kont.toLoop] at h
  | _, .loop _ φl :: K, _, _, hs, h => by
      simp only [Kont.toLoop, Option.some.injEq, Prod.mk.injEq] at h
      obtain ⟨rfl, rfl⟩ := h
      obtain ⟨xs, rfl, h₀⟩ := hs
      exact ⟨h₀, rfl, xs, rfl⟩
  | _, .endscope ℓs :: K, _, _, hs, h => by
      obtain ⟨φ₀, rfl, h₀⟩ := hs
      obtain ⟨h₁, h₂, xs, h₃⟩ := Shape.toLoop h₀ h
      exact ⟨h₁, h₂, xs ++ ℓs, by simp [h₃]⟩
  | _, .binopL _ _ :: K, _, _, hs, h | _, .binopR _ _ :: K, _, _, hs, h
  | _, .unop _ :: K, _, _, hs, h | _, .intCast _ _ :: K, _, _, hs, h
  | _, .fintrin _ :: K, _, _, hs, h | _, .dbg :: K, _, _, hs, h
  | _, .args _ _ _ :: K, _, _, hs, h | _, .repeatArray _ _ :: K, _, _, hs, h
  | _, .indexWriteRhs _ _ _ :: K, _, _, hs, h | _, .«match» _ :: K, _, _, hs, h
  | _, .letIn _ :: K, _, _, hs, h | _, .seq _ :: K, _, _, hs, h
  | _, .ite _ _ :: K, _, _, hs, h | _, .assign _ :: K, _, _, hs, h
  | _, .ret :: K, _, _, hs, h => Shape.toLoop (K := K) hs h

/-- A cell the environment names is live (helper). -/
theorem StackLive.env {H : Store} {φ : Frame} {K : List Kont} (h : StackLive H φ K) :
    ∀ ℓ ∈ φ.env, Live H ℓ := by
  intro ℓ hℓ
  rw [h.1.env] at hℓ
  exact h.2.2 ℓ (List.mem_append_right _ (List.mem_reverse.mp hℓ))

/-- The configuration invariant survives growth (helper). -/
theorem StackLive.grow {H H' : Store} {φ : Frame} {K : List Kont} (h : StackLive H φ K)
    (hg : Grow H H') : StackLive H' φ K :=
  ⟨h.1, h.2.1, fun ℓ hℓ => hg.2 ℓ (h.2.2 ℓ hℓ)⟩

/-- Looking a place's root up in a frame whose environment names live cells never
refuses with `useAfterDrop` (helper). -/
theorem rootCell_ne_uad {H : Store} {φ : Frame} {i : Nat} {w : Violation}
    (hφ : ∀ ℓ ∈ φ.env, Live H ℓ) (h : rootCell H φ i = .error w) : w ≠ .useAfterDrop := by
  unfold rootCell at h
  split at h
  · cases h; simp
  · rename_i ℓ hρ
    split at h
    · cases h; simp
    · rename_i hd; exact absurd hd (hφ ℓ (List.mem_of_getElem? hρ)).ne_dead
    · cases h

/-- What a step keeps (helper). -/
def StepLive : StepOut → Prop
  | .next C => ConfigLive C
  | .halted => True
  | .stuck w => w ≠ .useAfterDrop

/-- Retiring the cells at the end of the owed list keeps the rest live and
owed once (helper). -/
theorem unwind_keeps {D : Decls} {H H' : Store} {A xs : List Nat} {evs : List Event}
    (hnd : (A ++ xs).Nodup) (hl : ∀ ℓ ∈ A ++ xs, Live H ℓ)
    (hu : plainUnwind D H xs.reverse = .ok (H', evs)) :
    A.Nodup ∧ ∀ ℓ ∈ A, Live H' ℓ := by
  obtain ⟨hA, hxs, hdis⟩ := List.nodup_append.mp hnd
  have hp := plainUnwind_live (D := D) (H := H) (nodup_reverse hxs)
    (fun ℓ hℓ => hl ℓ (List.mem_append_right _ (List.mem_reverse.mp hℓ)))
  rw [hu] at hp
  obtain ⟨_, hkeep⟩ := hp.grow
  refine ⟨hA, fun ℓ hℓ => hkeep ℓ (fun hm => ?_) (hl ℓ (List.mem_append_left _ hℓ))⟩
  exact hdis ℓ hℓ ℓ (List.mem_reverse.mp hm) rfl

/-- The same teardown never refuses with `useAfterDrop` (helper). -/
theorem unwind_err {D : Decls} {H : Store} {A xs : List Nat} {w : Violation}
    (hnd : (A ++ xs).Nodup) (hl : ∀ ℓ ∈ A ++ xs, Live H ℓ)
    (hu : plainUnwind D H xs.reverse = .error w) : w ≠ .useAfterDrop := by
  obtain ⟨_, hxs, _⟩ := List.nodup_append.mp hnd
  have hp := plainUnwind_live (D := D) (H := H) (nodup_reverse hxs)
    (fun ℓ hℓ => hl ℓ (List.mem_append_right _ (List.mem_reverse.mp hℓ)))
  rw [hu] at hp
  exact hp

/-- Fresh cells appended to the owed list (helper). -/
theorem extend_keeps {H H' : Store} {A ls : List Nat} (hnd : A.Nodup)
    (hl : ∀ ℓ ∈ A, Live H ℓ) (hg : Grow H H') (hls : ls.Nodup)
    (hfresh : ∀ ℓ ∈ ls, H.length ≤ ℓ ∧ Live H' ℓ) :
    (A ++ ls).Nodup ∧ ∀ ℓ ∈ A ++ ls, Live H' ℓ := by
  refine ⟨List.nodup_append.mpr ⟨hnd, hls, fun a ha b hb => ?_⟩, fun ℓ hℓ => ?_⟩
  · exact (hl a ha).ne_of_le (hfresh b hb).1
  · rcases List.mem_append.mp hℓ with hm | hm
    · exact hg.2 ℓ (hl ℓ hm)
    · exact (hfresh ℓ hm).2

/-- (D-EndScope)'s pop undoes the extension (D-Let) and (D-Match) made (helper). -/
theorem Frame.popScope_ext (φ₀ : Frame) (ls : List Nat) :
    ({ env := ls.reverse ++ φ₀.env, scope := φ₀.scope ++ ls } : Frame).popScope ls.length
      = φ₀ := by
  cases φ₀
  simp [Frame.popScope]

/-- An operator's step touches no cell (helper). -/
theorem OpRes.toStep_live {H : Store} {φ : Frame} {K : List Kont} {tr : List Event}
    (h : StackLive H φ K) (o : OpRes) : StepLive (o.toStep H φ K tr) := by
  cases o with
  | val v => exact h
  | trap k => trivial
  | confused => simp [OpRes.toStep, StepLive]

/-- `step` at an expression keeps the invariant (helper). -/
theorem stepEval_live (M : FloatOps) (P : Program) {H : Store} {φ : Frame} {K : List Kont}
    {tr : List Event} (h : StackLive H φ K) (e : Expr) : StepLive (stepEval M P H φ K tr e) := by
  have henv := h.env
  cases e with
  | intLit w sg m => exact h
  | floatLit w l => exact h
  | boolLit b => exact h
  | unitLit => exact h
  | use p =>
      simp only [stepEval]
      split
      · exact rootCell_ne_uad henv (by assumption)
      · rename_i ℓ c _
        split
        · split
          · exact Contents.readAt_ne_uad _ (by assumption)
          · split
            · exact plainDestructure_ne_uad (by assumption)
            · split
              · simp [StepLive]
              · split
                · simp [StepLive]
                · exact h.grow (Grow.set_full H ℓ _)
        · split
          · exact Contents.readAt_ne_uad _ (by assumption)
          · split
            · simp [StepLive]
            · split
              · exact h
              · split
                · simp [StepLive]
                · exact h.grow (Grow.set_full H ℓ _)
  | binop op e₁ e₂ => exact h
  | unop op e₁ => exact h
  | intCast w sg e₁ => exact h
  | fintrin k e₁ => exact h
  | panic msg => trivial
  | dbg e₁ => exact h
  | mkStruct s args => exact h
  | mkEnum e k args => exact h
  | «match» scrut arms => exact h
  | mkArray T args => exact h
  | repeatArray T e₁ m => exact h
  | indexRead p idx πs => exact h
  | indexWrite p idx πs e₁ => exact h
  | indexDrop p idx πs => exact h
  | drop p =>
      simp only [stepEval]
      split
      · exact rootCell_ne_uad henv (by assumption)
      · rename_i ℓ c _
        split
        · split
          · exact Contents.readAt_ne_uad _ (by assumption)
          · split
            · exact plainDestructure_ne_uad (by assumption)
            · split
              · exact dropCell_ne_uad (by assumption)
              · split
                · simp [StepLive]
                · exact h.grow (Grow.set_full H ℓ _)
        · split
          · exact Contents.readAt_ne_uad _ (by assumption)
          · split
            · exact h
            · split
              · exact dropCell_ne_uad (by assumption)
              · split
                · simp [StepLive]
                · exact h.grow (Grow.set_full H ℓ _)
  | letIn m e₁ e₂ => exact h
  | assign p e₁ => exact h
  | seq e₁ e₂ => exact h
  | ite c e₁ e₂ => exact h
  | call f args => exact h
  | ret e₁ => exact h
  | loop e₁ =>
      refine ⟨⟨[], ?_, h.1⟩, h.2.1, h.2.2⟩
      cases φ; simp
  | brk =>
      simp only [stepEval]
      split
      · simp [StepLive]
      · rename_i φs K' hl
        obtain ⟨hs, hc, xs, hxs⟩ := h.1.toLoop hl
        have hnd : ((callerCells K' ++ φs.scope) ++ xs).Nodup := by
          rw [List.append_assoc, ← hxs, ← hc]; exact h.2.1
        have hlv : ∀ ℓ ∈ (callerCells K' ++ φs.scope) ++ xs, Live H ℓ := by
          rw [List.append_assoc, ← hxs, ← hc]; exact h.2.2
        have hd : φ.scope.drop φs.scope.length = xs := by rw [hxs, List.drop_left]
        rw [hd]
        split
        · exact unwind_err hnd hlv (by assumption)
        · rename_i H' evs hu
          obtain ⟨h₁, h₂⟩ := unwind_keeps hnd hlv hu
          exact ⟨hs, h₁, h₂⟩

/-- `step` at a completed argument list keeps the invariant: (D-Call) mints the
callee's cells fresh (helper). -/
theorem stepArgs_live (P : Program) {H : Store} {φ : Frame} {K : List Kont}
    {tr : List Event} (h : StackLive H φ K) (vs : List Val) (t : ArgsTag) :
    StepLive (stepArgs P H φ K tr vs t) := by
  have henv := h.env
  cases t with
  | struct s =>
      simp only [stepArgs]
      split
      · simp [StepLive]
      · split
        · exact h.grow (Grow.append H _)
        · simp [StepLive]
  | enum e k =>
      simp only [stepArgs]
      split
      · simp [StepLive]
      · split
        · simp [StepLive]
        · split
          · exact h.grow (Grow.append H _)
          · simp [StepLive]
  | array T => exact h.grow (Grow.append H _)
  | call f =>
      simp only [stepArgs]
      split
      · simp [StepLive]
      · split
        · obtain ⟨hgm, hndm, hlm⟩ := mintParams_live H vs
          revert hgm hndm hlm
          generalize mintParams H vs = m
          obtain ⟨H', ls⟩ := m
          intro hgm hndm hlm
          obtain ⟨hnd, hlv⟩ := extend_keeps h.2.1 h.2.2 hgm hndm hlm
          exact ⟨⟨by simp, h.1⟩, hnd, hlv⟩
        · simp [StepLive]
  | indexRead p πs =>
      simp only [stepArgs]
      split
      · exact dynPlace_ne_uad henv (by assumption)
      · trivial
      · split
        · exact Contents.readAt_ne_uad _ (by assumption)
        · split
          · simp [StepLive]
          · split
            · exact h
            · simp [StepLive]
  | indexDrop p πs =>
      simp only [stepArgs]
      split
      · exact dynPlace_ne_uad henv (by assumption)
      · trivial
      · split
        · exact Contents.readAt_ne_uad _ (by assumption)
        · split
          · simp [StepLive]
          · split
            · exact h
            · simp [StepLive]
  | indexWrite p πs v =>
      simp only [stepArgs]
      split
      · exact dynPlace_ne_uad henv (by assumption)
      · trivial
      · rename_i ℓ c sub ρ _
        split
        · exact Contents.readAt_ne_uad _ (by assumption)
        · split
          · exact dropCell_ne_uad (by assumption)
          · split
            · simp [StepLive]
            · split
              · simp [StepLive]
              · exact h.grow (Grow.set_full H ℓ _)

/-- `step` at a value returning into the top frame keeps the invariant: every
teardown walks cells the invariant says are live and owed once (helper). -/
theorem stepRet_live (M : FloatOps) (P : Program) {H : Store} {φ : Frame} {K : List Kont}
    {tr : List Event} (v : Val) (k : Kont) (h : StackLive H φ (k :: K)) :
    StepLive (stepRet M P H φ K tr v k) := by
  cases k with
  | binopL op e₂ => exact h
  | binopR op v₁ => exact OpRes.toStep_live (K := K) h _
  | unop op => exact OpRes.toStep_live (K := K) h _
  | intCast w sg => exact OpRes.toStep_live (K := K) h _
  | fintrin k => exact OpRes.toStep_live (K := K) h _
  | dbg =>
      simp only [stepRet]
      split
      · exact h
      · simp [StepLive]
  | args t vs es => exact h
  | repeatArray T n =>
      simp only [stepRet]
      split
      · exact StackLive.grow (K := K) h (Grow.append H _)
      · simp [StepLive]
  | indexWriteRhs p idx πs => exact h
  | «match» arms =>
      simp only [stepRet]
      split
      · rename_i e k i vs
        split
        · simp [StepLive]
        · obtain ⟨hgm, hndm, hlm⟩ := mintParams_live H vs
          revert hgm hndm hlm
          generalize mintParams H vs = m
          obtain ⟨H', ls⟩ := m
          intro hgm hndm hlm
          have hK : StackLive H φ K := h
          obtain ⟨hnd, hlv⟩ := extend_keeps hK.2.1 hK.2.2 hgm hndm hlm
          refine ⟨⟨φ, rfl, hK.1⟩, ?_, ?_⟩
          · show (callerCells K ++ (φ.scope ++ ls)).Nodup
            rw [← List.append_assoc]; exact hnd
          · show ∀ ℓ ∈ callerCells K ++ (φ.scope ++ ls), Live H' ℓ
            rw [← List.append_assoc]; exact hlv
      · simp [StepLive]
  | letIn e₂ =>
      have hK : StackLive H φ K := h
      have hnew : Live (H ++ [Cell.full (Contents.ofVal v)]) H.length :=
        ⟨Contents.ofVal v, by simp⟩
      obtain ⟨hnd, hlv⟩ := extend_keeps (ls := [H.length]) hK.2.1 hK.2.2 (Grow.append H _)
        (List.nodup_cons.mpr ⟨List.not_mem_nil, List.nodup_nil⟩)
        (fun ℓ hℓ => by rw [List.mem_singleton.mp hℓ]; exact ⟨Nat.le_refl _, hnew⟩)
      refine ⟨⟨φ, rfl, hK.1⟩, ?_, ?_⟩
      · show (callerCells K ++ (φ.scope ++ [H.length])).Nodup
        rw [← List.append_assoc]; exact hnd
      · show ∀ ℓ ∈ callerCells K ++ (φ.scope ++ [H.length]), Live _ ℓ
        rw [← List.append_assoc]; exact hlv
  | seq e₂ =>
      simp only [stepRet]
      split
      · exact h
      · split
        · exact dropContents_ne_uad (by assumption)
        · exact h
  | ite e₁ e₂ =>
      simp only [stepRet]
      split
      · exact h
      · exact h
      · simp [StepLive]
  | assign p =>
      have henv := StackLive.env (K := K) h
      simp only [stepRet]
      split
      · exact rootCell_ne_uad henv (by assumption)
      · rename_i ℓ c _
        split
        · exact Contents.readAt_ne_uad _ (by assumption)
        · split
          · exact dropCell_ne_uad (by assumption)
          · split
            · simp [StepLive]
            · exact StackLive.grow (K := K) h (Grow.set_full H ℓ _)
  | ret =>
      have hK : StackLive H φ K := h
      simp only [stepRet]
      split
      · simp [StepLive]
      · rename_i φs K' hc
        obtain ⟨hs, hcc⟩ := hK.1.toCall hc
        have hnd : (callerCells K' ++ φs.scope ++ φ.scope).Nodup := by
          rw [← hcc]; exact hK.2.1
        have hlv : ∀ ℓ ∈ callerCells K' ++ φs.scope ++ φ.scope, Live H ℓ := by
          rw [← hcc]; exact hK.2.2
        split
        · exact unwind_err hnd hlv (by assumption)
        · rename_i H' evs hu
          obtain ⟨h₁, h₂⟩ := unwind_keeps hnd hlv hu
          exact ⟨hs, h₁, h₂⟩
  | endscope ℓs =>
      obtain ⟨⟨φ₀, hφ, hs⟩, hnd, hlv⟩ := h
      subst hφ
      have hnd' : (callerCells K ++ φ₀.scope ++ ℓs).Nodup := by
        rw [List.append_assoc]; exact hnd
      have hlv' : ∀ ℓ ∈ callerCells K ++ φ₀.scope ++ ℓs, Live H ℓ := by
        rw [List.append_assoc]; exact hlv
      simp only [stepRet]
      split
      · exact unwind_err hnd' hlv' (by assumption)
      · rename_i H' evs hu
        obtain ⟨h₁, h₂⟩ := unwind_keeps hnd' hlv' hu
        show StackLive H' _ K
        rw [Frame.popScope_ext]
        exact ⟨hs, h₁, h₂⟩
  | loop e φs =>
      obtain ⟨⟨xs, hφ, hs⟩, hnd, hlv⟩ := h
      subst hφ
      have hnd' : (callerCells K ++ φs.scope ++ xs).Nodup := by
        rw [List.append_assoc]; exact hnd
      have hlv' : ∀ ℓ ∈ callerCells K ++ φs.scope ++ xs, Live H ℓ := by
        rw [List.append_assoc]; exact hlv
      simp only [stepRet]
      split
      · simp only [List.drop_left]
        split
        · exact unwind_err hnd' hlv' (by assumption)
        · rename_i H' evs hu
          obtain ⟨h₁, h₂⟩ := unwind_keeps hnd' hlv' hu
          refine ⟨⟨[], ?_, hs⟩, h₁, h₂⟩
          cases φs; simp
      · simp [StepLive]
  | call φs =>
      obtain ⟨⟨_, hs⟩, hnd, hlv⟩ := h
      simp only [stepRet]
      split
      · exact unwind_err hnd hlv (by assumption)
      · rename_i H' evs hu
        obtain ⟨h₁, h₂⟩ := unwind_keeps hnd hlv hu
        exact ⟨hs, h₁, h₂⟩

/-- **The invariant over `Step`**: `step` keeps it, and never answers `stuck
.useAfterDrop` under it (helper). -/
theorem step_live (M : FloatOps) (P : Program) {C : Config} (h : ConfigLive C) :
    StepLive (step M P C) := by
  match C, h with
  | .panic _ _, _ => trivial
  | .run H φ K (.eval e) tr, h => exact stepEval_live M P h e
  | .run H φ K (.args t vs (e :: es)) tr, h => exact h
  | .run H φ K (.args t vs []) tr, h => exact stepArgs_live P h vs t
  | .run _ _ [] (.ret _) _, _ => trivial
  | .run H φ (k :: K) (.ret v) tr, h => exact stepRet_live M P v k h

/-- `→*` keeps the invariant (helper). -/
theorem steps_live {M : FloatOps} {P : Program} {C C' : Config} (hs : Steps M P C C') :
    ConfigLive C → ConfigLive C' := by
  induction hs with
  | refl => exact id
  | step h₁ _ ih =>
      intro hC
      have := step_live M P hC
      rw [step_iff.mp h₁] at this
      exact ih this

end RueCore.Retire

namespace RueCore

open Retire

/-- **No use-after-drop, on every program**: `run` never refuses with
`useAfterDrop`, checked or not. -/
theorem run_no_use_after_drop (M : FloatOps) (P : Program) (fuel : Nat) :
    run M P fuel ≠ .stuck .useAfterDrop := by
  intro h
  have := Retire.eval_live M P fuel [] { env := [], scope := [] } (.call 0 [])
    ⟨fun _ h => absurd h List.not_mem_nil, fun _ h => absurd h List.not_mem_nil,
      List.nodup_nil⟩
  unfold run at h
  rw [h] at this
  exact this rfl

/-- **No use-after-drop over §6's relation, on every program**: a
configuration `→*` reaches from `Config.init` is never stuck on a retired
cell, checked or not. -/
theorem step_no_use_after_drop (M : FloatOps) (P : Program) {C : Config}
    (h : Steps M P Config.init C) : ¬ C.Stuck M P .useAfterDrop := by
  intro hs
  have hl := Retire.step_live M P (Retire.steps_live h ⟨rfl, List.nodup_nil, fun _ h => absurd h List.not_mem_nil⟩)
  unfold Config.Stuck at hs
  rw [hs] at hl
  exact hl rfl


end RueCore
