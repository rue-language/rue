import RueCore.Soundness

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

`no_double_free` says each identity occurs at most once in each.

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
holds of every run, typed or not, given only `WfDecls`' "a destructor-bearing
struct is not `Copy`". Typing enters `no_double_free` through `no_violation`:
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
§6.11's walk goes through. A destructor event frees nothing of its own — it is
nested under a marker, or under a destructure's residue drop — and a `@dbg`
frees nothing (helper). -/
def Event.freed (D : Decls) : Event → List Nat
  | .drop _ c => c.own D
  | .dropTemp v => v.own D
  | .dtor _ _ | .dbg _ => []

/-- The identity of the value a user destructor ran on (§6.11, `3.9:28`) —
every `dtor` event carries the struct it ran on (helper). -/
def Event.dtorIds : Event → List Nat
  | .dtor _ (.struct _ i _) => [i]
  | _ => []

/-- The identities the trace's markers free, in trace order. -/
def freedIds (D : Decls) (tr : List Event) : List Nat := tr.flatMap (Event.freed D)

/-- The identities the trace's destructors ran on, in trace order. -/
def dtorIds (tr : List Event) : List Nat := tr.flatMap Event.dtorIds

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

end RueCore
