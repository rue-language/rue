import RueCore.Soundness
import RueCore.Checker
import RueCore.Step

/-!
# RueCore.Trace — theorems over the drop trace (§7)

`eval`'s trace records every drop the machine runs, and since RUE-2323 it
records **which value** each drop is of: aggregate introduction mints a value
identity (`introVal`, `Dynamics.lean`), values carry it through every move,
and `Event.drop`, `Event.dropTemp` and `Event.dtor` carry the contents or the
value they ran on, identity included. This module states §7's
**no-double-free** bullet over that trace and proves it.

## What is counted

A `Copy` value is duplicated freely — (D-Use-Copy) §6.3, the dynamic-index
read, the repeat form — and it has no drop glue (§6.11: "scalars are Copy";
§3 gives a `Copy` struct no destructor, `3.9:31`), so a copy of one carries
its original's identity and nothing ever frees it. What the theorem counts is
the **owned** part of a value: `Contents.own`, the identities of its
non-`Copy` nodes, `⊘` skipped. Two projections of the trace read it:

* `freedIds`: the owned identities each `drop`/`dropTemp` marker frees — the
  whole dropped tree, which is where §6.11's walk goes;
* `dtorIds`: the identity of each value a user destructor ran on — §7's own
  wording, "every stored value's destructor runs at most once".

`no_double_free` says each identity occurs at most once in each. A
declared-linear destructure's residue is a third case neither projection
sees as freed: `dropResidue` frees it with no `drop`/`dropTemp` marker of its
own, so it is outside `freedIds` until RUE-2328 adds one; `dtorIds` already
catches it when the residue has a destructor.

## Why it holds: a conservation law

The proof is not about types. It is a **conservation law** over the machine,
proved by fuel induction over `eval` (`eval_conserves`): the owned
identities of the final store, the result value and the trace's projection
together are, as a multiset, at most the owned identities of the initial store
plus the identities minted during the run — and minting takes the store's next
index, so the minted ones are a range, each once (`Fresh`). Nothing is ever
duplicated: a move writes `⊘` where it took (§6.3), a drop consumes what it
drops, a `Copy` value owns nothing, and §6.11's `⊘`-skip is what keeps a
moved-out position from being counted twice — "this single skip is what makes
double-free impossible".

What the law needs from the program is **copy closure** (`Contents.copyClosed`):
nothing owned hides under a `Copy` node, or a copy would duplicate it. §3 makes
that a property of every well-typed value, and the machine enforces it with a
monitor at aggregate introduction and assignment (`Dynamics.lean`), so the law
holds of every finished run, typed or not, given only `WfDecls`' "a
destructor-bearing struct is not `Copy`". Typing enters `no_double_free`
through `no_violation`:
a program the checker accepts never reaches the monitor, or any other
refusal, so its trace is the whole run's — a `stuck` result carries no trace
and would make the statement vacuous.
-/

namespace RueCore

/-! ## Owned identities -/

mutual
/-- The identities of a contents' **owned** nodes: every non-`Copy` aggregate
in it, `⊘` skipped (§6.11's skip) and nothing below a `Copy` node, since a
`Copy` value is duplicated freely and has no drop glue. -/
def Contents.own (D : Decls) : Contents → List Nat
  | .hole | .int _ _ _ | .float _ _ | .bool _ | .unit => []
  | .struct s i cs => if D.classOf s = .copy then [] else i :: Contents.ownList D cs
  | .enum e _ i cs => if D.enumClassOf e = .copy then [] else i :: Contents.ownList D cs
  | .array T i cs =>
      if Ty.mult D (.array T cs.length) = .copy then [] else i :: Contents.ownList D cs

/-- `own` over a field, payload or element list (helper). -/
def Contents.ownList (D : Decls) : List Contents → List Nat
  | [] => []
  | c :: cs => Contents.own D c ++ Contents.ownList D cs
end

/-- A value's owned identities: its stored image's (helper). -/
abbrev Val.own (D : Decls) (v : Val) : List Nat := (Contents.ofVal v).own D

/-- A cell's owned identities: a retired cell, or a reserved identity slot,
holds none (helper). -/
def Cell.own (D : Decls) : Cell → List Nat
  | .full c => c.own D
  | .dead => []

/-- The store's owned identities, cell by cell (helper). -/
def storeOwn (D : Decls) (H : Store) : List Nat := H.flatMap (Cell.own D)

/-- Every live cell of the store is copy-closed (helper). -/
def StoreCC (D : Decls) (H : Store) : Prop :=
  ∀ (ℓ : Nat) (c : Contents), H[ℓ]? = some (Cell.full c) → c.copyClosed D = true

/-- Multiset inclusion, read by counts: every identity occurs in `l₁` at most
as often as in `l₂` (helper). -/
def IdLe (l₁ l₂ : List Nat) : Prop := ∀ a, l₁.count a ≤ l₂.count a

/-- The identities minted between two stores: the indices the store grew by,
since `introVal` takes the next index (helper). -/
def Fresh (H H' : Store) : List Nat := List.range' H.length (H'.length - H.length)

/-! ## The trace's projections -/

/-- The owned identities a `drop` or `dropTemp` marker frees: the whole tree
§6.11's walk goes through — and the shell a `consume` event ends (RUE-2427),
whose members were already moved out or dropped. A destructor event frees
nothing of its own — it is nested under a marker — and a `@dbg` frees nothing
(helper). -/
def Event.freed (D : Decls) : Event → List Nat
  | .drop _ c => c.own D
  | .dropTemp v => v.own D
  | .consume c => c.own D
  | .dtor _ _ | .dbg _ => []

/-- The identity of the value a user destructor ran on (§6.11, `3.9:28`) —
every `dtor` event carries the struct it ran on (helper). -/
def Event.dtorIds : Event → List Nat
  | .dtor _ (.struct _ i _) => [i]
  | _ => []

/-- The identities the trace's markers free, in trace order: what §7's
no-double-free bullet counts at a drop. -/
def freedIds (D : Decls) (tr : List Event) : List Nat := tr.flatMap (Event.freed D)

/-- The identities the trace's destructors ran on, in trace order: what §7's
no-double-free bullet counts at a destructor (`3.9:28`). -/
def dtorIds (tr : List Event) : List Nat := tr.flatMap Event.dtorIds

/-- `dtorIds` distributes over concatenation (helper). -/
theorem dtorIds_append (l₁ l₂ : List Event) : dtorIds (l₁ ++ l₂) = dtorIds l₁ ++ dtorIds l₂ := by
  simp [dtorIds, List.flatMap_append]

/-- `dtorIds` of the empty trace (helper). -/
@[simp] theorem dtorIds_nil : dtorIds [] = [] := rfl

/-- The trace a result carries: everything the run emitted, for a value, an
unwinding `return` or `break`, and a trap; nothing for a refusal or exhausted
fuel (helper). -/
def EvalRes.trace : EvalRes → List Event
  | .ok _ _ tr | .returned _ _ tr | .broke _ _ tr | .panic _ tr => tr
  | .stuck _ | .outOfFuel => []

/-! ## Owned identities: the node-level facts -/

/-- A `Copy` node owns nothing (helper). -/
theorem Contents.own_of_mult {D : Decls} {c : Contents} (h : c.mult D = .copy) : c.own D = [] := by
  cases c <;> simp_all [Contents.mult, Contents.own]

/-- An all-`Copy` contents is `Copy` at its root (helper). -/
theorem Contents.allCopy_mult {D : Decls} {c : Contents} (h : c.allCopy D = true) :
    c.mult D = .copy := by
  cases c <;> simp_all [Contents.allCopy, Contents.mult]

/-- An all-`Copy` contents owns nothing (helper). -/
theorem Contents.allCopy_own {D : Decls} {c : Contents} (h : c.allCopy D = true) :
    c.own D = [] :=
  Contents.own_of_mult (Contents.allCopy_mult h)

/-- An all-`Copy` list owns nothing (helper). -/
theorem Contents.allCopyList_own {D : Decls} :
    ∀ {cs : List Contents}, Contents.allCopyList D cs = true → Contents.ownList D cs = []
  | [], _ => rfl
  | c :: cs, h => by
      simp only [Contents.allCopyList, Bool.and_eq_true] at h
      simp [Contents.ownList, Contents.allCopy_own h.1, Contents.allCopyList_own h.2]

/-- A copy-closed node that is `Copy` is `Copy` all the way down (helper). -/
theorem Contents.copyClosed_allCopy {D : Decls} {c : Contents} (hc : c.copyClosed D = true)
    (hm : c.mult D = .copy) : c.allCopy D = true := by
  cases c <;> simp_all [Contents.copyClosed, Contents.mult, Contents.allCopy]

mutual
/-- All-`Copy` contents is copy-closed (helper). -/
theorem Contents.allCopy_copyClosed {D : Decls} :
    ∀ {c : Contents}, c.allCopy D = true → c.copyClosed D = true
  | .hole, _ | .int _ _ _, _ | .float _ _, _ | .bool _, _ | .unit, _ => rfl
  | .struct s i cs, h => by
      simp only [Contents.allCopy, Bool.and_eq_true, decide_eq_true_eq] at h
      simp [Contents.copyClosed, h.1, h.2]
  | .enum e k i cs, h => by
      simp only [Contents.allCopy, Bool.and_eq_true, decide_eq_true_eq] at h
      simp [Contents.copyClosed, h.1, h.2]
  | .array T i cs, h => by
      simp only [Contents.allCopy, Bool.and_eq_true, decide_eq_true_eq] at h
      simp [Contents.copyClosed, h.1, h.2]

/-- The same over a list (helper). -/
theorem Contents.allCopyList_copyClosedList {D : Decls} :
    ∀ {cs : List Contents}, Contents.allCopyList D cs = true → Contents.copyClosedList D cs = true
  | [], _ => rfl
  | c :: cs, h => by
      simp only [Contents.allCopyList, Bool.and_eq_true] at h
      simp [Contents.copyClosedList, Contents.allCopy_copyClosed h.1,
        Contents.allCopyList_copyClosedList h.2]
end

/-- A member of a copy-closed list is copy-closed (helper). -/
theorem Contents.copyClosedList_index {D : Decls} :
    ∀ {cs : List Contents} {f : Nat} {c : Contents},
      Contents.copyClosedList D cs = true → cs[f]? = some c → c.copyClosed D = true
  | [], _, _, _, h => by simp at h
  | c₀ :: cs, 0, c, h, hg => by
      simp only [List.getElem?_cons_zero, Option.some_inj] at hg
      subst hg
      simp only [Contents.copyClosedList, Bool.and_eq_true] at h
      exact h.1
  | c₀ :: cs, f + 1, c, h, hg => by
      simp only [List.getElem?_cons_succ] at hg
      simp only [Contents.copyClosedList, Bool.and_eq_true] at h
      exact Contents.copyClosedList_index h.2 hg

/-- A member of an all-`Copy` list is all-`Copy` (helper). -/
theorem Contents.allCopyList_index {D : Decls} :
    ∀ {cs : List Contents} {f : Nat} {c : Contents},
      Contents.allCopyList D cs = true → cs[f]? = some c → c.allCopy D = true
  | [], _, _, _, h => by simp at h
  | c₀ :: cs, 0, c, h, hg => by
      simp only [List.getElem?_cons_zero, Option.some_inj] at hg
      subst hg
      simp only [Contents.allCopyList, Bool.and_eq_true] at h
      exact h.1
  | c₀ :: cs, f + 1, c, h, hg => by
      simp only [List.getElem?_cons_succ] at hg
      simp only [Contents.allCopyList, Bool.and_eq_true] at h
      exact Contents.allCopyList_index h.2 hg

/-- Writing a copy-closed member keeps a copy-closed list so (helper). -/
theorem Contents.copyClosedList_set {D : Decls} :
    ∀ {cs : List Contents} (f : Nat) {c : Contents},
      Contents.copyClosedList D cs = true → c.copyClosed D = true →
      Contents.copyClosedList D (cs.set f c) = true
  | [], _, _, h, _ => by simpa using h
  | c₀ :: cs, 0, c, h, hc => by
      simp only [Contents.copyClosedList, Bool.and_eq_true] at h
      simp [Contents.copyClosedList, hc, h.2]
  | c₀ :: cs, f + 1, c, h, hc => by
      simp only [Contents.copyClosedList, Bool.and_eq_true] at h
      simp [Contents.copyClosedList, h.1, Contents.copyClosedList_set f h.2 hc]

/-- Writing an all-`Copy` member keeps an all-`Copy` list so (helper). -/
theorem Contents.allCopyList_set {D : Decls} :
    ∀ {cs : List Contents} (f : Nat) {c : Contents},
      Contents.allCopyList D cs = true → c.allCopy D = true →
      Contents.allCopyList D (cs.set f c) = true
  | [], _, _, h, _ => by simpa using h
  | c₀ :: cs, 0, c, h, hc => by
      simp only [Contents.allCopyList, Bool.and_eq_true] at h
      simp [Contents.allCopyList, hc, h.2]
  | c₀ :: cs, f + 1, c, h, hc => by
      simp only [Contents.allCopyList, Bool.and_eq_true] at h
      simp [Contents.allCopyList, h.1, Contents.allCopyList_set f h.2 hc]

/-- **Replacing one member, counted**: the list's owned identities lose the
old member's and gain the new one's (helper). -/
theorem Contents.ownList_set_count {D : Decls} (a : Nat) :
    ∀ {cs : List Contents} {f : Nat} {c : Contents} (c' : Contents), cs[f]? = some c →
      (Contents.ownList D (cs.set f c')).count a + (c.own D).count a
        = (Contents.ownList D cs).count a + (c'.own D).count a
  | [], _, _, _, h => by simp at h
  | c₀ :: cs, 0, c, c', hg => by
      simp only [List.getElem?_cons_zero, Option.some_inj] at hg
      subst hg
      simp only [List.set_cons_zero, Contents.ownList, List.count_append]
      omega
  | c₀ :: cs, f + 1, c, c', hg => by
      simp only [List.getElem?_cons_succ] at hg
      simp only [List.set_cons_succ, Contents.ownList, List.count_append]
      have := Contents.ownList_set_count (D := D) a c' hg
      omega

mutual
/-- A value's stored image read back is the value (helper). -/
theorem Contents.ofVal_toVal : ∀ {c : Contents} {v : Val}, c.toVal = some v → Contents.ofVal v = c
  | .hole, _, h => by simp [Contents.toVal] at h
  | .int _ _ _, _, h | .float _ _, _, h | .bool _, _, h | .unit, _, h => by
      simp [Contents.toVal] at h; subst h; rfl
  | .struct s i cs, v, h => by
      simp only [Contents.toVal, Option.map_eq_some_iff] at h
      obtain ⟨vs, hvs, rfl⟩ := h
      simp [Contents.ofVal, Contents.ofVals_toVals hvs]
  | .enum e k i cs, v, h => by
      simp only [Contents.toVal, Option.map_eq_some_iff] at h
      obtain ⟨vs, hvs, rfl⟩ := h
      simp [Contents.ofVal, Contents.ofVals_toVals hvs]
  | .array T i cs, v, h => by
      simp only [Contents.toVal, Option.map_eq_some_iff] at h
      obtain ⟨vs, hvs, rfl⟩ := h
      simp [Contents.ofVal, Contents.ofVals_toVals hvs]

/-- The same over a list (helper). -/
theorem Contents.ofVals_toVals :
    ∀ {cs : List Contents} {vs : List Val}, Contents.toVals cs = some vs → Contents.ofVals vs = cs
  | [], vs, h => by simp [Contents.toVals] at h; subst h; rfl
  | c :: cs, vs, h => by
      simp only [Contents.toVals] at h
      split at h
      · rename_i v vs' hv hvs
        cases h
        simp [Contents.ofVals, Contents.ofVal_toVal hv, Contents.ofVals_toVals hvs]
      · cases h
end

/-! ## Paths: a read, a write, and what they own -/

/-- A position read out of an all-`Copy` contents is all-`Copy` (helper). -/
theorem Contents.readAt_allCopy {D : Decls} : ∀ (π : List Nat) {c sub : Contents},
    c.allCopy D = true → c.readAt π = .ok sub → sub.allCopy D = true
  | [], c, sub, h, hr => by simp [Contents.readAt] at hr; subst hr; exact h
  | f :: π, c, sub, h, hr => by
      cases c with
      | struct s i cs =>
          simp only [Contents.readAt] at hr
          split at hr
          · rename_i cf hcf
            simp only [Contents.allCopy, Bool.and_eq_true] at h
            exact Contents.readAt_allCopy π (Contents.allCopyList_index h.2 hcf) hr
          · cases hr
      | array T i cs =>
          simp only [Contents.readAt] at hr
          split at hr
          · rename_i cf hcf
            simp only [Contents.allCopy, Bool.and_eq_true] at h
            exact Contents.readAt_allCopy π (Contents.allCopyList_index h.2 hcf) hr
          · cases hr
      | _ => simp [Contents.readAt] at hr

/-- A position read out of a copy-closed contents is copy-closed (helper). -/
theorem Contents.readAt_copyClosed {D : Decls} : ∀ (π : List Nat) {c sub : Contents},
    c.copyClosed D = true → c.readAt π = .ok sub → sub.copyClosed D = true
  | [], c, sub, h, hr => by simp [Contents.readAt] at hr; subst hr; exact h
  | f :: π, c, sub, h, hr => by
      cases c with
      | struct s i cs =>
          simp only [Contents.readAt] at hr
          split at hr
          · rename_i cf hcf
            simp only [Contents.copyClosed] at h
            split at h
            · exact Contents.allCopy_copyClosed
                (Contents.readAt_allCopy π (Contents.allCopyList_index h hcf) hr)
            · exact Contents.readAt_copyClosed π (Contents.copyClosedList_index h hcf) hr
          · cases hr
      | array T i cs =>
          simp only [Contents.readAt] at hr
          split at hr
          · rename_i cf hcf
            simp only [Contents.copyClosed] at h
            split at h
            · exact Contents.allCopy_copyClosed
                (Contents.readAt_allCopy π (Contents.allCopyList_index h hcf) hr)
            · exact Contents.readAt_copyClosed π (Contents.copyClosedList_index h hcf) hr
          · cases hr
      | _ => simp [Contents.readAt] at hr

/-- Writing all-`Copy` contents into all-`Copy` contents keeps it so (helper). -/
theorem Contents.writeAt_allCopy {D : Decls} : ∀ (π : List Nat) {c new c' : Contents},
    c.allCopy D = true → new.allCopy D = true → c.writeAt π new = some c' →
    c'.allCopy D = true
  | [], c, new, c', _, hn, hw => by simp [Contents.writeAt] at hw; subst hw; exact hn
  | f :: π, c, new, c', h, hn, hw => by
      cases c with
      | struct s i cs =>
          simp only [Contents.writeAt] at hw
          split at hw
          · rename_i cf hcf
            simp only [Option.map_eq_some_iff] at hw
            obtain ⟨cf', hw', rfl⟩ := hw
            simp only [Contents.allCopy, Bool.and_eq_true] at h ⊢
            exact ⟨h.1, Contents.allCopyList_set f h.2
              (Contents.writeAt_allCopy π (Contents.allCopyList_index h.2 hcf) hn hw')⟩
          · cases hw
      | array T i cs =>
          simp only [Contents.writeAt] at hw
          split at hw
          · rename_i cf hcf
            simp only [Option.map_eq_some_iff] at hw
            obtain ⟨cf', hw', rfl⟩ := hw
            simp only [Contents.allCopy, Bool.and_eq_true, List.length_set] at h ⊢
            exact ⟨h.1, Contents.allCopyList_set f h.2
              (Contents.writeAt_allCopy π (Contents.allCopyList_index h.2 hcf) hn hw')⟩
          · cases hw
      | _ => simp [Contents.writeAt] at hw

/-- **A `⊘` write keeps copy closure** (§6.3's `H[ℓ@π ↦ ⊘]`): `⊘` is
all-`Copy`, so it may sit under any node (helper). -/
theorem Contents.writeAt_copyClosed {D : Decls} : ∀ (π : List Nat) {c new c' : Contents},
    c.copyClosed D = true → new.allCopy D = true → c.writeAt π new = some c' →
    c'.copyClosed D = true
  | [], c, new, c', _, hn, hw => by
      simp [Contents.writeAt] at hw; subst hw; exact Contents.allCopy_copyClosed hn
  | f :: π, c, new, c', h, hn, hw => by
      cases c with
      | struct s i cs =>
          simp only [Contents.writeAt] at hw
          split at hw
          · rename_i cf hcf
            simp only [Option.map_eq_some_iff] at hw
            obtain ⟨cf', hw', rfl⟩ := hw
            simp only [Contents.copyClosed] at h ⊢
            split at h
            · rename_i hc
              rw [if_pos hc]
              exact Contents.allCopyList_set f h
                (Contents.writeAt_allCopy π (Contents.allCopyList_index h hcf) hn hw')
            · rename_i hc
              rw [if_neg hc]
              exact Contents.copyClosedList_set f h
                (Contents.writeAt_copyClosed π (Contents.copyClosedList_index h hcf) hn hw')
          · cases hw
      | array T i cs =>
          simp only [Contents.writeAt] at hw
          split at hw
          · rename_i cf hcf
            simp only [Option.map_eq_some_iff] at hw
            obtain ⟨cf', hw', rfl⟩ := hw
            simp only [Contents.copyClosed, List.length_set] at h ⊢
            split at h
            · rename_i hc
              rw [if_pos hc]
              exact Contents.allCopyList_set f h
                (Contents.writeAt_allCopy π (Contents.allCopyList_index h hcf) hn hw')
            · rename_i hc
              rw [if_neg hc]
              exact Contents.copyClosedList_set f h
                (Contents.writeAt_copyClosed π (Contents.copyClosedList_index h hcf) hn hw')
          · cases hw
      | _ => simp [Contents.writeAt] at hw

/-- **A write at a path, counted**: the contents after the write owns what it
owned before, less what sat at the path, plus what was written. This is §6.3's
move (write `⊘`, hand the old sub-tree on) and §6.8's store (drop the old
sub-tree, write the new value) read as a ledger. Below a `Copy` node both
sides own nothing there, which is where copy closure is needed (helper). -/
theorem Contents.writeAt_own {D : Decls} (a : Nat) : ∀ (π : List Nat) {c sub new c' : Contents},
    c.copyClosed D = true → c.readAt π = .ok sub → c.writeAt π new = some c' →
    (c'.own D).count a + (sub.own D).count a ≤ (c.own D).count a + (new.own D).count a
  | [], c, sub, new, c', _, hr, hw => by
      simp [Contents.readAt] at hr; simp [Contents.writeAt] at hw; subst hr; subst hw; omega
  | f :: π, c, sub, new, c', hcc, hr, hw => by
      cases c with
      | struct s i cs =>
          simp only [Contents.readAt] at hr
          simp only [Contents.writeAt] at hw
          split at hr
          · rename_i cf hcf
            rw [hcf] at hw
            simp only [Option.map_eq_some_iff] at hw
            obtain ⟨cf', hw', rfl⟩ := hw
            simp only [Contents.copyClosed] at hcc
            by_cases hc : D.classOf s = .copy
            · rw [if_pos hc] at hcc
              have hsub := Contents.readAt_allCopy π (Contents.allCopyList_index hcc hcf) hr
              simp [Contents.own, hc, Contents.allCopy_own hsub]
            · rw [if_neg hc] at hcc
              have ih := Contents.writeAt_own a π (Contents.copyClosedList_index hcc hcf) hr hw'
              have hset := Contents.ownList_set_count (D := D) a cf' hcf
              simp only [Contents.own, if_neg hc, List.count_cons]
              omega
          · cases hr
      | array T i cs =>
          simp only [Contents.readAt] at hr
          simp only [Contents.writeAt] at hw
          split at hr
          · rename_i cf hcf
            rw [hcf] at hw
            simp only [Option.map_eq_some_iff] at hw
            obtain ⟨cf', hw', rfl⟩ := hw
            simp only [Contents.copyClosed] at hcc
            by_cases hc : Ty.mult D (.array T cs.length) = .copy
            · rw [if_pos hc] at hcc
              have hsub := Contents.readAt_allCopy π (Contents.allCopyList_index hcc hcf) hr
              simp [Contents.own, hc, Contents.allCopy_own hsub]
            · rw [if_neg hc] at hcc
              have ih := Contents.writeAt_own a π (Contents.copyClosedList_index hcc hcf) hr hw'
              have hset := Contents.ownList_set_count (D := D) a cf' hcf
              simp only [Contents.own, List.length_set, if_neg hc, List.count_cons]
              omega
          · cases hr
      | _ => simp [Contents.readAt] at hr

/-! ## The store, counted -/

/-- Growing the store appends what the new cells own (helper). -/
theorem storeOwn_append (D : Decls) (H ext : Store) :
    storeOwn D (H ++ ext) = storeOwn D H ++ storeOwn D ext := by
  simp [storeOwn, List.flatMap_append]

/-- **Replacing one cell, counted** (helper). -/
theorem storeOwn_set_count (D : Decls) (a : Nat) : ∀ {H : Store} {ℓ : Nat} {x : Cell} (y : Cell),
    H[ℓ]? = some x →
      (storeOwn D (H.set ℓ y)).count a + (x.own D).count a
        = (storeOwn D H).count a + (y.own D).count a
  | [], _, _, _, h => by simp at h
  | c :: H, 0, x, y, h => by
      simp only [List.getElem?_cons_zero, Option.some_inj] at h
      subst h
      simp only [storeOwn, List.set_cons_zero, List.flatMap_cons, List.count_append]
      omega
  | c :: H, ℓ + 1, x, y, h => by
      simp only [List.getElem?_cons_succ] at h
      have := storeOwn_set_count D a y h
      simp only [storeOwn] at this
      simp only [storeOwn, List.set_cons_succ, List.flatMap_cons, List.count_append]
      omega

/-- Growing the store by a copy-closed cell keeps it copy-closed (helper). -/
theorem StoreCC.append {D : Decls} {H ext : Store} (h : StoreCC D H) (he : StoreCC D ext) :
    StoreCC D (H ++ ext) := by
  intro ℓ c hc
  by_cases hl : ℓ < H.length
  · rw [List.getElem?_append_left hl] at hc; exact h ℓ c hc
  · rw [List.getElem?_append_right (by omega)] at hc; exact he _ c hc

/-- A store of one copy-closed cell is copy-closed (helper). -/
theorem StoreCC.single {D : Decls} {c : Contents} (h : c.copyClosed D = true) :
    StoreCC D [.full c] := by
  intro ℓ c' hc
  cases ℓ with
  | zero => simp at hc; subst hc; exact h
  | succ ℓ => simp at hc

/-- A `†` cell is copy-closed (helper). -/
theorem StoreCC.dead {D : Decls} : StoreCC D [.dead] := by
  intro ℓ c hc
  cases ℓ with
  | zero => simp at hc
  | succ ℓ => simp at hc

/-- Writing a copy-closed cell keeps the store copy-closed (helper). -/
theorem StoreCC.set {D : Decls} {H : Store} {ℓ : Nat} {c : Contents} (h : StoreCC D H)
    (hc : c.copyClosed D = true) : StoreCC D (H.set ℓ (.full c)) := by
  intro ℓ' c' hc'
  by_cases he : ℓ = ℓ'
  · subst he
    simp only [List.getElem?_set_self'] at hc'
    simp at hc'
    obtain ⟨_, rfl⟩ := hc'
    exact hc
  · rw [List.getElem?_set_ne he] at hc'; exact h ℓ' c' hc'

/-- Retiring a cell keeps the store copy-closed (helper). -/
theorem StoreCC.set_dead {D : Decls} {H : Store} {ℓ : Nat} (h : StoreCC D H) :
    StoreCC D (H.set ℓ .dead) := by
  intro ℓ' c' hc'
  by_cases he : ℓ = ℓ'
  · subst he
    rw [List.getElem?_set] at hc'
    split at hc' <;> simp_all
  · rw [List.getElem?_set_ne he] at hc'; exact h ℓ' c' hc'

/-! ## §6.11's walk, counted -/

/-- A destructor-bearing struct is not `Copy` (`3.9:31`): the one fact about
the declarations the conservation law reads (helper). -/
def DtorNotCopy (D : Decls) : Prop :=
  ∀ (s : Nat) (sd : StructDecl), D.structs[s]? = some sd → sd.dtor = true → D.classOf s ≠ .copy

/-- `WfDecls` gives it: only `@copy` lifts to `Copy`, and a `@copy`
declaration declares no destructor (`3.8:18`, `3.9:31`) (helper). -/
theorem WfDecls.dtorNotCopy {D : Decls} (h : WfDecls D) : DtorNotCopy D := by
  intro s sd hd hdt hc
  have hcls : sd.cls = .copy := by simpa only [Decls.classOf, hd] using hc
  have hw := h.structs s sd hd
  have hattr : sd.attr = .copy := by
    have hj := hw.classIsJoin
    rw [hcls] at hj
    cases ha : sd.attr with
    | copy => rfl
    | linear => rw [ha] at hj; cases hj
    | none =>
        rw [ha] at hj
        simp only [Attr.lift] at hj
        split at hj <;> cases hj
  rw [(hw.copyWf hattr).2] at hdt
  cases hdt

mutual
/-- §6.11's walk emits only destructor events, so it frees nothing a marker
would count (helper). -/
theorem dropContents_freed {D : Decls} : ∀ {c : Contents} {evs : List Event},
    dropContents D c = .ok evs → evs.flatMap (Event.freed D) = []
  | .hole, _, h | .int _ _ _, _, h | .float _ _, _, h | .bool _, _, h | .unit, _, h => by
      simp [dropContents] at h; subst h; rfl
  | .struct s i cs, evs, h => by
      simp only [dropContents] at h
      split at h
      · cases h
      · split at h
        · cases h
        · rename_i evs' hl
          cases h
          split <;> simp [Event.freed, dropContentsList_freed hl]
  | .enum _ _ _ cs, _, h => by simp only [dropContents] at h; exact dropContentsList_freed h
  | .array _ _ cs, _, h => by simp only [dropContents] at h; exact dropContentsList_freed h

/-- The same over a list (helper). -/
theorem dropContentsList_freed {D : Decls} : ∀ {cs : List Contents} {evs : List Event},
    dropContentsList D cs = .ok evs → evs.flatMap (Event.freed D) = []
  | [], _, h => by simp [dropContentsList] at h; subst h; rfl
  | c :: cs, _, h => by
      simp only [dropContentsList] at h
      split at h
      · cases h
      · rename_i e₁ h₁
        split at h
        · cases h
        · rename_i e₂ h₂
          cases h
          simp [List.flatMap_append, dropContents_freed h₁, dropContentsList_freed h₂]
end

mutual
/-- An all-`Copy` contents runs no destructor (`3.9:31`) (helper). -/
theorem dropContents_allCopy_dtor {D : Decls} (hdt : DtorNotCopy D) :
    ∀ {c : Contents} {evs : List Event},
      c.allCopy D = true → dropContents D c = .ok evs → dtorIds evs = []
  | .hole, _, _, h | .int _ _ _, _, _, h | .float _ _, _, _, h | .bool _, _, _, h
  | .unit, _, _, h => by
      simp [dropContents] at h; subst h; rfl
  | .struct s i cs, evs, hac, h => by
      simp only [Contents.allCopy, Bool.and_eq_true, decide_eq_true_eq] at hac
      simp only [dropContents] at h
      split at h
      · cases h
      · rename_i sd hd
        split at h
        · cases h
        · rename_i evs' hl
          cases h
          have hnd : sd.dtor = false := by
            cases hsd : sd.dtor
            · rfl
            · exact absurd hac.1 (hdt s sd hd hsd)
          rw [hnd]
          simpa using dropContentsList_allCopy_dtor hdt hac.2 hl
  | .enum _ _ _ cs, _, hac, h => by
      simp only [Contents.allCopy, Bool.and_eq_true] at hac
      simp only [dropContents] at h; exact dropContentsList_allCopy_dtor hdt hac.2 h
  | .array _ _ cs, _, hac, h => by
      simp only [Contents.allCopy, Bool.and_eq_true] at hac
      simp only [dropContents] at h; exact dropContentsList_allCopy_dtor hdt hac.2 h

/-- The same over a list (helper). -/
theorem dropContentsList_allCopy_dtor {D : Decls} (hdt : DtorNotCopy D) :
    ∀ {cs : List Contents} {evs : List Event},
      Contents.allCopyList D cs = true → dropContentsList D cs = .ok evs → dtorIds evs = []
  | [], _, _, h => by simp [dropContentsList] at h; subst h; rfl
  | c :: cs, _, hac, h => by
      simp only [Contents.allCopyList, Bool.and_eq_true] at hac
      simp only [dropContentsList] at h
      split at h
      · cases h
      · rename_i e₁ h₁
        split at h
        · cases h
        · rename_i e₂ h₂
          cases h
          rw [dtorIds_append, dropContents_allCopy_dtor hdt hac.1 h₁,
            dropContentsList_allCopy_dtor hdt hac.2 h₂]; rfl
end

mutual
/-- **§6.11's walk runs each owned node's destructor at most once**: the
identities its destructor events name are, as a multiset, among the ones the
dropped contents owns. A `⊘` owns nothing and emits nothing (`3.8:60`'s skip);
a `Copy` node emits nothing either, because a destructor-bearing struct is
never `Copy` (helper). -/
theorem dropContents_dtor {D : Decls} (hdt : DtorNotCopy D) (a : Nat) :
    ∀ {c : Contents} {evs : List Event},
      c.copyClosed D = true → dropContents D c = .ok evs →
        (dtorIds evs).count a ≤ (c.own D).count a
  | .hole, _, _, h | .int _ _ _, _, _, h | .float _ _, _, _, h | .bool _, _, _, h
  | .unit, _, _, h => by
      simp [dropContents] at h; subst h; simp [dtorIds]
  | .struct s i cs, evs, hcc, h => by
      by_cases hc : D.classOf s = .copy
      · have hac : (Contents.struct s i cs).allCopy D = true :=
          Contents.copyClosed_allCopy hcc (by simpa [Contents.mult] using hc)
        rw [dropContents_allCopy_dtor hdt hac h]; simp
      · simp only [Contents.copyClosed, if_neg hc] at hcc
        simp only [dropContents] at h
        split at h
        · cases h
        · rename_i sd hd
          split at h
          · cases h
          · rename_i evs' hl
            cases h
            have ih := dropContentsList_dtor hdt a hcc hl
            simp only [Contents.own, if_neg hc, List.count_cons]
            rw [dtorIds_append]
            split
            · simp only [dtorIds, List.flatMap_cons, List.flatMap_nil, Event.dtorIds,
                List.append_nil, List.count_append, List.count_singleton] at ih ⊢
              split <;> simp_all <;> omega
            · simp only [dtorIds_nil, List.nil_append]
              omega
  | .enum e k i cs, evs, hcc, h => by
      by_cases hc : D.enumClassOf e = .copy
      · have hac : (Contents.enum e k i cs).allCopy D = true :=
          Contents.copyClosed_allCopy hcc (by simpa [Contents.mult] using hc)
        rw [dropContents_allCopy_dtor hdt hac h]; simp
      · simp only [Contents.copyClosed, if_neg hc] at hcc
        simp only [dropContents] at h
        have ih := dropContentsList_dtor hdt a hcc h
        simp only [Contents.own, if_neg hc, List.count_cons]
        omega
  | .array T i cs, evs, hcc, h => by
      by_cases hc : Ty.mult D (.array T cs.length) = .copy
      · have hac : (Contents.array T i cs).allCopy D = true :=
          Contents.copyClosed_allCopy hcc (by simpa [Contents.mult] using hc)
        rw [dropContents_allCopy_dtor hdt hac h]; simp
      · simp only [Contents.copyClosed, if_neg hc] at hcc
        simp only [dropContents] at h
        have ih := dropContentsList_dtor hdt a hcc h
        simp only [Contents.own, if_neg hc, List.count_cons]
        omega

/-- The same over a list (helper). -/
theorem dropContentsList_dtor {D : Decls} (hdt : DtorNotCopy D) (a : Nat) :
    ∀ {cs : List Contents} {evs : List Event},
      Contents.copyClosedList D cs = true → dropContentsList D cs = .ok evs →
        (dtorIds evs).count a ≤ (Contents.ownList D cs).count a
  | [], _, _, h => by simp [dropContentsList] at h; subst h; simp [dtorIds]
  | c :: cs, _, hcc, h => by
      simp only [Contents.copyClosedList, Bool.and_eq_true] at hcc
      simp only [dropContentsList] at h
      split at h
      · cases h
      · rename_i e₁ h₁
        split at h
        · cases h
        · rename_i e₂ h₂
          cases h
          have i₁ := dropContents_dtor hdt a hcc.1 h₁
          have i₂ := dropContentsList_dtor hdt a hcc.2 h₂
          rw [dtorIds_append, List.count_append]
          simp only [Contents.ownList, List.count_append]
          omega
end

/-! ## A trace projection the law can count -/

/-- What the conservation law asks of a projection `F` of the trace onto
identities: §6.11's walk projects to at most what the dropped contents owns,
with a binding's `drop` marker or a temporary's `dropTemp` marker on top, and
a `@dbg` projects to nothing. `freedIds` and `dtorIds` are the two projections
§7's bullet is about (`freed_measure`, `dtor_measure`) (helper). -/
structure TraceMeasure (D : Decls) (F : Event → List Nat) : Prop where
  /-- §6.11's walk, with no marker: a destructure's `Copy` residue subtree
  (§6.3), which `residueMark` gives no marker. -/
  walk : ∀ {c : Contents} {evs : List Event}, c.copyClosed D = true →
    dropContents D c = .ok evs → IdLe (evs.flatMap F) (c.own D)
  /-- A binding's drop: its `drop ℓ c` marker, then the walk (§6.11). -/
  marker : ∀ {ℓ : Nat} {c : Contents} {evs : List Event}, c.copyClosed D = true →
    dropContents D c = .ok evs → IdLe (F (.drop ℓ c) ++ evs.flatMap F) (c.own D)
  /-- A discarded temporary: its `dropTemp v` marker, then the walk (§6.7). -/
  temp : ∀ {v : Val} {evs : List Event}, (Contents.ofVal v).copyClosed D = true →
    dropContents D (Contents.ofVal v) = .ok evs → IdLe (F (.dropTemp v) ++ evs.flatMap F) (v.own D)
  /-- A consumption ends at most its shell (RUE-2427). -/
  consume : ∀ c, IdLe (F (.consume c)) (c.own D)
  /-- `@dbg` frees nothing. -/
  dbg : ∀ v, F (.dbg v) = []

/-- **`freedIds` is countable**: a marker frees exactly the tree it names, and
the walk under it names nothing more (helper). -/
theorem freed_measure (D : Decls) : TraceMeasure D (Event.freed D) where
  walk := fun _ h a => by rw [dropContents_freed h]; simp
  marker := fun _ h a => by rw [List.count_append, dropContents_freed h]; simp [Event.freed]
  temp := fun _ h a => by rw [List.count_append, dropContents_freed h]; simp [Event.freed]
  consume := fun _ a => by simp [Event.freed]
  dbg := fun _ => rfl

/-- **`dtorIds` is countable**: a destructor runs on an owned node of the
dropped tree, once per node (helper). -/
theorem dtor_measure {D : Decls} (hdt : DtorNotCopy D) : TraceMeasure D Event.dtorIds where
  walk := fun hc h a => dropContents_dtor hdt a hc h
  marker := fun hc h a => by
    simpa [Event.dtorIds, dtorIds] using dropContents_dtor hdt a hc h
  temp := fun hc h a => by
    simpa [Event.dtorIds, dtorIds] using dropContents_dtor hdt a hc h
  consume := fun _ a => by simp [Event.dtorIds]
  dbg := fun _ => rfl

/-- A binding's drop (`dropCell`, §6.11), counted: nothing for a `Copy` cell,
the marker and the walk otherwise (helper). -/
theorem dropCell_measure {D : Decls} {F : Event → List Nat} (hF : TraceMeasure D F)
    {ℓ : Nat} {c : Contents} {evs : List Event} (hc : c.copyClosed D = true)
    (h : dropCell D ℓ c = .ok evs) : IdLe (evs.flatMap F) (c.own D) := by
  unfold dropCell at h
  split at h
  · cases h; intro a; simp
  · split at h
    · cases h
    · rename_i evs' hw
      cases h
      intro a
      have := hF.marker (ℓ := ℓ) hc hw a
      simpa using this

/-- `drop-retire` (§6.1), counted: the cell's owned identities leave the store
and at most those reach the trace (helper). -/
theorem dropRetire_measure {D : Decls} {F : Event → List Nat} (hF : TraceMeasure D F)
    {H H' : Store} {ℓ : Nat} {evs : List Event} (hcc : StoreCC D H)
    (h : dropRetire D H ℓ = .ok (H', evs)) :
    (∀ a, (storeOwn D H').count a + (evs.flatMap F).count a ≤ (storeOwn D H).count a) ∧
      H'.length = H.length ∧ StoreCC D H' := by
  unfold dropRetire at h
  split at h
  · cases h
  · cases h
  · rename_i c hc
    split at h
    · cases h
    · split at h
      · cases h
      · rename_i evs' hd
        cases h
        refine ⟨fun a => ?_, by simp, hcc.set_dead⟩
        have h1 := storeOwn_set_count D a Cell.dead hc
        have h2 := dropCell_measure hF (hcc ℓ c hc) hd a
        simp only [Cell.own] at h1
        simp at h1
        omega

/-- `run-scope-drops` (§6.1), counted (helper). -/
theorem unwindLocs_measure {D : Decls} {F : Event → List Nat} (hF : TraceMeasure D F) :
    ∀ {H H' : Store} {ls : List Nat} {evs : List Event}, StoreCC D H →
      unwindLocs D H ls = .ok (H', evs) →
      (∀ a, (storeOwn D H').count a + (evs.flatMap F).count a ≤ (storeOwn D H).count a) ∧
        H'.length = H.length ∧ StoreCC D H'
  | H, H', [], evs, hcc, h => by
      simp [unwindLocs] at h; obtain ⟨rfl, rfl⟩ := h
      exact ⟨fun a => by simp, rfl, hcc⟩
  | H, H', ℓ :: ls, evs, hcc, h => by
      simp only [unwindLocs] at h
      split at h
      · cases h
      · rename_i H₁ evs₁ h₁
        split at h
        · cases h
        · rename_i H₂ evs₂ h₂
          cases h
          obtain ⟨i₁, l₁, c₁⟩ := dropRetire_measure hF hcc h₁
          obtain ⟨i₂, l₂, c₂⟩ := unwindLocs_measure hF c₁ h₂
          refine ⟨fun a => ?_, by omega, c₂⟩
          have := i₁ a; have := i₂ a
          simp only [List.flatMap_append, List.count_append]
          omega

/-- One residue subtree's drop — its marker (`residueMark`), then §6.11's
walk — counted (helper). -/
theorem residueMark_measure {D : Decls} {F : Event → List Nat} (hF : TraceMeasure D F)
    {ℓ : Nat} {r : Contents} {evs : List Event} (hcc : r.copyClosed D = true)
    (h : dropContents D r = .ok evs) :
    IdLe ((residueMark D ℓ r ++ evs).flatMap F) (r.own D) := by
  unfold residueMark
  split
  · simpa using hF.walk hcc h
  · intro a
    have := hF.marker (ℓ := ℓ) hcc h a
    simpa using this

/-- `drop*` over a destructure's residue (§6.3), counted (helper). -/
theorem dropResidue_measure {D : Decls} {F : Event → List Nat} (hF : TraceMeasure D F)
    {ℓ : Nat} : ∀ {rs : List Contents} {evs : List Event}, Contents.copyClosedList D rs = true →
      dropResidue D ℓ rs = .ok evs → IdLe (evs.flatMap F) (Contents.ownList D rs)
  | [], _, _, h => by simp [dropResidue] at h; subst h; intro a; simp
  | r :: rs, evs, hcc, h => by
      simp only [Contents.copyClosedList, Bool.and_eq_true] at hcc
      simp only [dropResidue] at h
      split at h
      · cases h
      · split at h
        · cases h
        · rename_i e₁ h₁
          split at h
          · cases h
          · rename_i e₂ h₂
            cases h
            intro a
            have := residueMark_measure hF (ℓ := ℓ) hcc.1 h₁ a
            have := dropResidue_measure hF hcc.2 h₂ a
            simp only [List.flatMap_append, List.count_append, Contents.ownList] at *
            omega

/-! ## §6.3's `split`, counted -/

/-- `ownList` distributes over concatenation (helper). -/
theorem Contents.ownList_append (D : Decls) :
    ∀ (l₁ l₂ : List Contents), Contents.ownList D (l₁ ++ l₂)
      = Contents.ownList D l₁ ++ Contents.ownList D l₂
  | [], _ => rfl
  | c :: l₁, l₂ => by simp [Contents.ownList, Contents.ownList_append D l₁ l₂]

/-- `copyClosedList` distributes over concatenation (helper). -/
theorem Contents.copyClosedList_append (D : Decls) :
    ∀ (l₁ l₂ : List Contents), Contents.copyClosedList D (l₁ ++ l₂)
      = (Contents.copyClosedList D l₁ && Contents.copyClosedList D l₂)
  | [], _ => by simp [Contents.copyClosedList]
  | c :: l₁, l₂ => by
      simp [Contents.copyClosedList, Contents.copyClosedList_append D l₁ l₂, Bool.and_assoc]

/-- `allCopyList` distributes over concatenation (helper). -/
theorem Contents.allCopyList_append (D : Decls) :
    ∀ (l₁ l₂ : List Contents), Contents.allCopyList D (l₁ ++ l₂)
      = (Contents.allCopyList D l₁ && Contents.allCopyList D l₂)
  | [], _ => by simp [Contents.allCopyList]
  | c :: l₁, l₂ => by
      simp [Contents.allCopyList, Contents.allCopyList_append D l₁ l₂, Bool.and_assoc]

mutual
/-- `split` of an all-`Copy` contents is all-`Copy` (helper). -/
theorem Contents.splitResidue_allCopy {D : Decls} : ∀ (π : List Nat) {c leaf : Contents}
    {rs : List Contents}, c.allCopy D = true → c.splitResidue D π = .ok (leaf, rs) →
    leaf.allCopy D = true ∧ Contents.allCopyList D rs = true
  | [], c, leaf, rs, h, hs => by
      simp [Contents.splitResidue] at hs; obtain ⟨rfl, rfl⟩ := hs; exact ⟨h, rfl⟩
  | f :: π, c, leaf, rs, h, hs => by
      cases c with
      | struct s i cs =>
          simp only [Contents.allCopy, Bool.and_eq_true] at h
          simp only [Contents.splitResidue] at hs
          exact Contents.splitFields_allCopy cs f π h.2 hs
      | array T i cs =>
          simp only [Contents.allCopy, Bool.and_eq_true] at h
          simp only [Contents.splitResidue] at hs
          exact Contents.splitFields_allCopy cs f π h.2 hs
      | _ => simp [Contents.splitResidue] at hs

/-- The same at `split`'s field step (helper). -/
theorem Contents.splitFields_allCopy {D : Decls} : ∀ (cs : List Contents) (f : Nat)
    (π : List Nat) {leaf : Contents} {rs : List Contents}, Contents.allCopyList D cs = true →
    Contents.splitFields D cs f π = .ok (leaf, rs) →
    leaf.allCopy D = true ∧ Contents.allCopyList D rs = true
  | [], _, _, _, _, _, hs => by simp [Contents.splitFields] at hs
  | c :: cs, 0, π, leaf, rs, h, hs => by
      simp only [Contents.allCopyList, Bool.and_eq_true] at h
      simp only [Contents.splitFields] at hs
      split at hs
      · cases hs
      · rename_i leaf' inner hsr
        cases hs
        obtain ⟨hl, hi⟩ := Contents.splitResidue_allCopy π h.1 hsr
        exact ⟨hl, by rw [Contents.allCopyList_append, hi, h.2]; rfl⟩
  | c :: cs, f + 1, π, leaf, rs, h, hs => by
      simp only [Contents.allCopyList, Bool.and_eq_true] at h
      simp only [Contents.splitFields] at hs
      split at hs
      · cases hs
      · rename_i leaf' rest hsf
        cases hs
        obtain ⟨hl, hr⟩ := Contents.splitFields_allCopy cs f π h.2 hsf
        exact ⟨hl, by simp [Contents.allCopyList, h.1, hr]⟩
end

mutual
/-- **`split`, counted** (§6.3): the leaf and the residue together own at most
what the consumed place owned — the nodes on the path between them are
consumed — and both stay copy-closed (helper). -/
theorem Contents.splitResidue_own {D : Decls} (a : Nat) : ∀ (π : List Nat) {c leaf : Contents}
    {rs : List Contents}, c.copyClosed D = true → c.splitResidue D π = .ok (leaf, rs) →
    (leaf.own D).count a + (Contents.ownList D rs).count a ≤ (c.own D).count a ∧
      leaf.copyClosed D = true ∧ Contents.copyClosedList D rs = true
  | [], c, leaf, rs, h, hs => by
      simp [Contents.splitResidue] at hs; obtain ⟨rfl, rfl⟩ := hs
      exact ⟨by simp [Contents.ownList], h, rfl⟩
  | f :: π, c, leaf, rs, h, hs => by
      cases c with
      | struct s i cs =>
          simp only [Contents.splitResidue] at hs
          simp only [Contents.copyClosed] at h
          split at h
          · rename_i hc
            obtain ⟨hl, hr⟩ := Contents.splitFields_allCopy cs f π h hs
            refine ⟨?_, Contents.allCopy_copyClosed hl, Contents.allCopyList_copyClosedList hr⟩
            simp [Contents.allCopy_own hl, Contents.allCopyList_own hr]
          · rename_i hc
            obtain ⟨hn, hl, hr⟩ := Contents.splitFields_own a cs f π h hs
            refine ⟨?_, hl, hr⟩
            simp only [Contents.own, if_neg hc, List.count_cons]
            omega
      | array T i cs =>
          simp only [Contents.splitResidue] at hs
          simp only [Contents.copyClosed] at h
          split at h
          · rename_i hc
            obtain ⟨hl, hr⟩ := Contents.splitFields_allCopy cs f π h hs
            refine ⟨?_, Contents.allCopy_copyClosed hl, Contents.allCopyList_copyClosedList hr⟩
            simp [Contents.allCopy_own hl, Contents.allCopyList_own hr]
          · rename_i hc
            obtain ⟨hn, hl, hr⟩ := Contents.splitFields_own a cs f π h hs
            refine ⟨?_, hl, hr⟩
            simp only [Contents.own, if_neg hc, List.count_cons]
            omega
      | _ => simp [Contents.splitResidue] at hs

/-- The same at `split`'s field step (helper). -/
theorem Contents.splitFields_own {D : Decls} (a : Nat) : ∀ (cs : List Contents) (f : Nat)
    (π : List Nat) {leaf : Contents} {rs : List Contents},
    Contents.copyClosedList D cs = true → Contents.splitFields D cs f π = .ok (leaf, rs) →
    (leaf.own D).count a + (Contents.ownList D rs).count a
        ≤ (Contents.ownList D cs).count a ∧
      leaf.copyClosed D = true ∧ Contents.copyClosedList D rs = true
  | [], _, _, _, _, _, hs => by simp [Contents.splitFields] at hs
  | c :: cs, 0, π, leaf, rs, h, hs => by
      simp only [Contents.copyClosedList, Bool.and_eq_true] at h
      simp only [Contents.splitFields] at hs
      split at hs
      · cases hs
      · rename_i leaf' inner hsr
        cases hs
        obtain ⟨hn, hl, hi⟩ := Contents.splitResidue_own a π h.1 hsr
        refine ⟨?_, hl, by rw [Contents.copyClosedList_append, hi, h.2]; rfl⟩
        simp only [Contents.ownList_append, Contents.ownList, List.count_append]
        omega
  | c :: cs, f + 1, π, leaf, rs, h, hs => by
      simp only [Contents.copyClosedList, Bool.and_eq_true] at h
      simp only [Contents.splitFields] at hs
      split at hs
      · cases hs
      · rename_i leaf' rest hsf
        cases hs
        obtain ⟨hn, hl, hr⟩ := Contents.splitFields_own a cs f π h.2 hsf
        refine ⟨?_, hl, by simp [Contents.copyClosedList, h.1, hr]⟩
        simp only [Contents.ownList, List.count_append]
        omega
end

/-- `skelFields` keeps the member count (helper). -/
theorem Contents.skelFields_length : ∀ (cs : List Contents) (f : Nat) (π : List Nat),
    (Contents.skelFields cs f π).length = cs.length
  | [], _, _ => by simp [Contents.skelFields]
  | c :: cs, 0, π => by simp [Contents.skelFields]
  | c :: cs, f + 1, π => by simp [Contents.skelFields, Contents.skelFields_length cs f π]

/-- A list of `⊘`s owns nothing (helper). -/
theorem Contents.ownList_holes {α : Type} (D : Decls) :
    ∀ cs : List α, Contents.ownList D (cs.map fun _ => .hole) = []
  | [] => rfl
  | _ :: cs => by simp [Contents.ownList, Contents.own, Contents.ownList_holes D cs]

mutual
/-- **`split` and the consumed shell, counted exactly** (§6.3, RUE-2427): the
leaf, the residue and the path's shell (`Contents.skeleton`) together own
exactly what the consumed place owned — every owned node of it is in exactly
one of the three (helper). -/
theorem Contents.skeleton_own {D : Decls} (a : Nat) : ∀ (π : List Nat) {c leaf : Contents}
    {rs : List Contents}, c.copyClosed D = true → c.splitResidue D π = .ok (leaf, rs) →
    (leaf.own D).count a + (Contents.ownList D rs).count a + ((c.skeleton π).own D).count a
      = (c.own D).count a
  | [], c, leaf, rs, _, hs => by
      simp [Contents.splitResidue] at hs; obtain ⟨rfl, rfl⟩ := hs
      simp [Contents.skeleton, Contents.own, Contents.ownList]
  | f :: π, c, leaf, rs, h, hs => by
      cases c with
      | struct s i cs =>
          simp only [Contents.splitResidue] at hs
          simp only [Contents.copyClosed] at h
          split at h
          · rename_i hc
            obtain ⟨hl, hr⟩ := Contents.splitFields_allCopy cs f π h hs
            simp [Contents.skeleton, Contents.own, hc, Contents.allCopy_own hl,
              Contents.allCopyList_own hr]
          · rename_i hc
            have := Contents.skelFields_own a cs f π h hs
            simp only [Contents.skeleton, Contents.own, if_neg hc, List.count_cons]
            omega
      | array T i cs =>
          simp only [Contents.splitResidue] at hs
          simp only [Contents.copyClosed] at h
          split at h
          · rename_i hc
            obtain ⟨hl, hr⟩ := Contents.splitFields_allCopy cs f π h hs
            simp [Contents.skeleton, Contents.own, Contents.skelFields_length, hc,
              Contents.allCopy_own hl, Contents.allCopyList_own hr]
          · rename_i hc
            have := Contents.skelFields_own a cs f π h hs
            simp only [Contents.skeleton, Contents.own, Contents.skelFields_length, if_neg hc,
              List.count_cons]
            omega
      | _ => simp [Contents.splitResidue] at hs

/-- The same at `split`'s member step (helper). -/
theorem Contents.skelFields_own {D : Decls} (a : Nat) : ∀ (cs : List Contents) (f : Nat)
    (π : List Nat) {leaf : Contents} {rs : List Contents},
    Contents.copyClosedList D cs = true → Contents.splitFields D cs f π = .ok (leaf, rs) →
    (leaf.own D).count a + (Contents.ownList D rs).count a
        + (Contents.ownList D (Contents.skelFields cs f π)).count a
      = (Contents.ownList D cs).count a
  | [], _, _, _, _, _, hs => by simp [Contents.splitFields] at hs
  | c :: cs, 0, π, leaf, rs, h, hs => by
      simp only [Contents.copyClosedList, Bool.and_eq_true] at h
      simp only [Contents.splitFields] at hs
      split at hs
      · cases hs
      · rename_i leaf' inner hsr
        cases hs
        have := Contents.skeleton_own a π h.1 hsr
        simp only [Contents.skelFields, Contents.ownList, Contents.ownList_append,
          Contents.ownList_holes, List.count_append, List.count_nil]
        omega
  | c :: cs, f + 1, π, leaf, rs, h, hs => by
      simp only [Contents.copyClosedList, Bool.and_eq_true] at h
      simp only [Contents.splitFields] at hs
      split at hs
      · cases hs
      · rename_i leaf' rest hsf
        cases hs
        have := Contents.skelFields_own a cs f π h.2 hsf
        simp only [Contents.skelFields, Contents.ownList, Contents.own, List.count_append,
          List.count_nil, List.nil_append]
        omega
end

/-- **§6.3's `destructure`, counted**: the leaf it hands on, the residue drops
it runs and the shell it consumes together account for at most what the
consumed place owned (helper). -/
theorem Contents.destructure_measure {D : Decls} {F : Event → List Nat} (hF : TraceMeasure D F)
    {ℓ : Nat} {cd leaf : Contents} {πs : List Nat} {evs : List Event}
    (hcc : cd.copyClosed D = true) (h : cd.destructure D ℓ πs = .ok (leaf, evs)) :
    (∀ a, (leaf.own D).count a + (evs.flatMap F).count a ≤ (cd.own D).count a) ∧
      leaf.copyClosed D = true := by
  unfold Contents.destructure at h
  split at h
  · cases h
  · rename_i leaf' rs hs
    split at h
    · cases h
    · rename_i evs' hd
      cases h
      have hl := (Contents.splitResidue_own 0 πs hcc hs).2.1
      have hr := (Contents.splitResidue_own 0 πs hcc hs).2.2
      refine ⟨fun a => ?_, hl⟩
      have := Contents.skeleton_own a πs hcc hs
      have := dropResidue_measure hF hr hd a
      have := hF.consume (cd.skeleton πs) a
      simp only [List.flatMap_append, List.flatMap_cons, List.flatMap_nil, List.append_nil,
        List.count_append] at *
      omega

/-! ## The conservation law's promise about one evaluation -/

/-- `Fresh` over three stores that only grew is the two ranges back to back
(helper). -/
theorem Fresh.count_trans {H H₁ H₂ : Store} (h₁ : H.length ≤ H₁.length)
    (h₂ : H₁.length ≤ H₂.length) (a : Nat) :
    (Fresh H H₂).count a = (Fresh H H₁).count a + (Fresh H₁ H₂).count a := by
  have : Fresh H H₂ = Fresh H H₁ ++ Fresh H₁ H₂ := by
    simp only [Fresh]
    have := (List.range'_append (s := H.length) (m := H₁.length - H.length)
      (n := H₂.length - H₁.length) (step := 1))
    rw [show H.length + 1 * (H₁.length - H.length) = H₁.length by omega,
      show H₁.length - H.length + (H₂.length - H₁.length) = H₂.length - H.length by omega]
      at this
    exact this.symm
  rw [this, List.count_append]

/-- A trap's range composes the same way (helper). -/
theorem Fresh.count_trans_range {H H₁ : Store} (h₁ : H.length ≤ H₁.length) (N a : Nat) :
    (List.range' H.length (H₁.length - H.length + N)).count a
      = (Fresh H H₁).count a + (List.range' H₁.length N).count a := by
  have := (List.range'_append (s := H.length) (m := H₁.length - H.length) (n := N) (step := 1))
  rw [show H.length + 1 * (H₁.length - H.length) = H₁.length by omega] at this
  rw [← this, List.count_append]; rfl

/-- Nothing was minted between a store and itself (helper). -/
@[simp] theorem Fresh.self (H : Store) : Fresh H H = [] := by simp [Fresh]

/-- One reserved slot mints one identity, the old length (helper). -/
theorem Fresh.snoc (H : Store) (c : Cell) : Fresh H (H ++ [c]) = [H.length] := by
  simp [Fresh]

/-- **The conservation law, for one evaluation** from store `H`, holding the
owned identities `X` besides it (a pending operand's value): the result's
store, its value and the trace's projection `F` together own at most what `H`
and `X` owned, plus what was minted on the way — each identity counted, as a
multiset. The store and the value stay copy-closed. A trap carries no store,
so its minted range is existential; a refusal and exhausted fuel promise
nothing (helper). -/
def Cons (D : Decls) (F : Event → List Nat) (H : Store) (X : List Nat) : EvalRes → Prop
  | .ok H' v tr | .returned H' v tr =>
      H.length ≤ H'.length ∧ StoreCC D H' ∧ (Contents.ofVal v).copyClosed D = true ∧
      ∀ a, (storeOwn D H').count a + (v.own D).count a + (tr.flatMap F).count a
        ≤ (storeOwn D H).count a + X.count a + (Fresh H H').count a
  | .broke H' _ tr =>
      H.length ≤ H'.length ∧ StoreCC D H' ∧
      ∀ a, (storeOwn D H').count a + (tr.flatMap F).count a
        ≤ (storeOwn D H).count a + X.count a + (Fresh H H').count a
  | .panic _ tr =>
      ∃ N, ∀ a, (tr.flatMap F).count a
        ≤ (storeOwn D H).count a + X.count a + (List.range' H.length N).count a
  | .stuck _ | .outOfFuel => True

/-- Holding more does not break the law (helper). -/
theorem Cons.weaken {D : Decls} {F : Event → List Nat} {H : Store} {X Y : List Nat}
    {r : EvalRes} (h : Cons D F H X r) (hXY : ∀ a, X.count a ≤ Y.count a) : Cons D F H Y r := by
  cases r with
  | ok H' v tr =>
      obtain ⟨h1, h2, h3, h4⟩ := h
      exact ⟨h1, h2, h3, fun a => by have := h4 a; have := hXY a; omega⟩
  | returned H' v tr =>
      obtain ⟨h1, h2, h3, h4⟩ := h
      exact ⟨h1, h2, h3, fun a => by have := h4 a; have := hXY a; omega⟩
  | broke H' sc tr =>
      obtain ⟨h1, h2, h4⟩ := h
      exact ⟨h1, h2, fun a => by have := h4 a; have := hXY a; omega⟩
  | panic k tr =>
      obtain ⟨N, h4⟩ := h
      exact ⟨N, fun a => by have := h4 a; have := hXY a; omega⟩
  | stuck w => trivial
  | outOfFuel => trivial

/-- **Composition**: an evaluation from `H₁` whose own law holds, run after a
step from `H` to `H₁` that emitted `tr` and left `Y` held, satisfies the law
from `H` with the step's trace prefixed — §6.2's search, as a ledger
(helper). -/
theorem Cons.prefix {D : Decls} {F : Event → List Nat} {H H₁ : Store} {X Y : List Nat}
    {tr : List Event} {r : EvalRes} (hle : H.length ≤ H₁.length)
    (hI : ∀ a, (storeOwn D H₁).count a + Y.count a + (tr.flatMap F).count a
      ≤ (storeOwn D H).count a + X.count a + (Fresh H H₁).count a)
    (hr : Cons D F H₁ Y r) : Cons D F H X (r.withTrace tr) := by
  cases r with
  | ok H₂ v tr₂ =>
      obtain ⟨h1, h2, h3, h4⟩ := hr
      refine ⟨Nat.le_trans hle h1, h2, h3, fun a => ?_⟩
      have := hI a; have := h4 a; have := Fresh.count_trans hle h1 a
      simp only [List.flatMap_append, List.count_append]
      omega
  | returned H₂ v tr₂ =>
      obtain ⟨h1, h2, h3, h4⟩ := hr
      refine ⟨Nat.le_trans hle h1, h2, h3, fun a => ?_⟩
      have := hI a; have := h4 a; have := Fresh.count_trans hle h1 a
      simp only [List.flatMap_append, List.count_append]
      omega
  | broke H₂ sc tr₂ =>
      obtain ⟨h1, h2, h4⟩ := hr
      refine ⟨Nat.le_trans hle h1, h2, fun a => ?_⟩
      have := hI a; have := h4 a; have := Fresh.count_trans hle h1 a
      simp only [List.flatMap_append, List.count_append]
      omega
  | panic k tr₂ =>
      obtain ⟨N, h4⟩ := hr
      refine ⟨H₁.length - H.length + N, fun a => ?_⟩
      have := hI a; have := h4 a; have := Fresh.count_trans_range hle N a
      simp only [List.flatMap_append, List.count_append]
      omega
  | stuck w => trivial
  | outOfFuel => trivial

/-- A step that emitted nothing: the law transports back along it (helper). -/
theorem Cons.shift {D : Decls} {F : Event → List Nat} {H H₁ : Store} {X Y : List Nat}
    {r : EvalRes} (hle : H.length ≤ H₁.length)
    (hI : ∀ a, (storeOwn D H₁).count a + Y.count a
      ≤ (storeOwn D H).count a + X.count a + (Fresh H H₁).count a)
    (hr : Cons D F H₁ Y r) : Cons D F H X r := by
  have := Cons.prefix (tr := []) hle (fun a => by simpa using hI a) hr
  cases r <;> simpa [EvalRes.withTrace] using this

/-- **§6.2's search, as a ledger**: an operand that keeps the law, sequenced
into a context that keeps it holding the operand's value, keeps it
(helper). -/
theorem Cons.bind {D : Decls} {F : Event → List Nat} {H : Store} {X : List Nat}
    {r : EvalRes} {k : Store → Val → EvalRes} (hr : Cons D F H X r)
    (hk : ∀ H₁ v tr, r = .ok H₁ v tr → StoreCC D H₁ → (Contents.ofVal v).copyClosed D = true →
      Cons D F H₁ (v.own D) (k H₁ v)) :
    Cons D F H X (r.andThen k) := by
  cases r with
  | ok H₁ v tr =>
      obtain ⟨h1, h2, h3, h4⟩ := hr
      exact Cons.prefix h1 (fun a => by have := h4 a; omega) (hk H₁ v tr rfl h2 h3)
  | returned H₁ v tr => exact hr
  | broke H₁ sc tr => exact hr
  | panic k tr => exact hr
  | stuck w => trivial
  | outOfFuel => trivial

/-- §6.9's call boundary, as a ledger: the same, with an unwinding `return`
becoming the call's value (helper). -/
theorem Cons.absorb {D : Decls} {F : Event → List Nat} {H : Store} {X : List Nat}
    {r : EvalRes} {k : Store → Val → EvalRes} (hr : Cons D F H X r)
    (hk : ∀ H₁ v tr, r = .ok H₁ v tr → StoreCC D H₁ → (Contents.ofVal v).copyClosed D = true →
      Cons D F H₁ (v.own D) (k H₁ v)) :
    Cons D F H X (r.absorb k) := by
  cases r with
  | ok H₁ v tr =>
      obtain ⟨h1, h2, h3, h4⟩ := hr
      exact Cons.prefix h1 (fun a => by have := h4 a; omega) (hk H₁ v tr rfl h2 h3)
  | returned H₁ v tr => exact hr
  | broke H₁ sc tr => trivial
  | panic k tr => exact hr
  | stuck w => trivial
  | outOfFuel => trivial

/-- A value produced where the store is, owning nothing held (helper). -/
theorem Cons.pure {D : Decls} {F : Event → List Nat} {H : Store} {X : List Nat} {v : Val}
    (hcc : StoreCC D H) (hv : (Contents.ofVal v).copyClosed D = true)
    (ho : ∀ a, (v.own D).count a ≤ X.count a) : Cons D F H X (.ok H v []) :=
  ⟨Nat.le_refl _, hcc, hv, fun a => by have := ho a; simp; omega⟩

/-- A scalar owns nothing and is copy-closed (helper). -/
def Val.scalar : Val → Prop
  | .int _ _ _ | .float _ _ | .bool _ | .unit => True
  | _ => False

/-- A scalar's ledger (helper). -/
theorem Val.scalar_own {D : Decls} {v : Val} (h : v.scalar) :
    v.own D = [] ∧ (Contents.ofVal v).copyClosed D = true := by
  cases v with
  | int => exact ⟨rfl, rfl⟩
  | float => exact ⟨rfl, rfl⟩
  | bool => exact ⟨rfl, rfl⟩
  | unit => exact ⟨rfl, rfl⟩
  | _ => simp [Val.scalar] at h

/-- A scalar produced where the store is keeps the law (helper). -/
theorem Cons.scalar {D : Decls} {F : Event → List Nat} {H : Store} {X : List Nat} {v : Val}
    (hcc : StoreCC D H) (hs : v.scalar) : Cons D F H X (.ok H v []) :=
  Cons.pure hcc (Val.scalar_own hs).2 (fun a => by simp [(Val.scalar_own (D := D) hs).1])

/-- An operator's outcome keeps the law when its value is a scalar (helper). -/
theorem Cons.opRes {D : Decls} {F : Event → List Nat} {H : Store} {X : List Nat} {o : OpRes}
    (hcc : StoreCC D H) (hs : ∀ v, o = .val v → v.scalar) : Cons D F H X (o.toRes H) := by
  cases o with
  | val v =>
      obtain ⟨h1, h2⟩ := Val.scalar_own (D := D) (hs v rfl)
      exact Cons.pure hcc h2 (fun a => by simp [h1])
  | trap k => exact ⟨0, fun a => by simp⟩
  | confused => trivial

/-! ## The forms' own ledgers -/
/-- `range_check`'s value is a scalar (helper). -/
theorem intResult_scalar {w s n v} (h : intResult w s n = .val v) : v.scalar := by
  unfold intResult at h; split at h <;> cases h; trivial

/-- §6.4's integer rules produce a scalar (helper). -/
theorem binOpInt_scalar {op w s n₁ n₂ v} (h : binOpInt op w s n₁ n₂ = .val v) : v.scalar := by
  unfold binOpInt at h
  split at h <;> (repeat' split at h) <;> first | exact intResult_scalar h | (cases h; trivial) | cases h

/-- §6.4's float rules produce a scalar (helper). -/
theorem binOpFloat_scalar {M op w a b v} (h : binOpFloat M op w a b = .val v) : v.scalar := by
  unfold binOpFloat at h
  split at h <;> first | (cases h; trivial) | cases h

/-- §6.4's binary operators produce a scalar (helper). -/
theorem evalBinOp_scalar {M op a b v} (h : evalBinOp M op a b = .val v) : v.scalar := by
  unfold evalBinOp at h
  split at h
  · split at h
    · exact binOpInt_scalar h
    · cases h
  · split at h
    · exact binOpFloat_scalar h
    · cases h
  · cases h

/-- §6.4's unary operators produce a scalar (helper). -/
theorem evalUnOp_scalar {op a v} (h : evalUnOp op a = .val v) : v.scalar := by
  unfold evalUnOp at h
  split at h <;> first | exact intResult_scalar h | (cases h; trivial) | cases h

/-- `@intCast` produces a scalar (helper). -/
theorem evalIntCast_scalar {w s a v} (h : evalIntCast w s a = .val v) : v.scalar := by
  unfold evalIntCast at h
  split at h
  · split at h <;> first | (cases h; trivial) | cases h
  · cases h

/-- §6.4's float intrinsics produce a scalar (helper). -/
theorem evalFintrin_scalar {M k a v} (h : evalFintrin M k a = .val v) : v.scalar := by
  unfold evalFintrin at h
  split at h <;> (repeat' split at h) <;> first | (cases h; trivial) | cases h

/-- A value's stored image has the value's class (helper). -/
theorem Contents.mult_ofVal (D : Decls) (v : Val) : (Contents.ofVal v).mult D = v.mult D := by
  have h : ∀ vs : List Val, (Contents.ofVals vs).length = vs.length := by
    intro vs; induction vs <;> simp_all [Contents.ofVals]
  cases v <;> simp [Contents.ofVal, Contents.mult, Val.mult, h]

/-- A `Copy` value owns nothing (helper). -/
theorem Val.own_of_copy {D : Decls} {v : Val} (h : v.mult D = .copy) : v.own D = [] :=
  Contents.own_of_mult (by rw [Contents.mult_ofVal]; exact h)

/-- `ownList` over `ofVals` of a cons (helper). -/
theorem Contents.ownList_ofVals_cons (D : Decls) (v : Val) (vs : List Val) :
    Contents.ownList D (Contents.ofVals (v :: vs)) = v.own D ++ Contents.ownList D (Contents.ofVals vs) :=
  rfl

/-- `n` copies of a `Copy` value own nothing (helper). -/
theorem Contents.ownList_replicate {D : Decls} {v : Val} (h : v.mult D = .copy) :
    ∀ n, Contents.ownList D (Contents.ofVals (List.replicate n v)) = []
  | 0 => rfl
  | n + 1 => by
      simp only [List.replicate_succ, Contents.ownList_ofVals_cons, Val.own_of_copy h,
        Contents.ownList_replicate h n, List.nil_append]

/-- The store `mintParams` leaves owns the store's identities and the
arguments' (§6.9's (D-Call), §6.6's (D-Match)) (helper). -/
theorem storeOwn_mintParams (D : Decls) (H : Store) (vs : List Val) :
    storeOwn D (mintParams H vs).1 = storeOwn D H ++ Contents.ownList D (Contents.ofVals vs) := by
  rw [mintParams_store, storeOwn_append]
  congr 1
  induction vs with
  | nil => rfl
  | cons v vs ih =>
      simp only [List.map_cons, storeOwn, List.flatMap_cons, Cell.own] at ih ⊢
      rw [ih]; rfl

/-- The cells `mintParams` adds are copy-closed when the arguments are
(helper). -/
theorem StoreCC.mintParams {D : Decls} {H : Store} {vs : List Val} (h : StoreCC D H)
    (hv : Contents.copyClosedList D (Contents.ofVals vs) = true) :
    StoreCC D (RueCore.mintParams H vs).1 := by
  rw [mintParams_store]
  refine h.append ?_
  induction vs with
  | nil => intro ℓ c hc; simp at hc
  | cons v vs ih =>
      simp only [Contents.ofVals, Contents.copyClosedList, Bool.and_eq_true] at hv
      intro ℓ c hc
      cases ℓ with
      | zero => simp at hc; subst hc; exact hv.1
      | succ ℓ => simp only [List.map_cons, List.getElem?_cons_succ] at hc; exact ih hv.2 ℓ c hc

/-- The store `mintParams` leaves is the old one grown (helper). -/
theorem mintParams_length (H : Store) (vs : List Val) :
    (mintParams H vs).1.length = H.length + vs.length := by
  rw [mintParams_store]; simp

/-- An aggregate owns at most its members' identities and its own (helper). -/
theorem Contents.own_struct_le (D : Decls) (s i : Nat) (cs : List Contents) (a : Nat) :
    ((Contents.struct s i cs).own D).count a ≤ (Contents.ownList D cs).count a + [i].count a := by
  simp only [Contents.own]; split <;> simp [List.count_cons] <;> omega

/-- The same at an enum (helper). -/
theorem Contents.own_enum_le (D : Decls) (e k i : Nat) (cs : List Contents) (a : Nat) :
    ((Contents.enum e k i cs).own D).count a ≤ (Contents.ownList D cs).count a + [i].count a := by
  simp only [Contents.own]; split <;> simp [List.count_cons] <;> omega

/-- The same at an array (helper). -/
theorem Contents.own_array_le (D : Decls) (T : Ty) (i : Nat) (cs : List Contents) (a : Nat) :
    ((Contents.array T i cs).own D).count a ≤ (Contents.ownList D cs).count a + [i].count a := by
  simp only [Contents.own]; split <;> simp [List.count_cons] <;> omega

/-- An enum owns at least its payload, and a copy-closed one's payload is
copy-closed — what (D-Match) §6.6 hands the arm's cells (helper). -/
theorem Contents.enum_payload {D : Decls} {e k i : Nat} {cs : List Contents}
    (h : (Contents.enum e k i cs).copyClosed D = true) (a : Nat) :
    (Contents.ownList D cs).count a ≤ ((Contents.enum e k i cs).own D).count a ∧
      Contents.copyClosedList D cs = true := by
  simp only [Contents.copyClosed] at h
  split at h
  · rename_i hc
    refine ⟨by simp [Contents.allCopyList_own h], Contents.allCopyList_copyClosedList h⟩
  · rename_i hc
    refine ⟨by simp [Contents.own, hc, List.count_cons], h⟩

/-- **(D-Match)'s consumption, counted** (RUE-2427): the payload the arm's cells
receive and the shell `matchConsume` ends together account for at most the
scrutinee — the payload moves, the shell ends, nothing is duplicated
(helper). -/
theorem matchConsume_measure {D : Decls} {F : Event → List Nat} (hF : TraceMeasure D F)
    {e k i : Nat} {vs : List Val} (h : (Contents.enum e k i (Contents.ofVals vs)).copyClosed D = true)
    (a : Nat) :
    (Contents.ownList D (Contents.ofVals vs)).count a + ((matchConsume D e k i vs).flatMap F).count a
      ≤ ((Contents.enum e k i (Contents.ofVals vs)).own D).count a := by
  unfold matchConsume
  simp only [Contents.copyClosed] at h
  split
  · rename_i hc
    rw [if_pos hc] at h
    simp [Contents.allCopyList_own h]
  · rename_i hc
    have := hF.consume (.enum e k i (vs.map fun _ => .hole)) a
    simp only [Contents.own, if_neg hc, Contents.ownList_holes, List.count_cons,
      List.count_nil] at this ⊢
    simp only [List.flatMap_cons, List.flatMap_nil, List.append_nil]
    omega

/-- **Aggregate introduction, as a ledger** (`introVal`): the new value owns at
most its members and the one identity just minted, which is the one index the
store grew by (helper). -/
theorem Cons.intro {D : Decls} {F : Event → List Nat} {H : Store} {Y : List Nat}
    {mk : Nat → Val} (hcc : StoreCC D H)
    (ho : ∀ a, ((mk H.length).own D).count a ≤ Y.count a + [H.length].count a) :
    Cons D F H Y (RueCore.introVal D H mk) := by
  unfold RueCore.introVal
  split
  · rename_i hv
    refine ⟨by simp, hcc.append StoreCC.dead, hv, fun a => ?_⟩
    have := ho a
    rw [storeOwn_append, Fresh.snoc]
    simp [storeOwn, Cell.own]
    omega
  · trivial

/-- What `dynPlace` lands on is a live cell and a read of it (helper). -/
theorem dynPlace_at {H : Store} {φ : Frame} {p : Place} {vs : List Val} {πs : List (List Nat)}
    {ℓ : Nat} {c sub : Contents} {ρ : List Nat} (h : dynPlace H φ p vs πs = .at ℓ c sub ρ) :
    H[ℓ]? = some (.full c) ∧ c.readAt p.path = .ok sub := by
  unfold dynPlace at h
  split at h
  · cases h
  · split at h
    · cases h
    · rename_i ℓ' hρ
      split at h
      · cases h
      · cases h
      · rename_i c' hc
        split at h
        · cases h
        · rename_i sub' hr
          split at h
          · cases h; exact ⟨hc, hr⟩
          · cases h
          · cases h

/-- The law's promise about an argument list (helper). -/
def ArgsCons (D : Decls) (F : Event → List Nat) (H : Store) : ArgsRes → Prop
  | .ok H' vs tr =>
      H.length ≤ H'.length ∧ StoreCC D H' ∧
      Contents.copyClosedList D (Contents.ofVals vs) = true ∧
      ∀ a, (storeOwn D H').count a + (Contents.ownList D (Contents.ofVals vs)).count a +
          (tr.flatMap F).count a ≤ (storeOwn D H).count a + (Fresh H H').count a
  | .abort r => Cons D F H [] r

/-- **An argument list keeps the law** (§6.2's left-to-right search through
`g(v̄, …, E, …)`, `S{ v̄, …, E, … }` and `[ v̄, …, E, … ]`): each argument's
value is held while the next one runs, and an abort abandons the ones already
built — which only loses identities, never duplicates one (helper). -/
theorem evalArgs_cons {D : Decls} {F : Event → List Nat} {ev : Store → Expr → EvalRes}
    (hev : ∀ H e, StoreCC D H → Cons D F H [] (ev H e)) :
    ∀ (H : Store) (es : List Expr), StoreCC D H → ArgsCons D F H (evalArgs ev H es)
  | H, [], hcc => ⟨Nat.le_refl _, hcc, rfl, fun a => by simp [Contents.ofVals, Contents.ownList]⟩
  | H, e :: es, hcc => by
      simp only [evalArgs]
      have h₁ := hev H e hcc
      cases hr : ev H e with
      | ok H₁ v tr =>
          rw [hr] at h₁
          obtain ⟨l₁, c₁, v₁, i₁⟩ := h₁
          have h₂ := evalArgs_cons hev H₁ es c₁
          dsimp only
          cases hra : evalArgs ev H₁ es with
          | ok H₂ vs tr₂ =>
              rw [hra] at h₂
              dsimp only
              obtain ⟨l₂, c₂, v₂, i₂⟩ := h₂
              refine ⟨Nat.le_trans l₁ l₂, c₂, ?_, fun a => ?_⟩
              · simp [Contents.ofVals, Contents.copyClosedList, v₁, v₂]
              · have := i₁ a; have := i₂ a; have := Fresh.count_trans l₁ l₂ a
                simp only [Contents.ownList_ofVals_cons, List.flatMap_append, List.count_append]
                simp only [List.count_nil] at *
                omega
          | abort r =>
              rw [hra] at h₂
              dsimp only
              exact Cons.prefix (Y := []) l₁ (fun a => by have := i₁ a; simp at *; omega) h₂
      | returned H₁ v tr => rw [hr] at h₁; exact h₁
      | broke H₁ sc tr => rw [hr] at h₁; exact h₁
      | panic k tr => rw [hr] at h₁; exact h₁
      | stuck w => trivial
      | outOfFuel => trivial

/-! ## The place forms' ledgers: a move, a drop, a write -/

/-- Writing a cell mints nothing (helper). -/
@[simp] theorem Fresh.set (H : Store) (ℓ : Nat) (x : Cell) : Fresh H (H.set ℓ x) = [] := by
  simp [Fresh]

/-- `()` owns nothing (helper). -/
@[simp] theorem Val.own_unit (D : Decls) : Val.unit.own D = [] := rfl

/-- **(D-Use-Move) §6.3, as a ledger**: the value handed on owns what the
place owned, and the place now holds `⊘` — so the identity moved, it did not
multiply (helper). -/
theorem Cons.move {D : Decls} {F : Event → List Nat} {H : Store} {X : List Nat} {ℓ : Nat}
    {c c' sub : Contents} {π : List Nat} {v : Val} (hcc : StoreCC D H)
    (hc : H[ℓ]? = some (.full c)) (hr : c.readAt π = .ok sub)
    (hw : c.writeAt π .hole = some c') (hv : sub.toVal = some v) :
    Cons D F H X (.ok (H.set ℓ (.full c')) v []) := by
  have hccc := hcc ℓ c hc
  have hsub : Contents.ofVal v = sub := Contents.ofVal_toVal hv
  refine ⟨by simp, hcc.set (Contents.writeAt_copyClosed π hccc rfl hw), ?_, fun a => ?_⟩
  · rw [hsub]; exact Contents.readAt_copyClosed π hccc hr
  · have h1 := storeOwn_set_count D a (.full c') hc
    have h2 := Contents.writeAt_own a π hccc hr hw
    simp only [Cell.own, Contents.own, List.count_nil] at h1 h2
    simp only [Val.own, hsub, List.flatMap_nil, List.count_nil]
    omega

/-- **(D-Use-Declared-Linear) §6.3, as a ledger**: the leaf handed on and the
residue dropped together account for the consumed place, which becomes `⊘`
(helper). -/
theorem Cons.destructure {D : Decls} {F : Event → List Nat} (hF : TraceMeasure D F)
    {H : Store} {X : List Nat} {ℓ : Nat} {c c' cd leaf : Contents} {πd πs : List Nat}
    {v : Val} {evs : List Event} (hcc : StoreCC D H) (hc : H[ℓ]? = some (.full c))
    (hr : c.readAt πd = .ok cd) (hd : cd.destructure D ℓ πs = .ok (leaf, evs))
    (hv : leaf.toVal = some v) (hw : c.writeAt πd .hole = some c') :
    Cons D F H X (.ok (H.set ℓ (.full c')) v evs) := by
  have hccc := hcc ℓ c hc
  have hcd := Contents.readAt_copyClosed πd hccc hr
  obtain ⟨hm, hl⟩ := Contents.destructure_measure hF hcd hd
  have hsub : Contents.ofVal v = leaf := Contents.ofVal_toVal hv
  refine ⟨by simp, hcc.set (Contents.writeAt_copyClosed πd hccc rfl hw), by rw [hsub]; exact hl,
    fun a => ?_⟩
  have h1 := storeOwn_set_count D a (.full c') hc
  have h2 := Contents.writeAt_own a πd hccc hr hw
  have h3 := hm a
  simp only [Cell.own, Contents.own, List.count_nil] at h1 h2
  simp only [Val.own, hsub]
  omega

/-- **§6.11's `@drop`, as a ledger**: the place's residue is dropped and the
place becomes `⊘`, so what the trace frees leaves the store (helper). -/
theorem Cons.dropPlace {D : Decls} {F : Event → List Nat} (hF : TraceMeasure D F)
    {H : Store} {X : List Nat} {ℓ : Nat} {c c' sub : Contents} {π : List Nat}
    {evs : List Event} (hcc : StoreCC D H) (hc : H[ℓ]? = some (.full c))
    (hr : c.readAt π = .ok sub) (hd : dropCell D ℓ sub = .ok evs)
    (hw : c.writeAt π .hole = some c') :
    Cons D F H X (.ok (H.set ℓ (.full c')) .unit evs) := by
  have hccc := hcc ℓ c hc
  have hsub := Contents.readAt_copyClosed π hccc hr
  refine ⟨by simp, hcc.set (Contents.writeAt_copyClosed π hccc rfl hw), rfl, fun a => ?_⟩
  have h1 := storeOwn_set_count D a (.full c') hc
  have h2 := Contents.writeAt_own a π hccc hr hw
  have h3 := dropCell_measure hF hsub hd a
  simp only [Cell.own, Contents.own, List.count_nil] at h1 h2
  simp only [Val.own, Contents.ofVal, Contents.own, List.count_nil]
  omega

/-- **§6.11's `@drop` at a declared plan, as a ledger**: the residue, then the
leaf, then `⊘` at the consumed place (helper). -/
theorem Cons.dropDeclared {D : Decls} {F : Event → List Nat} (hF : TraceMeasure D F)
    {H : Store} {X : List Nat} {ℓ : Nat} {c c' cd leaf : Contents} {πd πs : List Nat}
    {evs levs : List Event} (hcc : StoreCC D H) (hc : H[ℓ]? = some (.full c))
    (hr : c.readAt πd = .ok cd) (hd : cd.destructure D ℓ πs = .ok (leaf, evs))
    (hl : dropCell D ℓ leaf = .ok levs) (hw : c.writeAt πd .hole = some c') :
    Cons D F H X (.ok (H.set ℓ (.full c')) .unit (evs ++ levs)) := by
  have hccc := hcc ℓ c hc
  have hcd := Contents.readAt_copyClosed πd hccc hr
  obtain ⟨hm, hlc⟩ := Contents.destructure_measure hF hcd hd
  refine ⟨by simp, hcc.set (Contents.writeAt_copyClosed πd hccc rfl hw), rfl, fun a => ?_⟩
  have h1 := storeOwn_set_count D a (.full c') hc
  have h2 := Contents.writeAt_own a πd hccc hr hw
  have h3 := hm a
  have h4 := dropCell_measure hF hlc hl a
  simp only [Cell.own, Contents.own, List.count_nil] at h1 h2
  simp only [Val.own, Contents.ofVal, Contents.own, List.count_nil, List.flatMap_append,
    List.count_append]
  omega

/-- **(D-Assign) §6.8, as a ledger**: the old contents at the place is dropped
and the held value takes its position (helper). -/
theorem Cons.assign {D : Decls} {F : Event → List Nat} (hF : TraceMeasure D F)
    {H : Store} {Y : List Nat} {ℓ : Nat} {c c' old : Contents} {π : List Nat} {v : Val}
    {evs : List Event} (hcc : StoreCC D H) (hc : H[ℓ]? = some (.full c))
    (hr : c.readAt π = .ok old) (hd : dropCell D ℓ old = .ok evs)
    (hw : c.writeAt π (Contents.ofVal v) = some c') (hc' : c'.copyClosed D = true) :
    Cons D F H (v.own D ++ Y) (.ok (H.set ℓ (.full c')) .unit evs) := by
  have hccc := hcc ℓ c hc
  have hold := Contents.readAt_copyClosed π hccc hr
  refine ⟨by simp, hcc.set hc', rfl, fun a => ?_⟩
  have h1 := storeOwn_set_count D a (.full c') hc
  have h2 := Contents.writeAt_own a π hccc hr hw
  have h3 := dropCell_measure hF hold hd a
  simp only [Cell.own] at h1
  simp only [List.count_append, Fresh.set, Val.own_unit, List.count_nil]
  simp only [Val.own] at *
  omega

/-- **(D-Assign) below a dynamic index, as a ledger**: the same, at the leaf
the index resolved to, one level down (helper). -/
theorem Cons.assignDyn {D : Decls} {F : Event → List Nat} (hF : TraceMeasure D F)
    {H : Store} {Y : List Nat} {ℓ : Nat} {c c' sub sub' old : Contents} {π ρ : List Nat}
    {v : Val} {evs : List Event} (hcc : StoreCC D H) (hc : H[ℓ]? = some (.full c))
    (hr : c.readAt π = .ok sub) (hr' : sub.readAt ρ = .ok old)
    (hd : dropCell D ℓ old = .ok evs) (hw' : sub.writeAt ρ (Contents.ofVal v) = some sub')
    (hw : c.writeAt π sub' = some c') (hc' : c'.copyClosed D = true) :
    Cons D F H (v.own D ++ Y) (.ok (H.set ℓ (.full c')) .unit evs) := by
  have hccc := hcc ℓ c hc
  have hsub := Contents.readAt_copyClosed π hccc hr
  have hold := Contents.readAt_copyClosed ρ hsub hr'
  refine ⟨by simp, hcc.set hc', rfl, fun a => ?_⟩
  have h1 := storeOwn_set_count D a (.full c') hc
  have h2 := Contents.writeAt_own a π hccc hr hw
  have h2' := Contents.writeAt_own a ρ hsub hr' hw'
  have h3 := dropCell_measure hF hold hd a
  simp only [Cell.own] at h1
  simp only [List.count_append, Fresh.set, Val.own_unit, List.count_nil]
  simp only [Val.own] at *
  omega

/-- A scope teardown after a value (`endscope` §6.7, the frame pop §6.9), as
a ledger (helper). -/
theorem Cons.unwind {D : Decls} {F : Event → List Nat} (hF : TraceMeasure D F)
    {H : Store} {v : Val} {ls : List Nat} (hcc : StoreCC D H)
    (hv : (Contents.ofVal v).copyClosed D = true) :
    Cons D F H (v.own D)
      (match unwindLocs D H ls with
       | .error w => .stuck w
       | .ok (H', evs) => .ok H' v evs) := by
  cases hu : unwindLocs D H ls with
  | error w => trivial
  | ok r =>
      obtain ⟨H', evs⟩ := r
      obtain ⟨i, l, c⟩ := unwindLocs_measure hF hcc hu
      refine ⟨by omega, c, hv, fun a => ?_⟩
      have := i a
      have hf : Fresh H H' = [] := by simp [Fresh, l]
      rw [hf]; simp
      omega

/-! ## The law, over the whole machine -/

/-- **The conservation law** (§7's no-double-free, the invariant half): every
evaluation, of every expression from every copy-closed store, at every fuel,
keeps `Cons` — no identity is ever duplicated, only moved, consumed, or
minted fresh. By fuel induction over `eval`, one case per form, each closed by
its ledger above. It reads no typing derivation: copy closure is what the
machine maintains itself, and `DtorNotCopy` (inside `TraceMeasure`, for
`dtorIds`) is the one fact it needs about the declarations. -/
theorem eval_conserves (M : FloatOps) {P : Program} {F : Event → List Nat}
    (hF : TraceMeasure P.decls F) :
    ∀ (fuel : Nat) (H : Store) (φ : Frame) (e : Expr), StoreCC P.decls H →
      Cons P.decls F H [] (eval M fuel P H φ e) := by
  intro fuel
  induction fuel with
  | zero => intro H φ e _; simp only [eval]; trivial
  | succ n ih =>
    intro H φ e hcc
    have hargs := fun (H' : Store) (es : List Expr) (hc : StoreCC P.decls H') =>
      evalArgs_cons (fun H'' e' hc' => ih H'' φ e' hc') H' es hc
    cases e with
    | intLit w sg m => exact Cons.scalar hcc trivial
    | floatLit w l => exact Cons.scalar hcc trivial
    | boolLit b => exact Cons.scalar hcc trivial
    | unitLit => exact Cons.scalar hcc trivial
    | use p =>
        simp only [eval]
        split
        · trivial
        · rename_i ℓ _
          split
          · trivial
          · trivial
          · rename_i c hc
            split
            · split
              · trivial
              · rename_i cd hr
                split
                · trivial
                · rename_i leaf evs hd
                  split
                  · trivial
                  · rename_i v hv
                    split
                    · trivial
                    · rename_i c' hw
                      exact Cons.destructure hF hcc hc hr hd hv hw
            · split
              · trivial
              · rename_i sub hr
                split
                · trivial
                · rename_i v hv
                  split
                  · rename_i hm
                    have hsub := Contents.ofVal_toVal hv
                    refine Cons.pure hcc ?_ (fun a => by simp [Val.own_of_copy hm])
                    rw [hsub]; exact Contents.readAt_copyClosed _ (hcc ℓ c hc) hr
                  · split
                    · trivial
                    · rename_i c' hw
                      exact Cons.move hcc hc hr hw hv
    | binop op e₁ e₂ =>
        simp only [eval]
        refine Cons.bind (ih H φ e₁ hcc) (fun H₁ v₁ _ _ hc₁ _ => ?_)
        refine Cons.bind ((ih H₁ φ e₂ hc₁).weaken (by simp)) (fun H₂ v₂ _ _ hc₂ _ => ?_)
        exact Cons.opRes hc₂ (fun v h => evalBinOp_scalar h)
    | unop op e₁ =>
        simp only [eval]
        refine Cons.bind (ih H φ e₁ hcc) (fun H₁ v₁ _ _ hc₁ _ => ?_)
        exact Cons.opRes hc₁ (fun v h => evalUnOp_scalar h)
    | intCast w sg e₁ =>
        simp only [eval]
        refine Cons.bind (ih H φ e₁ hcc) (fun H₁ v₁ _ _ hc₁ _ => ?_)
        exact Cons.opRes hc₁ (fun v h => evalIntCast_scalar h)
    | fintrin k e₁ =>
        simp only [eval]
        refine Cons.bind (ih H φ e₁ hcc) (fun H₁ v₁ _ _ hc₁ _ => ?_)
        exact Cons.opRes hc₁ (fun v h => evalFintrin_scalar h)
    | panic msg => simp only [eval]; exact ⟨0, fun a => by simp⟩
    | dbg e₁ =>
        simp only [eval]
        refine Cons.bind (ih H φ e₁ hcc) (fun H₁ v₁ _ _ hc₁ _ => ?_)
        split
        · refine ⟨Nat.le_refl _, hc₁, rfl, fun a => ?_⟩
          simp [hF.dbg]
        · trivial
    | mkStruct s args =>
        simp only [eval]
        have ka := hargs H args hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          obtain ⟨l₁, c₁, _, i₁⟩ := ka
          refine Cons.prefix (Y := Contents.ownList P.decls (Contents.ofVals vs)) l₁
            (fun a => by have := i₁ a; simp only [List.count_nil] at *; omega) ?_
          split
          · trivial
          · split
            · exact Cons.intro c₁ (fun a => Contents.own_struct_le P.decls s _ _ a)
            · trivial
    | mkEnum e k args =>
        simp only [eval]
        have ka := hargs H args hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          obtain ⟨l₁, c₁, _, i₁⟩ := ka
          refine Cons.prefix (Y := Contents.ownList P.decls (Contents.ofVals vs)) l₁
            (fun a => by have := i₁ a; simp only [List.count_nil] at *; omega) ?_
          split
          · trivial
          · split
            · trivial
            · split
              · exact Cons.intro c₁ (fun a => Contents.own_enum_le P.decls e k _ _ a)
              · trivial
    | «match» scrut arms =>
        simp only [eval]
        refine Cons.bind (ih H φ scrut hcc) (fun H₀ v _ _ hc₀ hv => ?_)
        cases v with
        | enum e k i vs =>
          dsimp only
          split
          · trivial
          · rename_i body harm
            have hpay := Contents.enum_payload hv
            refine Cons.prefix (H₁ := (mintParams H₀ vs).1) (Y := []) ?_ ?_ ?_
            · rw [mintParams_length]; omega
            · intro a
              have := matchConsume_measure hF hv a
              rw [storeOwn_mintParams]
              simp only [List.count_append, List.count_nil]
              show _ ≤ _ + ((Contents.enum e k i (Contents.ofVals vs)).own P.decls).count a + _
              omega
            · exact Cons.bind (ih _ _ body (hc₀.mintParams (hpay 0).2))
                (fun H₂ v₂ _ _ hc₂ hv₂ => Cons.unwind hF hc₂ hv₂)
        | _ => trivial
    | mkArray T args =>
        simp only [eval]
        have ka := hargs H args hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          obtain ⟨l₁, c₁, _, i₁⟩ := ka
          refine Cons.prefix (Y := Contents.ownList P.decls (Contents.ofVals vs)) l₁
            (fun a => by have := i₁ a; simp only [List.count_nil] at *; omega) ?_
          exact Cons.intro c₁ (fun a => Contents.own_array_le P.decls T _ _ a)
    | repeatArray T e₁ m =>
        simp only [eval]
        refine Cons.bind (ih H φ e₁ hcc) (fun H₁ v₁ _ _ hc₁ _ => ?_)
        split
        · rename_i hm
          refine Cons.intro hc₁ (fun a => ?_)
          have := Contents.own_array_le P.decls T H₁.length
            (Contents.ofVals (List.replicate m v₁)) a
          rw [Contents.ownList_replicate hm] at this
          simp only [List.count_nil] at this
          show _ ≤ _
          have h0 : (v₁.own P.decls).count a = 0 := by simp [Val.own_of_copy hm]
          exact Nat.le_trans this (by omega)
        · trivial
    | indexRead p idx πs =>
        simp only [eval]
        have ka := hargs H idx hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          obtain ⟨l₁, c₁, _, i₁⟩ := ka
          refine Cons.prefix (Y := Contents.ownList P.decls (Contents.ofVals vs)) l₁
            (fun a => by have := i₁ a; simp only [List.count_nil] at *; omega) ?_
          split
          · trivial
          · exact ⟨0, fun a => by simp⟩
          · rename_i ℓ c sub ρ hdp
            obtain ⟨hc, hr⟩ := dynPlace_at hdp
            split
            · trivial
            · rename_i leaf hr'
              split
              · trivial
              · rename_i v hv
                split
                · rename_i hm
                  refine Cons.pure c₁ ?_ (fun a => by simp [Val.own_of_copy hm])
                  rw [Contents.ofVal_toVal hv]
                  exact Contents.readAt_copyClosed ρ
                    (Contents.readAt_copyClosed _ (c₁ ℓ c hc) hr) hr'
                · trivial
    | indexWrite p idx πs e₁ =>
        simp only [eval]
        refine Cons.bind (ih H φ e₁ hcc) (fun H₁ v _ _ hc₁ hv => ?_)
        have ka := hargs H₁ idx hc₁
        split
        · rename_i r hra; rw [hra] at ka; exact ka.weaken (by simp)
        · rename_i H₂ vs tr hra
          rw [hra] at ka
          obtain ⟨l₂, c₂, _, i₂⟩ := ka
          refine Cons.prefix
            (Y := v.own P.decls ++ Contents.ownList P.decls (Contents.ofVals vs)) l₂
            (fun a => by have := i₂ a; simp only [List.count_append] at *; omega) ?_
          split
          · trivial
          · exact ⟨0, fun a => by simp⟩
          · rename_i ℓ c sub ρ hdp
            obtain ⟨hc, hr⟩ := dynPlace_at hdp
            split
            · trivial
            · rename_i old hr'
              split
              · trivial
              · split
                · trivial
                · rename_i evs hd
                  split
                  · trivial
                  · rename_i sub' hw'
                    split
                    · trivial
                    · rename_i c' hw
                      split
                      · rename_i hc'
                        exact Cons.assignDyn hF c₂ hc hr hr' hd hw' hw hc'
                      · trivial
    | indexDrop p idx πs =>
        simp only [eval]
        refine Cons.bind (ih H φ _ hcc) (fun H₁ v₁ _ _ hc₁ _ => ?_)
        exact Cons.pure hc₁ rfl (fun a => by simp)
    | drop p =>
        simp only [eval]
        split
        · trivial
        · rename_i ℓ _
          split
          · trivial
          · trivial
          · rename_i c hc
            split
            · split
              · trivial
              · rename_i cd hr
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
                        exact Cons.dropDeclared hF hcc hc hr hd hl hw
            · split
              · trivial
              · rename_i sub hr
                split
                · trivial
                · split
                  · trivial
                  · rename_i evs hd
                    split
                    · exact Cons.scalar hcc trivial
                    · split
                      · trivial
                      · rename_i c' hw
                        exact Cons.dropPlace hF hcc hc hr hd hw
    | letIn m e₁ e₂ =>
        simp only [eval]
        refine Cons.bind (ih H φ e₁ hcc) (fun H₁ v₁ _ _ hc₁ hv₁ => ?_)
        refine Cons.shift (H₁ := H₁ ++ [.full (Contents.ofVal v₁)]) (Y := []) (by simp)
          (fun a => ?_) ?_
        · rw [storeOwn_append]
          simp [storeOwn, Cell.own, List.count_append]
        · refine Cons.bind (ih _ _ e₂ (hc₁.append (StoreCC.single hv₁)))
            (fun H₂ v₂ _ _ hc₂ hv₂ => ?_)
          split
          · trivial
          · rename_i H₃ evs hdr
            obtain ⟨i, l, c⟩ := dropRetire_measure hF hc₂ hdr
            exact ⟨by omega, c, hv₂, fun a => by have := i a; simp [Fresh, l]; omega⟩
    | assign p e₁ =>
        simp only [eval]
        refine Cons.bind (ih H φ e₁ hcc) (fun H₁ v _ _ hc₁ hv => ?_)
        split
        · trivial
        · split
          · trivial
          · trivial
          · rename_i c hc
            split
            · trivial
            · rename_i old hr
              split
              · trivial
              · split
                · trivial
                · rename_i evs hd
                  split
                  · trivial
                  · rename_i c' hw
                    split
                    · rename_i hc'
                      exact (Cons.assign (Y := []) hF hc₁ hc hr hd hw hc').weaken (by simp)
                    · trivial
    | seq e₁ e₂ =>
        simp only [eval]
        refine Cons.bind (ih H φ e₁ hcc) (fun H₁ v₁ _ _ hc₁ hv₁ => ?_)
        split
        · trivial
        · split
          · trivial
          · rename_i evs hd
            refine Cons.prefix (Y := []) (Nat.le_refl _) (fun a => ?_) (ih H₁ φ e₂ hc₁)
            have := hF.temp hv₁ hd a
            simp only [List.flatMap_cons, List.count_append, List.count_nil, Fresh.self] at *
            omega
        · exact (ih H₁ φ e₂ hc₁).weaken (by simp)
    | ite c e₁ e₂ =>
        simp only [eval]
        refine Cons.bind (ih H φ c hcc) (fun H₀ v₀ _ _ hc₀ _ => ?_)
        split
        · split
          · exact (ih H₀ φ e₁ hc₀).weaken (by simp)
          · exact (ih H₀ φ e₂ hc₀).weaken (by simp)
        · trivial
    | call f args =>
        simp only [eval]
        have ka := hargs H args hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          obtain ⟨l₁, c₁, cv₁, i₁⟩ := ka
          refine Cons.prefix (Y := Contents.ownList P.decls (Contents.ofVals vs)) l₁
            (fun a => by have := i₁ a; simp only [List.count_nil] at *; omega) ?_
          split
          · trivial
          · rename_i fd hfd
            split
            · refine Cons.shift (H₁ := (mintParams H₁ vs).1) (Y := []) ?_ ?_ ?_
              · rw [mintParams_length]; omega
              · intro a
                rw [storeOwn_mintParams]
                simp only [List.count_append, List.count_nil]
                omega
              · refine Cons.absorb (ih _ _ fd.body (c₁.mintParams cv₁))
                  (fun H₃ v _ _ hc₃ hv₃ => ?_)
                simp only [runAllScopeDrops]
                exact Cons.unwind hF hc₃ hv₃
            · trivial
    | ret e₁ =>
        simp only [eval]
        refine Cons.bind (ih H φ e₁ hcc) (fun H₁ v _ _ hc₁ hv => ?_)
        simp only [runAllScopeDrops]
        split
        · trivial
        · rename_i H₂ evs hu
          obtain ⟨i, l, c⟩ := unwindLocs_measure hF hc₁ hu
          exact ⟨by omega, c, hv, fun a => by have := i a; simp [Fresh, l]; omega⟩
    | loop e₁ =>
        simp only [eval]
        have hb := ih H φ e₁ hcc
        split
        · rename_i H₁ tr hr
          rw [hr] at hb
          obtain ⟨l, c, _, i⟩ := hb
          exact Cons.prefix (Y := []) l (fun a => by have := i a; simp at *; omega)
            (ih H₁ φ (.loop e₁) c)
        · trivial
        · rename_i H₁ sc tr hr
          rw [hr] at hb
          obtain ⟨l, c, i⟩ := hb
          split
          · trivial
          · rename_i H₂ evs hu
            obtain ⟨i', l', c'⟩ := unwindLocs_measure hF c hu
            refine ⟨by omega, c', rfl, fun a => ?_⟩
            have := i a; have := i' a
            have hf : Fresh H H₂ = Fresh H H₁ := by simp [Fresh, l']
            rw [hf]
            simp only [List.flatMap_append, List.count_append, Val.own_unit, List.count_nil] at *
            omega
        · exact hb
    | brk =>
        simp only [eval]
        exact ⟨Nat.le_refl _, hcc, fun a => by simp⟩

/-! ## §7: no double free -/

/-- An index occurs in a range once when it lies in it, and not at all
otherwise (helper). Core's `List.count_range'` costs `Classical.choice`, and so
does `omega` on a conjunction or a disequation, so this proof avoids both. -/
theorem range'_count (a : Nat) : ∀ (n s : Nat),
    (List.range' s n).count a = if s ≤ a ∧ a < s + n then 1 else 0
  | 0, s => by
      simp only [List.range'_zero, List.count_nil]
      rw [if_neg (fun h => Nat.lt_irrefl a (Nat.lt_of_lt_of_le h.2 (by simpa using h.1)))]
  | n + 1, s => by
      rw [List.range'_succ, List.count_cons, range'_count a n (s + 1)]
      by_cases h₁ : s = a
      · subst h₁
        have e2 : s ≤ s ∧ s < s + (n + 1) := ⟨Nat.le_refl s, by omega⟩
        rw [if_neg (fun h => Nat.not_succ_le_self s h.1), if_pos e2,
          if_pos (show (s == s) = true from decide_eq_true rfl)]
      · have : (s == a) = false := beq_false_of_ne h₁
        rw [this, if_neg Bool.false_ne_true, Nat.add_zero]
        have hlt : s < a ∨ a < s := Nat.lt_or_gt_of_ne h₁
        clear h₁ this
        by_cases h₂ : s + 1 ≤ a ∧ a < s + 1 + n
        · obtain ⟨h₃, h₄⟩ := h₂
          rw [if_pos ⟨by omega, by omega⟩, if_pos ⟨by omega, by omega⟩]
        · rw [if_neg h₂, if_neg (fun h => h₂ ⟨by
            obtain ⟨h₃, _⟩ := h
            exact hlt.elim id (fun h'' => absurd h₃ (Nat.not_le_of_gt h'')), by omega⟩)]

/-- An index occurs in a range at most once (helper). -/
theorem range'_count_le_one (s n a : Nat) : (List.range' s n).count a ≤ 1 := by
  rw [range'_count]; split <;> omega

/-- **The law, over a whole program** (§6.12's top-level result): a run starts
from the empty store, so everything its trace projects to was minted during
the run — a range of store indices, each once. Every projection `F` the law
can count therefore names each identity at most once (helper). -/
theorem run_trace_once (M : FloatOps) {P : Program} {F : Event → List Nat}
    (hF : TraceMeasure P.decls F) (fuel : Nat) :
    ∀ a, ((run M P fuel).trace.flatMap F).count a ≤ 1 := by
  intro a
  have h := eval_conserves M hF fuel [] { env := [], scope := [] } (.call 0 [])
    (fun ℓ c hc => by simp at hc)
  unfold run
  generalize eval M fuel P [] { env := [], scope := [] } (.call 0 []) = r at h ⊢
  cases r with
  | ok H v tr =>
      have := h.2.2.2 a
      have := range'_count_le_one 0 (H.length - 0) a
      simp only [EvalRes.trace, storeOwn, Fresh, List.length_nil, List.flatMap_nil,
        List.count_nil] at *
      omega
  | returned H v tr =>
      have := h.2.2.2 a
      have := range'_count_le_one 0 (H.length - 0) a
      simp only [EvalRes.trace, storeOwn, Fresh, List.length_nil, List.flatMap_nil,
        List.count_nil] at *
      omega
  | broke H sc tr =>
      have := h.2.2 a
      have := range'_count_le_one 0 (H.length - 0) a
      simp only [EvalRes.trace, storeOwn, Fresh, List.length_nil, List.flatMap_nil,
        List.count_nil] at *
      omega
  | panic k tr =>
      obtain ⟨N, hN⟩ := h
      have := hN a
      have := range'_count_le_one 0 N a
      simp only [EvalRes.trace, storeOwn, List.length_nil, List.flatMap_nil,
        List.count_nil] at *
      omega
  | stuck w => simp [EvalRes.trace]
  | outOfFuel => simp [EvalRes.trace]

/-- **No identity appears twice among the `drop`/`dropTemp` free events, on
any finished run** (§6.11): each owned identity occurs at most once among the
trees those markers free (`freedIds`). A declared-linear destructure's
residue is freed with no marker of its own (`dropResidue`), so it sits
outside this count until RUE-2328 adds one; `dtor_once` below already
catches it when the residue has a destructor. Holds unconditionally, for
every program, no hypothesis at all: the machine refuses the one shape — an
owned value hidden under a `Copy` node — that would let a copy duplicate it
(`Contents.copyClosed`). -/
theorem freed_once (M : FloatOps) (P : Program) (fuel : Nat) :
    ∀ a, (freedIds P.decls (run M P fuel).trace).count a ≤ 1 :=
  run_trace_once M (freed_measure P.decls) fuel

/-- **No destructor runs twice on one value, on any finished run** (§6.11, `3.9:28`):
each identity occurs at most once among the values the trace's `dtor` events
ran on. It needs only that a destructor-bearing struct is not `Copy`
(`3.9:31`), which `WfDecls` gives (`WfDecls.dtorNotCopy`). -/
theorem dtor_once (M : FloatOps) {P : Program} (hdt : DtorNotCopy P.decls) (fuel : Nat) :
    ∀ a, (dtorIds (run M P fuel).trace).count a ≤ 1 :=
  run_trace_once M (dtor_measure hdt) fuel

/-- **No double free** (§7): a well-typed program's run is never refused, so
its trace is the whole run's (`no_violation`), and in that trace no identity
appears twice among the `drop`/`dropTemp` free events (`freedIds`) and no
identity has its destructor run twice (`dtorIds`, §7's literal claim). The
first conjunct is what makes the other two a statement about the program
rather than about a refusal: a refused run carries no trace at all.

§7 argues the bullet from three mechanisms, and each is a case of the
conservation law `eval_conserves` proves: a move leaves `⊘` behind (§6.3), so
the old owner no longer owns what it handed on; §6.11's walk skips every `⊘`
("this single skip is what makes double-free impossible"), so a moved-out
position is never dropped through the old owner; and a `match` binding takes
the payload whole into the arm's cells, so the scrutinee's owner is gone.
The declared-linear destructure (§6.3) consumes its place the same way: the
leaf is handed on, the residue is dropped once with no marker of its own —
so it counts toward `dtorIds` when it has a destructor, but sits outside
`freedIds` until RUE-2328 — and the place becomes `⊘`. -/
theorem no_double_free (M : FloatModel) {P : Program} (h : ProgramTyped P) (fuel : Nat) :
    (∀ w, run M.toFloatOps P fuel ≠ .stuck w) ∧
      (∀ a, (freedIds P.decls (run M.toFloatOps P fuel).trace).count a ≤ 1) ∧
      (∀ a, (dtorIds (run M.toFloatOps P fuel).trace).count a ≤ 1) :=
  ⟨no_violation M h fuel, freed_once M.toFloatOps P fuel,
    dtor_once M.toFloatOps h.wf.decls.dtorNotCopy fuel⟩

/-! ## Witnesses: what the monitor stands between

The one shape the conservation law needs excluded is an owned value under a
`Copy` node. Here it is, built by an ill-typed program: a `@copy` struct `S0`
whose `i64` field is given an `S1`, which declares a destructor. The program
copies the `S0` and then drops the field through both copies:

```rue
let p = S0 { x0: S1 { x0: 1 } };  // ill-typed: x0 is an i64
let q = p;                        // S0 is Copy, so this copies
@drop(p.x0);
@drop(q.x0);
0
```

The checker rejects it; `eval` refuses it at the literal with
`ownedUnderCopy`; and `Step`, which follows §6's rules and has no monitor,
runs it to the end and runs `S1`'s destructor on the **same identity twice**.
So the identity count is not vacuous: without the monitor, or without the
typing that keeps a checked program away from it, the trace shows a double
free. -/

/-- `S0 = @copy struct { x0: i64 }` and `S1 = struct { x0: i64 }` with a
destructor (helper). -/
def dupDecls : Decls :=
  Decls.ofStructs
    [{ attr := .copy, fields := [.int .w64 .signed], dtor := false, cls := .copy },
     { attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine }]

/-- The program above (helper). -/
def dupProgram : Program :=
  Program.entry dupDecls (.int .w64 .signed)
    (.letIn false (.mkStruct 0 [.mkStruct 1 [.intLit .w64 .signed 1]])
      (.letIn false (.use (.var 0))
        (.seq (.drop (.proj (.var 1) 0))
          (.seq (.drop (.proj (.var 0) 0)) (.intLit .w64 .signed 0)))))

/-- The checker rejects it (§5.8's (Struct-Intro): the field is an `i64`). -/
example : checkProgram dupProgram = false := by rfl

/-- `eval` refuses it where the owned value would go under the `Copy` node
(`Contents.copyClosed`). -/
example : run Float.exactOps dupProgram 100 = .stuck .ownedUnderCopy := by rfl

/-- **§6's relation, with no monitor, frees one identity twice**: `S1 #0` is
destroyed through `p.x0` and again through `q.x0`, so `dtorIds` names `0`
twice. -/
theorem dupProgram_step_double_free (M : FloatOps) :
    ∃ H, Steps M dupProgram Config.init
      (.run H { env := [], scope := [] } [] (.ret (.int .w64 .signed 0))
        [.drop 2 (.struct 1 0 [.int .w64 .signed 1]), .dtor 1 (.struct 1 0 [.int .w64 .signed 1]),
         .drop 3 (.struct 1 0 [.int .w64 .signed 1]), .dtor 1 (.struct 1 0 [.int .w64 .signed 1])]) ∧
      dtorIds [.drop 2 (.struct 1 0 [.int .w64 .signed 1]),
        .dtor 1 (.struct 1 0 [.int .w64 .signed 1]),
        .drop 3 (.struct 1 0 [.int .w64 .signed 1]),
        .dtor 1 (.struct 1 0 [.int .w64 .signed 1])] = [0, 0] :=
  ⟨_, stepN_steps (n := 100), rfl⟩

/-- The same monitor at an assignment: `let mut p = S0 { 1 }; p.x0 = S1 { 2 }; 0`
writes an owned value under the `Copy` node, and `eval` refuses the write
((D-Assign) §6.8). -/
example :
    run Float.exactOps
      (Program.entry dupDecls (.int .w64 .signed)
        (.letIn true (.mkStruct 0 [.intLit .w64 .signed 1])
          (.seq (.assign (.proj (.var 0) 0) (.mkStruct 1 [.intLit .w64 .signed 2]))
            (.intLit .w64 .signed 0)))) 100
      = .stuck .ownedUnderCopy := by rfl

end RueCore
