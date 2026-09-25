import RueCore.TraceExact

/-!
# RueCore.TraceOrder — the order drops run in (§3.9, §6.11, §7)

`no_double_free` (`Trace.lean`) says no owned value is ended twice, and
`drop_exactly_once` (`TraceExact.lean`) that each is ended exactly once. This
module says **in what order** the ends happen. The order has two halves, and
they need two different pieces of machinery, so `drop_order` is their
conjunction.

## Within a value: a grammar over the trace (§3.9, §6.11)

§6.11 fixes the order of one value's drop: its user destructor first
(`3.9:28`), then its fields in declaration order (`3.9:13`), an array's
elements in ascending index order (`3.9:15`, `3.8:73`), and an enum's
**active** payload only (`6.3:20`), every `⊘` skipped. `dropEvents` is that
order written as a function. `Blocks` is a grammar over whole traces: a trace
is a sequence of blocks, each a `@dbg` line, a consumption, or a drop marker
(`drop ℓ c`, `dropTemp v`) followed by **exactly** §6.11's walk of what the
marker names. The grammar has no other place for a destructor event, so a
trace in it runs every destructor inside the walk of the marker before it,
in §6.11's order, and no destructor anywhere else.

`run_blocks` proves every finished run's trace is in the grammar. It reads no
typing derivation, only `DtorNotCopy` (a destructor-bearing struct is not
`Copy`, `3.9:31`), which a declared-linear destructure's `Copy` residue
subtree needs: that subtree is dropped with no marker, as `@drop` of a `Copy`
place is, and its walk is empty only because nothing under a `Copy` node owns
anything (copy closure, which the machine maintains) and no `Copy` struct
declares a destructor.

## Across cells: last-in first-out, over §6's relation (§6.7, §6.9, §6.10)

The order *between* cells is not a property of the trace alone: every
`drop ℓ c` block is a valid block by itself, so "the trace can be cut into
newest-first groups" says nothing. What gives it content is the machine's
scope records, so this half is stated over `Step`, from every configuration
reachable from `Config.init`:

* `reachable_ordered`: every scope record in a reachable configuration — the
  current frame's, every suspended caller's and loop boundary's, and every
  pending `endscope` marker's — lists its cells in strictly increasing
  location order, below the store's length. Records are only ever extended
  with freshly allocated cells, so **registration order is location order**.
* `step_drop_order`: every step's `drop` markers either all name one cell
  (an overwrite, `@drop`, or a destructure's residue, several sub-positions
  of one binding) or name distinct cells in **strictly decreasing** location
  order — newest registered first.
* `reachable_nested`: the scopes **nest**. A frame's pending `endscope`
  markers are exactly the tail of its record, innermost last, and the whole
  registration stack — every suspended caller's record, then the current
  frame's — is in location order. So (D-EndScope)'s pop by count removes the
  marker's own cells (`Frame.popScope_tail`).
* `reachable_lifo`: every step is **last-in first-out** on that stack
  (`Lifo`). It keeps the stack as a prefix of the new one, or cuts it back
  and drops only cells of the suffix it cut, newest first; each such cell is
  newer than every cell still registered. That orders drops *across* steps:
  `{ let a; let b; }` exits over two (D-EndScope) steps, and `b` drops
  first. `swappedMarkers_rejected` is a configuration with the two markers
  swapped: every record is in order and every step drops one cell, yet it
  drops oldest first — and it is not `Nested`, so no run reaches it.

`drop_order` states both halves over `Step`: the within-value half reaches
`Step`'s finished runs through `eval_complete` (`step_blocks`).

## What the grammar does not constrain

`Blocks` ties each marker to its walk, not to the cell: fidelity to what the
cell held is `dropCell` reading `H(ℓ)` and the exactly-once ledger
(`TraceExact.lean`). A consumption carries no walk, so on a program the
checker rejects a destructor-bearing node can be consumed without its
destructor running (`destructure_under_dtor`, which `3.9:34` makes E0456);
the grammar accepts that trace. And "an enum's active payload only" is how
`Contents.enum` stores a value — it holds the active variant's payload and
no other — rather than a clause of the grammar.
-/

namespace RueCore

/-! ## Within a value: the block grammar -/

/-- **§6.11's order, as a grammar over the trace.** A trace is a sequence of
blocks: a `@dbg` line, a consumption (`consume c`, which runs no drop of its
own), or a drop marker followed by exactly the events §6.11's walk of what it
names emits (`dropEvents`) — for a binding's drop `drop ℓ c`, the contents
`c`, and for a discarded temporary `dropTemp v`, the value `v`. A destructor
event (`dtor`) appears only inside such a walk, so the grammar says where
every destructor runs: inside the drop of the value that owns it, after the
destructors of everything dropped before it in §6.11's order (§3.9, §6.11). -/
inductive Blocks (D : Decls) : List Event → Prop
  | nil : Blocks D []
  | dbg {v : Val} {t : List Event} : Blocks D t → Blocks D (.dbg v :: t)
  | consume {c : Contents} {t : List Event} : Blocks D t → Blocks D (.consume c :: t)
  | drop {ℓ : Nat} {c : Contents} {t : List Event} :
      Blocks D t → Blocks D (.drop ℓ c :: (dropEvents D c ++ t))
  | dropTemp {v : Val} {t : List Event} :
      Blocks D t → Blocks D (.dropTemp v :: (dropEvents D (.ofVal v) ++ t))

/-- Two block sequences, one after the other, are one (helper). -/
theorem Blocks.append {D : Decls} {t u : List Event} (h₁ : Blocks D t) (h₂ : Blocks D u) :
    Blocks D (t ++ u) := by
  induction h₁ with
  | nil => exact h₂
  | dbg _ ih => exact .dbg ih
  | consume _ ih => exact .consume ih
  | drop _ ih => simpa only [List.cons_append, List.append_assoc] using Blocks.drop ih
  | dropTemp _ ih => simpa only [List.cons_append, List.append_assoc] using Blocks.dropTemp ih

/-- A single binding's drop block (helper). -/
theorem Blocks.dropOne {D : Decls} (ℓ : Nat) (c : Contents) :
    Blocks D (.drop ℓ c :: dropEvents D c) := by
  simpa using (Blocks.drop (ℓ := ℓ) (c := c) (Blocks.nil (D := D)))

mutual
/-- **The walk is §6.11's order**: whenever `dropContents` succeeds, it emits
exactly `dropEvents`, with no typing hypothesis — the walk refuses only an
unbound struct index, where `dropEvents` would emit nothing (§6.11). -/
theorem dropContents_eq {D : Decls} : ∀ {c : Contents} {evs : List Event},
    dropContents D c = .ok evs → evs = dropEvents D c
  | .hole, _, h | .int _ _ _, _, h | .float _ _, _, h | .bool _, _, h | .unit, _, h => by
      simp [dropContents] at h; subst h; rfl
  | .struct s i cs, evs, h => by
      simp only [dropContents] at h
      split at h
      · cases h
      · rename_i sd hd
        split at h
        · cases h
        · rename_i evs' hl
          cases h
          simp [dropEvents, hd, dropContentsList_eq hl]
  | .enum _ _ _ cs, _, h => by
      simp only [dropContents] at h; simp only [dropEvents]; exact dropContentsList_eq h
  | .array _ _ cs, _, h => by
      simp only [dropContents] at h; simp only [dropEvents]; exact dropContentsList_eq h

/-- The same over a list (helper). -/
theorem dropContentsList_eq {D : Decls} : ∀ {cs : List Contents} {evs : List Event},
    dropContentsList D cs = .ok evs → evs = dropEventsList D cs
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
          simp [dropEventsList, dropContents_eq h₁, dropContentsList_eq h₂]
end

mutual
/-- An all-`Copy` contents' walk is empty: no `Copy` struct declares a
destructor (`3.9:31`) (helper). -/
theorem dropEvents_allCopy {D : Decls} (hdt : DtorNotCopy D) :
    ∀ {c : Contents}, c.allCopy D = true → dropEvents D c = []
  | .hole, _ | .int _ _ _, _ | .float _ _, _ | .bool _, _ | .unit, _ => rfl
  | .struct s i cs, hac => by
      simp only [Contents.allCopy, Bool.and_eq_true, decide_eq_true_eq] at hac
      simp only [dropEvents, dropEventsList_allCopy hdt hac.2, List.append_nil]
      split
      · rename_i sd hd
        have : sd.dtor = false := by
          cases hsd : sd.dtor
          · rfl
          · exact absurd hac.1 (hdt s sd hd hsd)
        simp [this]
      · rfl
  | .enum _ _ _ cs, hac => by
      simp only [Contents.allCopy, Bool.and_eq_true] at hac
      simp only [dropEvents]; exact dropEventsList_allCopy hdt hac.2
  | .array _ _ cs, hac => by
      simp only [Contents.allCopy, Bool.and_eq_true] at hac
      simp only [dropEvents]; exact dropEventsList_allCopy hdt hac.2

/-- The same over a list (helper). -/
theorem dropEventsList_allCopy {D : Decls} (hdt : DtorNotCopy D) :
    ∀ {cs : List Contents}, Contents.allCopyList D cs = true → dropEventsList D cs = []
  | [], _ => rfl
  | c :: cs, hac => by
      simp only [Contents.allCopyList, Bool.and_eq_true] at hac
      simp [dropEventsList, dropEvents_allCopy hdt hac.1, dropEventsList_allCopy hdt hac.2]
end

/-- A binding's drop (`dropCell`) is one block, or nothing for `Copy`
contents (helper). -/
theorem dropCell_blocks {D : Decls} {ℓ : Nat} {c : Contents} {evs : List Event}
    (h : dropCell D ℓ c = .ok evs) : Blocks D evs := by
  unfold dropCell at h
  split at h
  · cases h; exact .nil
  · split at h
    · cases h
    · rename_i evs' hw
      cases h
      rw [dropContents_eq hw]
      exact Blocks.dropOne ℓ c

/-- `drop-retire` (§6.1) is one block or nothing (helper). -/
theorem dropRetire_blocks {D : Decls} {H H' : Store} {ℓ : Nat} {evs : List Event}
    (h : dropRetire D H ℓ = .ok (H', evs)) : Blocks D evs := by
  unfold dropRetire at h
  split at h
  · cases h
  · cases h
  · split at h
    · cases h
    · split at h
      · cases h
      · rename_i evs' hd
        cases h
        exact dropCell_blocks hd

/-- `run-scope-drops` (§6.1) is a sequence of blocks, one per live
non-`Copy` cell, in the order given (helper). -/
theorem unwindLocs_blocks {D : Decls} :
    ∀ {H H' : Store} {ls : List Nat} {evs : List Event},
      unwindLocs D H ls = .ok (H', evs) → Blocks D evs
  | _, _, [], _, h => by simp [unwindLocs] at h; obtain ⟨_, rfl⟩ := h; exact .nil
  | H, _, ℓ :: ls, _, h => by
      simp only [unwindLocs] at h
      split at h
      · cases h
      · rename_i H₁ evs₁ h₁
        split at h
        · cases h
        · rename_i H₂ evs₂ h₂
          cases h
          exact (dropRetire_blocks h₁).append (unwindLocs_blocks h₂)

/-- §6.3's `drop*` on a destructure's residue is a sequence of blocks: a
non-`Copy` subtree's marker and walk, and a `Copy` subtree's empty walk
(helper). -/
theorem dropResidue_blocks {D : Decls} (hdt : DtorNotCopy D) {ℓ : Nat} :
    ∀ {rs : List Contents} {evs : List Event}, Contents.copyClosedList D rs = true →
      dropResidue D ℓ rs = .ok evs → Blocks D evs
  | [], _, _, h => by simp [dropResidue] at h; subst h; exact .nil
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
            have ih := dropResidue_blocks hdt hcc.2 h₂
            rw [dropContents_eq h₁]
            unfold residueMark
            split
            · rename_i hm
              rw [dropEvents_allCopy hdt (Contents.copyClosed_allCopy hcc.1 hm)]
              simpa using ih
            · simpa using Blocks.drop (ℓ := ℓ) (c := r) ih

/-- §6.3's destructure is a sequence of blocks: the residue's, then the
consumed shell (helper). -/
theorem destructure_blocks {D : Decls} (hdt : DtorNotCopy D) {ℓ : Nat} {cd leaf : Contents}
    {πs : List Nat} {evs : List Event} (hcc : cd.copyClosed D = true)
    (h : cd.destructure D ℓ πs = .ok (leaf, evs)) : Blocks D evs := by
  unfold Contents.destructure at h
  split at h
  · cases h
  · rename_i leaf' rs hs
    split at h
    · cases h
    · rename_i evs' hd
      cases h
      exact (dropResidue_blocks hdt (Contents.splitResidue_own 0 πs hcc hs).2.2 hd).append
        (.consume .nil)

/-- (D-Match)'s consumption is a block or nothing (helper). -/
theorem matchConsume_blocks {D : Decls} {e k i : Nat} {vs : List Val} :
    Blocks D (matchConsume D e k i vs) := by
  unfold matchConsume
  split
  · exact .nil
  · exact .consume .nil

/-! ## Within a value: every evaluation's trace is blocks -/

/-- A prefix of blocks before a result's blocks (helper). -/
theorem Blocks.withTrace {D : Decls} {tr : List Event} {r : EvalRes} (h₁ : Blocks D tr)
    (h₂ : Blocks D r.trace) : Blocks D (r.withTrace tr).trace := by
  cases r <;> simp only [EvalRes.withTrace, EvalRes.trace] at h₂ ⊢ <;>
    first | exact h₁.append h₂ | exact .nil

/-- §6.2's search keeps the grammar (helper). -/
theorem Blocks.bind {D : Decls} {r : EvalRes} {k : Store → Val → EvalRes}
    (hr : Blocks D r.trace) (hk : ∀ H₁ v tr, r = .ok H₁ v tr → Blocks D (k H₁ v).trace) :
    Blocks D (r.andThen k).trace := by
  cases r with
  | ok H₁ v tr => exact Blocks.withTrace hr (hk H₁ v tr rfl)
  | _ => exact hr

/-- §6.9's call boundary keeps the grammar (helper). -/
theorem Blocks.absorb {D : Decls} {r : EvalRes} {k : Store → Val → EvalRes}
    (hr : Blocks D r.trace) (hk : ∀ H₁ v tr, r = .ok H₁ v tr → Blocks D (k H₁ v).trace) :
    Blocks D (r.absorb k).trace := by
  cases r with
  | ok H₁ v tr => exact Blocks.withTrace hr (hk H₁ v tr rfl)
  | broke => exact .nil
  | _ => exact hr

/-- An operator's outcome emits nothing (helper). -/
theorem Blocks.opRes {D : Decls} {H : Store} {o : OpRes} : Blocks D (o.toRes H).trace := by
  cases o <;> exact .nil

/-- Aggregate introduction emits nothing (helper). -/
theorem Blocks.intro {D D' : Decls} {H : Store} {mk : Nat → Val} :
    Blocks D (introVal D' H mk).trace := by
  unfold introVal; split <;> exact .nil

/-- A copy-closed store is one step further along an evaluation that reached
a value (helper). -/
theorem eval_ok_cc (M : FloatOps) {P : Program} {n : Nat} {H H₁ : Store} {φ : Frame} {e : Expr}
    {v : Val} {tr : List Event} (hcc : StoreCC P.decls H)
    (hr : eval M n P H φ e = .ok H₁ v tr) :
    StoreCC P.decls H₁ ∧ (Contents.ofVal v).copyClosed P.decls = true := by
  have h := eval_conserves M (freed_measure P.decls) n H φ e hcc
  rw [hr] at h
  exact ⟨h.2.1, h.2.2.1⟩

/-- The grammar's promise about an argument list (helper). -/
def ArgsBlocks (D : Decls) : ArgsRes → Prop
  | .ok _ _ tr => Blocks D tr
  | .abort r => Blocks D r.trace

/-- An argument list keeps the grammar (helper). -/
theorem evalArgs_blocks {D : Decls} {ev : Store → Expr → EvalRes}
    (hev : ∀ H e, StoreCC D H → Blocks D (ev H e).trace)
    (hcc : ∀ H e H₁ v tr, StoreCC D H → ev H e = .ok H₁ v tr → StoreCC D H₁) :
    ∀ (H : Store) (es : List Expr), StoreCC D H → ArgsBlocks D (evalArgs ev H es)
  | H, [], _ => .nil
  | H, e :: es, hc => by
      simp only [evalArgs]
      have h₁ := hev H e hc
      cases hr : ev H e with
      | ok H₁ v tr =>
          rw [hr] at h₁
          have h₂ := evalArgs_blocks hev hcc H₁ es (hcc H e H₁ v tr hc hr)
          dsimp only
          cases hra : evalArgs ev H₁ es with
          | ok H₂ vs tr₂ => rw [hra] at h₂; exact Blocks.append h₁ h₂
          | abort r => rw [hra] at h₂; exact Blocks.withTrace h₁ h₂
      | _ => rw [hr] at h₁; exact h₁

/-- **Every evaluation's trace is in §6.11's block grammar** (§3.9, §6.11):
every evaluation, of every expression, from every copy-closed store, at every
fuel. By fuel induction over `eval`; no typing derivation, only
`DtorNotCopy`, which a destructure's `Copy` residue needs (module
docstring). -/
theorem eval_blocks (M : FloatOps) {P : Program} (hdt : DtorNotCopy P.decls) :
    ∀ (fuel : Nat) (H : Store) (φ : Frame) (e : Expr), StoreCC P.decls H →
      Blocks P.decls (eval M fuel P H φ e).trace := by
  intro fuel
  induction fuel with
  | zero => intro H φ e _; exact .nil
  | succ n ih =>
    intro H φ e hcc
    have hok := fun {H' : Store} {φ' : Frame} {e' : Expr} {H₁ : Store} {v : Val}
        {tr : List Event} (hc : StoreCC P.decls H') (hr : eval M n P H' φ' e' = .ok H₁ v tr) =>
      eval_ok_cc M hc hr
    have hargs := fun (H' : Store) (es : List Expr) (hc : StoreCC P.decls H') =>
      evalArgs_blocks (ev := fun H'' e' => eval M n P H'' φ e')
        (fun H'' e' hc' => ih H'' φ e' hc') (fun _ _ _ _ _ hc' hr => (hok hc' hr).1) H' es hc
    have hargsc := fun (H' : Store) (es : List Expr) (hc : StoreCC P.decls H') =>
      evalArgs_cons (F := Event.freed P.decls) (ev := fun H'' e' => eval M n P H'' φ e')
        (fun H'' e' hc' => eval_conserves M (freed_measure P.decls) n H'' φ e' hc') H' es hc
    cases e with
    | intLit | floatLit | boolLit | unitLit | panic | brk => exact .nil
    | use p =>
        simp only [eval]
        split
        · exact .nil
        · split
          · exact .nil
          · exact .nil
          · rename_i c hc
            split
            · split
              · exact .nil
              · rename_i cd hr
                split
                · exact .nil
                · rename_i leaf evs hd
                  have hb := destructure_blocks hdt
                    (Contents.readAt_copyClosed _ (hcc _ c hc) hr) hd
                  (repeat' split) <;> first | exact .nil | exact hb
            · (repeat' split) <;> exact .nil
    | drop p =>
        simp only [eval]
        split
        · exact .nil
        · split
          · exact .nil
          · exact .nil
          · rename_i c hc
            split
            · split
              · exact .nil
              · rename_i cd hr
                split
                · exact .nil
                · rename_i leaf evs hd
                  have hb := destructure_blocks hdt
                    (Contents.readAt_copyClosed _ (hcc _ c hc) hr) hd
                  split
                  · exact .nil
                  · split
                    · exact .nil
                    · rename_i levs hl
                      split
                      · exact .nil
                      · exact hb.append (dropCell_blocks hl)
            · split
              · exact .nil
              · split
                · exact .nil
                · split
                  · exact .nil
                  · rename_i evs hd
                    split
                    · exact .nil
                    · split
                      · exact .nil
                      · exact dropCell_blocks hd
    | binop op e₁ e₂ =>
        simp only [eval]
        refine Blocks.bind (ih H φ e₁ hcc) (fun H₁ _ _ hr => ?_)
        exact Blocks.bind (ih H₁ φ e₂ (hok hcc hr).1) (fun _ _ _ _ => Blocks.opRes)
    | unop _ e₁ | intCast _ _ e₁ | fintrin _ e₁ =>
        simp only [eval]
        exact Blocks.bind (ih H φ e₁ hcc) (fun _ _ _ _ => Blocks.opRes)
    | dbg e₁ =>
        simp only [eval]
        refine Blocks.bind (ih H φ e₁ hcc) (fun _ _ _ _ => ?_)
        split
        · exact .dbg .nil
        · exact .nil
    | repeatArray T e₁ m =>
        simp only [eval]
        refine Blocks.bind (ih H φ e₁ hcc) (fun _ _ _ _ => ?_)
        split
        · exact Blocks.intro
        · exact .nil
    | mkStruct _ args | mkEnum _ _ args | mkArray _ args =>
        simp only [eval]
        have ka := hargs H args hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          refine Blocks.withTrace ka ?_
          (repeat' split) <;> first | exact .nil | exact Blocks.intro
    | indexRead p idx πs =>
        simp only [eval]
        have ka := hargs H idx hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          refine Blocks.withTrace ka ?_
          (repeat' split) <;> exact .nil
    | indexDrop p idx πs =>
        simp only [eval]
        exact Blocks.bind (ih H φ _ hcc) (fun _ _ _ _ => .nil)
    | indexWrite p idx πs e₁ =>
        simp only [eval]
        refine Blocks.bind (ih H φ e₁ hcc) (fun H₁ _ _ hr => ?_)
        have ka := hargs H₁ idx (hok hcc hr).1
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₂ vs tr hra
          rw [hra] at ka
          refine Blocks.withTrace ka ?_
          split
          · exact .nil
          · exact .nil
          · split
            · exact .nil
            · split
              · exact .nil
              · split
                · exact .nil
                · rename_i evs hd
                  (repeat' split) <;> first | exact .nil | exact dropCell_blocks hd
    | «match» scrut arms =>
        simp only [eval]
        refine Blocks.bind (ih H φ scrut hcc) (fun H₀ v _ hr => ?_)
        obtain ⟨hc₀, hv⟩ := hok hcc hr
        cases v with
        | enum e k i vs =>
          dsimp only
          split
          · exact .nil
          · rename_i body _
            refine Blocks.withTrace matchConsume_blocks ?_
            refine Blocks.bind (ih _ _ body (hc₀.mintParams (Contents.enum_payload hv 0).2))
              (fun _ _ _ _ => ?_)
            split
            · exact .nil
            · rename_i H₃ evs hu
              exact unwindLocs_blocks hu
        | _ => exact .nil
    | letIn m e₁ e₂ =>
        simp only [eval]
        refine Blocks.bind (ih H φ e₁ hcc) (fun H₁ v₁ _ hr => ?_)
        obtain ⟨hc₁, hv₁⟩ := hok hcc hr
        refine Blocks.bind (ih _ _ e₂ (hc₁.append (StoreCC.single hv₁))) (fun _ _ _ _ => ?_)
        split
        · exact .nil
        · rename_i H₃ evs hdr
          exact dropRetire_blocks hdr
    | assign p e₁ =>
        simp only [eval]
        refine Blocks.bind (ih H φ e₁ hcc) (fun _ _ _ _ => ?_)
        split
        · exact .nil
        · split
          · exact .nil
          · exact .nil
          · split
            · exact .nil
            · split
              · exact .nil
              · split
                · exact .nil
                · rename_i evs hd
                  (repeat' split) <;> first | exact .nil | exact dropCell_blocks hd
    | seq e₁ e₂ =>
        simp only [eval]
        refine Blocks.bind (ih H φ e₁ hcc) (fun H₁ v₁ _ hr => ?_)
        have hc₁ := (hok hcc hr).1
        split
        · exact .nil
        · split
          · exact .nil
          · rename_i evs hd
            refine Blocks.withTrace ?_ (ih H₁ φ e₂ hc₁)
            rw [dropContents_eq hd]
            simpa using (Blocks.dropTemp (v := v₁) (Blocks.nil (D := P.decls)))
        · exact ih H₁ φ e₂ hc₁
    | ite c e₁ e₂ =>
        simp only [eval]
        refine Blocks.bind (ih H φ c hcc) (fun H₀ _ _ hr => ?_)
        have hc₀ := (hok hcc hr).1
        split
        · split
          · exact ih H₀ φ e₁ hc₀
          · exact ih H₀ φ e₂ hc₀
        · exact .nil
    | call f args =>
        simp only [eval]
        have ka := hargs H args hcc
        have kc := hargsc H args hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka kc
          obtain ⟨_, c₁, cv₁, _⟩ := kc
          refine Blocks.withTrace ka ?_
          split
          · exact .nil
          · rename_i fd _
            split
            · refine Blocks.absorb (ih _ _ fd.body (c₁.mintParams cv₁)) (fun _ _ _ _ => ?_)
              split
              · exact .nil
              · rename_i H₄ evs hu
                exact unwindLocs_blocks hu
            · exact .nil
    | ret e₁ =>
        simp only [eval]
        refine Blocks.bind (ih H φ e₁ hcc) (fun _ _ _ _ => ?_)
        split
        · exact .nil
        · rename_i H₂ evs hu
          exact unwindLocs_blocks hu
    | loop e₁ =>
        simp only [eval]
        have hb := ih H φ e₁ hcc
        split
        · rename_i H₁ tr hr
          rw [hr] at hb
          exact Blocks.withTrace hb (ih H₁ φ (.loop e₁) (hok hcc hr).1)
        · exact .nil
        · rename_i H₁ sc tr hr
          rw [hr] at hb
          split
          · exact .nil
          · rename_i H₂ evs hu
            exact Blocks.append hb (unwindLocs_blocks hu)
        · exact hb

/-- **Every finished run's trace is in §6.11's block grammar** (§3.9, §6.11):
every destructor event of every run sits inside §6.11's walk of the drop
marker before it — the value's own destructor first (`3.9:28`), then its
fields in declaration order (`3.9:13`), an array's elements ascending
(`3.9:15`), an enum's active payload only (`6.3:20`), every `⊘` skipped —
and nowhere else. It needs only `DtorNotCopy`, which `WfDecls` gives. -/
theorem run_blocks (M : FloatOps) {P : Program} (hdt : DtorNotCopy P.decls) (fuel : Nat) :
    Blocks P.decls (run M P fuel).trace :=
  eval_blocks M hdt fuel [] _ _ (fun ℓ c hc => by simp at hc)

/-! ## Across cells: the scope records of a reachable configuration -/

/-- A scope record in **registration order is location order**: its cells
strictly increasing, every one below the store's length `n` (helper). -/
def Rec (n : Nat) (ls : List Nat) : Prop := ls.Pairwise (· < ·) ∧ ∀ ℓ ∈ ls, ℓ < n

/-- A longer store keeps a record ordered (helper). -/
theorem Rec.mono {n m : Nat} {ls : List Nat} (h : Rec n ls) (hn : n ≤ m) : Rec m ls :=
  ⟨h.1, fun ℓ hm => Nat.lt_of_lt_of_le (h.2 ℓ hm) hn⟩

/-- Part of a record, in its order, is ordered (helper). -/
theorem Rec.sublist {n : Nat} {ls ls' : List Nat} (h : Rec n ls) (hs : ls'.Sublist ls) :
    Rec n ls' :=
  ⟨h.1.sublist hs, fun ℓ hm => h.2 ℓ (hs.subset hm)⟩

/-- Fresh cells are allocated in increasing order (helper). Core's
`List.pairwise_lt_range'` costs `Classical.choice`, so this proof avoids it. -/
theorem range'_increasing : ∀ (s k : Nat), (List.range' s k).Pairwise (· < ·)
  | _, 0 => .nil
  | s, k + 1 => by
      rw [List.range'_succ]
      exact List.pairwise_cons.mpr
        ⟨fun a ha => Nat.lt_of_succ_le (List.mem_range'_1.mp ha).1, range'_increasing (s + 1) k⟩

/-- A record extended with freshly allocated cells, `n` onwards, is ordered
(helper). -/
theorem Rec.fresh {n k : Nat} {ls : List Nat} (h : Rec n ls) :
    Rec (n + k) (ls ++ List.range' n k) := by
  refine ⟨List.pairwise_append.mpr ⟨h.1, range'_increasing _ _, fun a ha b hb => ?_⟩,
    fun ℓ hm => ?_⟩
  · have := h.2 a ha; have := (List.mem_range'_1.mp hb).1; omega
  · rcases List.mem_append.mp hm with h' | h'
    · have := h.2 ℓ h'; omega
    · exact (List.mem_range'_1.mp h').2

/-- What a frame of the control stack owes, ordered: a pending `endscope`
marker's cells, and the scope record of a suspended caller (`ret(E, φ)`) or
of a loop boundary (`loopβ(e, φ)`) (helper). -/
def Kont.Ordered (n : Nat) : Kont → Prop
  | .endscope ls => Rec n ls
  | .loop _ φ => Rec n φ.scope
  | .call φ => Rec n φ.scope
  | _ => True

/-- A longer store keeps a frame ordered (helper). -/
theorem Kont.Ordered.mono {n m : Nat} {k : Kont} (h : k.Ordered n) (hn : n ≤ m) :
    k.Ordered m := by
  cases k <;> first | trivial | exact Rec.mono h hn

/-- **Every scope record of a configuration is in registration order**, which
is location order: the current frame's, and every one the control stack
holds (§6.1's `σ`, §6.7's `endscope`, §6.9's `ret(E, φ)`, §6.10's
`loopβ(e, φ)`). -/
def Config.Ordered : Config → Prop
  | .run H φ K _ _ => Rec H.length φ.scope ∧ ∀ k ∈ K, k.Ordered H.length
  | .panic _ _ => True

/-- A step that leaves the frame alone, grows or keeps the store, and pushes
only frames that owe nothing keeps the invariant (helper). -/
theorem Config.Ordered.keep {H H' : Store} {φ : Frame} {K K' : List Kont} {f f' : Focus}
    {tr tr' : List Event} (h : (Config.run H φ K f tr).Ordered) (hn : H.length ≤ H'.length)
    (hK : ∀ k ∈ K', k ∈ K ∨ ∀ n, k.Ordered n) : (Config.run H' φ K' f' tr').Ordered :=
  ⟨h.1.mono hn, fun k hk => by
    rcases hK k hk with h' | h'
    · exact (h.2 k h').mono hn
    · exact h' _⟩

/-- A step that keeps the stack (helper). -/
theorem Config.Ordered.same {H H' : Store} {φ : Frame} {K : List Kont} {f f' : Focus}
    {tr tr' : List Event} (h : (Config.run H φ K f tr).Ordered) (hn : H.length ≤ H'.length) :
    (Config.run H' φ K f' tr').Ordered :=
  h.keep hn (fun _ hk => .inl hk)

/-- A step that pushes a context frame, which owes nothing (helper). -/
theorem Config.Ordered.push {H H' : Store} {φ : Frame} {K : List Kont} {k : Kont} {f f' : Focus}
    {tr tr' : List Event} (h : (Config.run H φ K f tr).Ordered) (hn : H.length ≤ H'.length)
    (hk : ∀ n, k.Ordered n) : (Config.run H' φ (k :: K) f' tr').Ordered :=
  h.keep hn (fun _ hm => by
    rcases List.mem_cons.mp hm with rfl | hm
    · exact .inr hk
    · exact .inl hm)

/-- A step that pops a context frame (helper). -/
theorem Config.Ordered.pop {H H' : Store} {φ : Frame} {K : List Kont} {k : Kont} {f f' : Focus}
    {tr tr' : List Event} (h : (Config.run H φ (k :: K) f tr).Ordered)
    (hn : H.length ≤ H'.length) : (Config.run H' φ K f' tr').Ordered :=
  h.keep hn (fun _ hm => .inl (List.mem_cons_of_mem _ hm))

/-- The monitor-free drop-retire keeps the store's length (helper). -/
theorem plainDropRetire_length {D : Decls} {H H' : Store} {ℓ : Nat} {evs : List Event}
    (h : plainDropRetire D H ℓ = .ok (H', evs)) : H'.length = H.length := by
  unfold plainDropRetire at h
  split at h
  · cases h
  · cases h
  · split at h
    · cases h
    · cases h; simp

/-- The monitor-free unwind keeps the store's length (helper). -/
theorem plainUnwind_length {D : Decls} :
    ∀ {H H' : Store} {ls : List Nat} {evs : List Event},
      plainUnwind D H ls = .ok (H', evs) → H'.length = H.length
  | _, _, [], _, h => by simp [plainUnwind] at h; rw [h.1]
  | H, _, ℓ :: ls, _, h => by
      simp only [plainUnwind] at h
      split at h
      · cases h
      · rename_i H₁ evs₁ h₁
        split at h
        · cases h
        · rename_i H₂ evs₂ h₂
          cases h
          rw [plainUnwind_length h₂, plainDropRetire_length h₁]

/-- (D-Return)'s search: the caller's frame is on the stack, and what is left
under it was under it (helper). -/
theorem Kont.toCall_mem : ∀ {K K' : List Kont} {φ : Frame}, Kont.toCall K = some (φ, K') →
    Kont.call φ ∈ K ∧ ∀ k ∈ K', k ∈ K
  | [], _, _, h => by simp [Kont.toCall] at h
  | k :: K, K', φ, h => by
      cases k <;> simp only [Kont.toCall, Option.some.injEq, Prod.mk.injEq] at h
      case call φ' =>
        obtain ⟨rfl, rfl⟩ := h
        exact ⟨List.mem_cons_self, fun k hk => List.mem_cons_of_mem _ hk⟩
      all_goals
        obtain ⟨h₁, h₂⟩ := Kont.toCall_mem h
        exact ⟨List.mem_cons_of_mem _ h₁, fun k hk => List.mem_cons_of_mem _ (h₂ k hk)⟩

/-- (D-Break)'s search: the loop boundary is on the stack, and what is left
under it was under it (helper). -/
theorem Kont.toLoop_mem : ∀ {K K' : List Kont} {φ : Frame}, Kont.toLoop K = some (φ, K') →
    (∃ e, Kont.loop e φ ∈ K) ∧ ∀ k ∈ K', k ∈ K
  | [], _, _, h => by simp [Kont.toLoop] at h
  | k :: K, K', φ, h => by
      cases k <;> simp only [Kont.toLoop, Option.some.injEq, Prod.mk.injEq] at h
      case loop e φ' =>
        obtain ⟨rfl, rfl⟩ := h
        exact ⟨⟨e, List.mem_cons_self⟩, fun k hk => List.mem_cons_of_mem _ hk⟩
      case call => cases h
      all_goals
        obtain ⟨⟨e, h₁⟩, h₂⟩ := Kont.toLoop_mem h
        exact ⟨⟨e, List.mem_cons_of_mem _ h₁⟩, fun k hk => List.mem_cons_of_mem _ (h₂ k hk)⟩

/-- `mintParams`' store and cells, as the invariant reads them (helper). -/
theorem mintParams_eq {H H' : Store} {vs : List Val} {ls : List Nat}
    (h : mintParams H vs = (H', ls)) :
    H'.length = H.length + vs.length ∧ ls = List.range' H.length vs.length := by
  have h₁ := mintParams_length H vs
  have h₂ := mintParams_locs H vs
  rw [h] at h₁ h₂
  exact ⟨h₁, h₂⟩

/-- **Every step keeps every scope record in registration order** (§6.7,
§6.9, §6.10): a record is only ever extended with cells allocated at that
step — (D-Let)'s one, (D-Match)'s payload cells, (D-Call)'s parameter cells —
which are past every cell already in it, and only ever shortened from its
end ((D-EndScope)'s pop) or replaced by one the stack held. -/
theorem step_ordered {M : FloatOps} {P : Program} {C C' : Config} (h : Step M P C C')
    (hC : C.Ordered) : C'.Ordered := by
  cases h
  case «match» H φ K tr arms e k i vs body H' ls _ hm =>
    obtain ⟨hl, rfl⟩ := mintParams_eq hm
    refine ⟨hl ▸ hC.1.fresh, fun k hk => ?_⟩
    rcases List.mem_cons.mp hk with rfl | hk
    · exact hl ▸ (⟨range'_increasing _ _, fun ℓ hm => (List.mem_range'_1.mp hm).2⟩ :
        Rec (H.length + vs.length) (List.range' H.length vs.length))
    · exact (hC.2 k (List.mem_cons_of_mem _ hk)).mono (by omega)
  case letBind H φ K tr e₂ v =>
    refine ⟨by simpa using Rec.fresh (k := 1) hC.1, fun k hk => ?_⟩
    rcases List.mem_cons.mp hk with rfl | hk
    · exact ⟨List.pairwise_singleton _ _, fun ℓ hm => by simp at hm; simp [hm]⟩
    · exact (hC.2 k (List.mem_cons_of_mem _ hk)).mono (by simp)
  case endScope H φ K tr ℓs v H' evs hu =>
    have hl := plainUnwind_length hu
    refine ⟨(hC.1.sublist (List.take_sublist _ _)).mono (by omega), fun k hk => ?_⟩
    exact (hC.2 k (List.mem_cons_of_mem _ hk)).mono (by omega)
  case call H φ K tr f vs fd H' ls _ _ hm =>
    obtain ⟨hl, rfl⟩ := mintParams_eq hm
    have h0 : Rec H.length [] := ⟨.nil, by simp⟩
    refine ⟨by simpa [hl] using Rec.fresh (k := vs.length) h0, fun k hk => ?_⟩
    rcases List.mem_cons.mp hk with rfl | hk
    · exact hC.1.mono (by omega)
    · exact (hC.2 k hk).mono (by omega)
  case callReturn H φ K tr φs v H' evs hu =>
    have hl := plainUnwind_length hu
    exact ⟨(hC.2 _ List.mem_cons_self).mono (by omega),
      fun k hk => (hC.2 k (List.mem_cons_of_mem _ hk)).mono (by omega)⟩
  case ret H φ K tr v φs K' H' evs hk hu =>
    have hl := plainUnwind_length hu
    obtain ⟨h₁, h₂⟩ := Kont.toCall_mem hk
    exact ⟨(hC.2 _ (List.mem_cons_of_mem _ h₁)).mono (by omega),
      fun k hk => (hC.2 k (List.mem_cons_of_mem _ (h₂ k hk))).mono (by omega)⟩
  case loopEnter H φ K tr e =>
    refine ⟨hC.1, fun k hk => ?_⟩
    rcases List.mem_cons.mp hk with rfl | hk
    · exact hC.1
    · exact hC.2 k hk
  case loopIter H φ K tr e φs H' evs hu =>
    have hl := plainUnwind_length hu
    have hφs := hC.2 _ List.mem_cons_self
    exact ⟨(show Rec H.length φs.scope from hφs).mono (by omega),
      fun k hk => (hC.2 k hk).mono (by omega)⟩
  case brk H φ K tr φs K' H' evs hk hu =>
    have hl := plainUnwind_length hu
    obtain ⟨⟨e, h₁⟩, h₂⟩ := Kont.toLoop_mem hk
    exact ⟨(show Rec H.length φs.scope from hC.2 _ h₁).mono (by omega),
      fun k hk => (hC.2 k (h₂ k hk)).mono (by omega)⟩
  all_goals first
    | trivial
    | exact hC.same (by first | exact Nat.le_refl _ | simp)
    | exact hC.push (by first | exact Nat.le_refl _ | simp) (fun _ => trivial)
    | exact hC.pop (by first | exact Nat.le_refl _ | simp)
    | exact (hC.pop (f' := .ret .unit) (tr' := []) (Nat.le_refl _)).push (Nat.le_refl _)
        (fun _ => trivial)

/-- **Registration order is location order, everywhere the machine goes**
(§6.1, §6.7, §6.9, §6.10): in every configuration reachable from §6.12's
initial one, every scope record — the current frame's, every suspended
caller's and loop boundary's, and every pending `endscope` marker's — lists
its cells in strictly increasing location order. No typing hypothesis. -/
theorem reachable_ordered {M : FloatOps} {P : Program} {C : Config}
    (h : Steps M P Config.init C) : C.Ordered := by
  have key : ∀ {C₁ C₂ : Config}, Steps M P C₁ C₂ → C₁.Ordered → C₂.Ordered := by
    intro C₁ C₂ hs
    induction hs with
    | refl => exact id
    | step h₁ _ ih => exact fun hC => ih (step_ordered h₁ hC)
  exact key h ⟨⟨.nil, by simp⟩, by simp⟩

/-! ## Across cells: every step's drops are newest-first -/

/-- The cells a trace's `drop` markers name, in trace order (helper). -/
def dropLocs (tr : List Event) : List Nat :=
  tr.filterMap fun | .drop ℓ _ => some ℓ | _ => none

/-- `dropLocs` distributes over concatenation (helper). -/
theorem dropLocs_append (l₁ l₂ : List Event) : dropLocs (l₁ ++ l₂) = dropLocs l₁ ++ dropLocs l₂ :=
  List.filterMap_append

mutual
/-- §6.11's walk names no cell: it emits destructor events only (helper). -/
theorem dropLocs_dropEvents (D : Decls) : ∀ c : Contents, dropLocs (dropEvents D c) = []
  | .hole | .int _ _ _ | .float _ _ | .bool _ | .unit => rfl
  | .struct s i cs => by
      simp only [dropEvents, dropLocs_append, dropLocs_dropEventsList D cs, List.append_nil]
      split
      · split <;> rfl
      · rfl
  | .enum _ _ _ cs | .array _ _ cs => by simp only [dropEvents]; exact dropLocs_dropEventsList D cs

/-- The same over a list (helper). -/
theorem dropLocs_dropEventsList (D : Decls) : ∀ cs : List Contents,
    dropLocs (dropEventsList D cs) = []
  | [] => rfl
  | c :: cs => by
      simp only [dropEventsList, dropLocs_append, dropLocs_dropEvents D c,
        dropLocs_dropEventsList D cs, List.append_nil]
end

/-- A binding's drop names its own cell once, or nothing for `Copy` contents
(helper). -/
theorem dropCell_locs' {D : Decls} {ℓ : Nat} {c : Contents} {evs : List Event}
    (h : dropCell D ℓ c = .ok evs) : dropLocs evs = [] ∨ dropLocs evs = [ℓ] := by
  unfold dropCell at h
  split at h
  · cases h; exact .inl rfl
  · split at h
    · cases h
    · rename_i evs' hw
      cases h
      refine .inr ?_
      rw [dropContents_eq hw,
        show (Event.drop ℓ c :: dropEvents D c) = [.drop ℓ c] ++ dropEvents D c from rfl,
        dropLocs_append, dropLocs_dropEvents]
      rfl

/-- A binding's drop names at most its own cell (helper). -/
theorem dropCell_locs {D : Decls} {ℓ : Nat} {c : Contents} {evs : List Event}
    (h : dropCell D ℓ c = .ok evs) : ∀ x ∈ dropLocs evs, x = ℓ := by
  intro x hx
  rcases dropCell_locs' h with h' | h' <;> rw [h'] at hx <;> simp_all

/-- A destructure's residue drops name only the destructured cell (helper). -/
theorem plainResidue_locs {D : Decls} {ℓ : Nat} : ∀ {rs : List Contents} {evs : List Event},
    plainResidue D ℓ rs = .ok evs → ∀ x ∈ dropLocs evs, x = ℓ
  | [], _, h => by simp [plainResidue] at h; subst h; simp [dropLocs]
  | r :: rs, _, h => by
      simp only [plainResidue] at h
      split at h
      · cases h
      · rename_i e₁ h₁
        split at h
        · cases h
        · rename_i e₂ h₂
          cases h
          intro x hx
          simp only [dropLocs_append, dropContents_eq h₁, dropLocs_dropEvents, List.append_nil,
            List.mem_append] at hx
          rcases hx with hx | hx
          · unfold residueMark at hx
            split at hx
            · simp [dropLocs] at hx
            · simpa [dropLocs] using hx
          · exact plainResidue_locs h₂ x hx

/-- §6.3's destructure names only the destructured cell (helper). -/
theorem plainDestructure_locs {D : Decls} {ℓ : Nat} {c leaf : Contents} {πs : List Nat}
    {evs : List Event} (h : plainDestructure D ℓ c πs = .ok (leaf, evs)) :
    ∀ x ∈ dropLocs evs, x = ℓ := by
  unfold plainDestructure at h
  split at h
  · cases h
  · split at h
    · cases h
    · rename_i evs' hr
      cases h
      intro x hx
      simp only [dropLocs_append, List.mem_append] at hx
      rcases hx with hx | hx
      · exact plainResidue_locs hr x hx
      · simp [dropLocs] at hx

/-- `run-scope-drops` names the cells it is given, in the order given, each
at most once (helper). -/
theorem plainUnwind_locs {D : Decls} : ∀ {H H' : Store} {ls : List Nat} {evs : List Event},
    plainUnwind D H ls = .ok (H', evs) → (dropLocs evs).Sublist ls
  | _, _, [], _, h => by
      simp [plainUnwind] at h; obtain ⟨_, rfl⟩ := h; exact .slnil
  | H, _, ℓ :: ls, _, h => by
      simp only [plainUnwind] at h
      split at h
      · cases h
      · rename_i H₁ evs₁ h₁
        split at h
        · cases h
        · rename_i H₂ evs₂ h₂
          cases h
          have ih := plainUnwind_locs h₂
          have hd : dropLocs evs₁ = [] ∨ dropLocs evs₁ = [ℓ] := by
            unfold plainDropRetire at h₁
            split at h₁
            · cases h₁
            · cases h₁
            · split at h₁
              · cases h₁
              · rename_i hd
                cases h₁
                exact dropCell_locs' hd
          rw [dropLocs_append]
          rcases hd with hd | hd <;> rw [hd]
          · exact ih.cons ℓ
          · exact ih.cons_cons ℓ

/-- **The order one step drops cells in**: its `drop` markers either all name
one cell — an overwrite, an `@drop`, a destructure's residue, several
sub-positions of one binding — or name distinct cells in strictly decreasing
location order (helper). -/
def NewestFirst (ls : List Nat) : Prop := (∃ ℓ, ∀ x ∈ ls, x = ℓ) ∨ ls.Pairwise (· > ·)

/-- A teardown of an ordered record drops newest-first (helper). -/
theorem NewestFirst.teardown {n : Nat} {ls ls' : List Nat} (h : Rec n ls)
    (hs : ls'.Sublist ls.reverse) : NewestFirst ls' :=
  .inr ((List.pairwise_reverse.mpr h.1).sublist hs)

/-- The output a configuration has produced so far (§6.12) (helper). -/
def Config.trace : Config → List Event
  | .run _ _ _ _ tr => tr
  | .panic _ tr => tr

/-- **Every step of §6's relation from an ordered configuration drops
newest-first** (§6.7, §6.9, §6.10, §6.11): it appends to the trace, and the
`drop` markers it appends name one cell or name distinct cells in strictly
decreasing location order. A teardown — (D-EndScope), (D-Return-Value)'s
frame pop, (D-Return)'s σ-walk, (D-Loop-Iter)'s end of a turn and
(D-Break)'s unwind — walks an ordered record backwards
(`NewestFirst.teardown`). -/
theorem step_drop_order {M : FloatOps} {P : Program} {C C' : Config} (h : Step M P C C')
    (hC : C.Ordered) : ∃ evs, C'.trace = C.trace ++ evs ∧ NewestFirst (dropLocs evs) := by
  have one : ∀ {evs : List Event} (ℓ : Nat), (∀ x ∈ dropLocs evs, x = ℓ) →
      NewestFirst (dropLocs evs) := fun ℓ h => .inl ⟨ℓ, h⟩
  have none : ∀ {evs : List Event}, dropLocs evs = [] → NewestFirst (dropLocs evs) :=
    fun h => .inl ⟨0, by simp [h]⟩
  cases h
  case useDeclared H φ K tr p ℓ c πd πs cd leaf evs v c' _ _ _ hd _ _ =>
    exact ⟨_, rfl, one ℓ (plainDestructure_locs hd)⟩
  case dbg => exact ⟨_, rfl, none rfl⟩
  case «match» =>
    refine ⟨_, rfl, none ?_⟩
    unfold matchConsume; split <;> rfl
  case endScope H φ K tr ℓs v H' evs hu =>
    exact ⟨_, rfl, NewestFirst.teardown (hC.2 _ List.mem_cons_self) (plainUnwind_locs hu)⟩
  case seqDrop hd =>
    refine ⟨_, rfl, none ?_⟩
    rw [show ∀ v evs, (Event.dropTemp v :: evs) = [.dropTemp v] ++ evs from fun _ _ => rfl,
      dropLocs_append, dropContents_eq hd, dropLocs_dropEvents]; rfl
  case assign H φ K tr p v ℓ c old evs c' _ _ hd _ => exact ⟨_, rfl, one ℓ (dropCell_locs hd)⟩
  case indexWrite H φ K tr p πs v vs ℓ c sub ρ old evs sub' c' _ _ hd _ _ => exact ⟨_, rfl, one ℓ (dropCell_locs hd)⟩
  case dropDeclared H φ K tr p ℓ c πd πs cd leaf evs levs c' _ _ _ hd hl _ =>
    refine ⟨_, rfl, one ℓ (fun x hx => ?_)⟩
    rw [dropLocs_append, List.mem_append] at hx
    rcases hx with hx | hx
    · exact plainDestructure_locs hd x hx
    · exact dropCell_locs hl x hx
  case dropMove H φ K tr p ℓ c sub evs c' _ _ _ _ hd _ => exact ⟨_, rfl, one ℓ (dropCell_locs hd)⟩
  case callReturn H φ K tr φs v H' evs hu =>
    exact ⟨_, rfl, NewestFirst.teardown hC.1 (plainUnwind_locs hu)⟩
  case ret hu => exact ⟨_, rfl, NewestFirst.teardown hC.1 (plainUnwind_locs hu)⟩
  case loopIter hu =>
    exact ⟨_, rfl, NewestFirst.teardown (hC.1.sublist (List.drop_sublist _ _))
      (plainUnwind_locs hu)⟩
  case brk hu =>
    exact ⟨_, rfl, NewestFirst.teardown (hC.1.sublist (List.drop_sublist _ _))
      (plainUnwind_locs hu)⟩
  all_goals exact ⟨[], by simp [Config.trace], none rfl⟩

/-- **Newest-first teardown, on every reachable step** (§6.7, §6.9, §6.10):
from every configuration reachable from §6.12's initial one, every step's
`drop` markers name one cell or distinct cells newest-first. No typing
hypothesis. -/
theorem reachable_drop_order {M : FloatOps} {P : Program} {C C' : Config}
    (hr : Steps M P Config.init C) (h : Step M P C C') :
    ∃ evs, C'.trace = C.trace ++ evs ∧ NewestFirst (dropLocs evs) :=
  step_drop_order h (reachable_ordered hr)

/-! ## Across steps: scopes nest, and teardown is last-in first-out

`step_drop_order` orders the drops of one step. A source block
`{ let a; let b; }` exits over **two** (D-EndScope) steps, so ordering them
needs more: that the pending `endscope` markers are exactly the tail of the
scope record, innermost last (`Config.Nested`). Then every teardown removes a
suffix of the machine's whole registration stack — the suspended callers'
records, then the current frame's — and drops cells only from that suffix,
newest first (`Lifo`). Since the stack is in location order, a cell a
teardown deregisters is newer than every cell still registered
(`Lifo.newer`): across steps, across scopes and across frames, the machine
drops last-in first-out. -/

/-- The scope record the pending `endscope` markers and loop boundaries of
one frame account for (§6.7, §6.10): reading the stack top-down, each
`endscope ℓs` is the tail of what is left of the record, a loop boundary
`loopβ(e, φs)` has exactly `φs`'s record left, and a caller's frame
`ret(E, φs)` starts the same reading over for the caller's record `φs`
(helper). -/
def Nest : List Nat → List Kont → Prop
  | _, [] => True
  | sc, .endscope ls :: K => ∃ sc', sc = sc' ++ ls ∧ Nest sc' K
  | sc, .loop _ φs :: K => sc = φs.scope ∧ Nest φs.scope K
  | _, .call φs :: K => Nest φs.scope K
  | sc, _ :: K => Nest sc K

/-- The registration records of the suspended callers, bottom of the stack
first (§6.9's `ret(E, φ)` frames) (helper). -/
def Stk : List Kont → List Nat
  | [] => []
  | .call φs :: K => Stk K ++ φs.scope
  | _ :: K => Stk K

/-- The machine's whole **registration stack**: every suspended caller's
scope record, bottom first, then the current frame's (§6.1's `σ` per frame);
empty at a trap (helper). -/
def Config.stack : Config → List Nat
  | .run _ φ K _ _ => Stk K ++ φ.scope
  | .panic _ _ => []

/-- **Scopes nest** (§6.7, §6.9, §6.10): every frame's pending `endscope`
markers are exactly the tail of its scope record, innermost last, and the
whole registration stack is in location order, below the store's length. -/
def Config.Nested : Config → Prop
  | .run H φ K _ _ => Nest φ.scope K ∧ Rec H.length (Stk K ++ φ.scope)
  | .panic _ _ => True

/-- (D-EndScope)'s pop by count removes exactly the marker's cells when they
are the record's tail (helper). -/
theorem Frame.popScope_tail (φ : Frame) {sc ls : List Nat} (h : φ.scope = sc ++ ls) :
    (φ.popScope ls.length).scope = sc := by
  simp [Frame.popScope, h]

/-- (D-Return)'s search, read by the nesting: the caller's record is the
next one, and the stack below it is the rest (helper). -/
theorem Nest.toCall {K K' : List Kont} {φ : Frame} :
    ∀ {sc : List Nat}, Nest sc K → Kont.toCall K = some (φ, K') →
      Nest φ.scope K' ∧ Stk K = Stk K' ++ φ.scope := by
  induction K with
  | nil => intro _ _ h; simp [Kont.toCall] at h
  | cons k K ih =>
      intro sc hn h
      cases k
      case call φ' =>
        simp only [Kont.toCall, Option.some.injEq, Prod.mk.injEq] at h
        obtain ⟨rfl, rfl⟩ := h
        exact ⟨hn, rfl⟩
      case endscope ls =>
        obtain ⟨sc', _, hn'⟩ := hn
        exact ih hn' h
      case loop e φ' => exact ih hn.2 h
      all_goals exact ih hn h

/-- (D-Break)'s search, read by the nesting: the loop boundary's record is a
prefix of the frame's, the rest being the cells the body registered, and no
caller's record is crossed (helper). -/
theorem Nest.toLoop {K K' : List Kont} {φ : Frame} :
    ∀ {sc : List Nat}, Nest sc K → Kont.toLoop K = some (φ, K') →
      (∃ m, sc = φ.scope ++ m) ∧ Nest φ.scope K' ∧ Stk K = Stk K' := by
  induction K with
  | nil => intro _ _ h; simp [Kont.toLoop] at h
  | cons k K ih =>
      intro sc hn h
      cases k
      case loop e φ' =>
        simp only [Kont.toLoop, Option.some.injEq, Prod.mk.injEq] at h
        obtain ⟨rfl, rfl⟩ := h
        exact ⟨⟨[], by simp [hn.1]⟩, hn.2, rfl⟩
      case call => simp [Kont.toLoop] at h
      case endscope ls =>
        obtain ⟨sc', hsc, hn'⟩ := hn
        obtain ⟨⟨m, hm⟩, h₂, h₃⟩ := ih hn' h
        exact ⟨⟨m ++ ls, by rw [hsc, hm, List.append_assoc]⟩, h₂, h₃⟩
      all_goals exact ih hn h

/-- **How one step changes the registration stack, and what it drops**: it
either keeps the stack as a prefix of the new one — nothing deregistered —
or cuts the stack back to a prefix of the old one, and its `drop` markers
then name only cells of the suffix it cut, newest first (helper). -/
def Lifo (S S' : List Nat) (ls : List Nat) : Prop :=
  S <+: S' ∨ (S' <+: S ∧ ls.Sublist (S.drop S'.length).reverse)

/-- **What a teardown deregisters is newer than everything still
registered**: on a stack in location order, a step that cuts the stack back
drops only cells it deregistered, each newer than every cell still
registered (helper). -/
theorem Lifo.newer {S S' ls : List Nat} (hS : S.Pairwise (· < ·)) (h : Lifo S S' ls)
    (hcut : ¬ S <+: S') : ∀ ℓ ∈ ls, ℓ ∉ S' ∧ ∀ ℓ' ∈ S', ℓ' < ℓ := by
  rcases h with h | ⟨⟨rest, rfl⟩, hs⟩
  · exact absurd h hcut
  · intro ℓ hℓ
    have hr : ℓ ∈ rest := by
      have := hs.subset hℓ
      simpa using this
    have hlt : ∀ ℓ' ∈ S', ℓ' < ℓ := fun ℓ' hℓ' =>
      (List.pairwise_append.mp hS).2.2 ℓ' hℓ' ℓ hr
    exact ⟨fun hm => Nat.lt_irrefl ℓ (hlt ℓ hm), hlt⟩

/-- A step that keeps the frame and the callers keeps the stack (helper). -/
theorem Lifo.same {S ls : List Nat} : Lifo S S ls := .inl (List.prefix_refl S)

/-- A teardown of the current frame's tail (helper). -/
theorem Lifo.cut {A m ls : List Nat} (h : ls.Sublist m.reverse) : Lifo (A ++ m) A ls :=
  .inr ⟨List.prefix_append A m, by simpa using h⟩

/-- **Every step keeps the scopes nested** (§6.7, §6.9, §6.10): (D-Let) and
(D-Match) push a marker equal to the cells they append to the record,
(D-EndScope) pops both together, a call starts a frame of its own, and a
return, a loop turn's end and a `break` restore a record the stack held. -/
theorem step_nested {M : FloatOps} {P : Program} {C C' : Config} (h : Step M P C C')
    (hC : C.Nested) : C'.Nested := by
  cases h
  case «match» H φ K tr arms e k i vs body H' ls _ hm =>
    obtain ⟨hl, rfl⟩ := mintParams_eq hm
    refine ⟨⟨φ.scope, rfl, hC.1⟩, ?_⟩
    rw [← List.append_assoc, hl]; exact hC.2.fresh
  case letBind H φ K tr e₂ v =>
    refine ⟨⟨φ.scope, rfl, hC.1⟩, ?_⟩
    have h1 : Rec (H.length + 1) (Stk K ++ φ.scope ++ List.range' H.length 1) :=
      Rec.fresh (k := 1) hC.2
    rw [← List.append_assoc]; exact h1.mono (by simp)
  case endScope H φ K tr ℓs v H' evs hu =>
    have hl := plainUnwind_length hu
    obtain ⟨sc', hsc, hn⟩ := hC.1
    have hp := Frame.popScope_tail φ hsc
    have h2 : Rec H.length (Stk K ++ φ.scope) := hC.2
    refine ⟨by rw [hp]; exact hn, ?_⟩
    show Rec H'.length (Stk K ++ (φ.popScope ℓs.length).scope)
    rw [hp]
    refine (h2.sublist ?_).mono (by omega)
    rw [hsc, ← List.append_assoc]; exact List.sublist_append_left _ _
  case call H φ K tr f vs fd H' ls _ _ hm =>
    obtain ⟨hl, rfl⟩ := mintParams_eq hm
    refine ⟨hC.1, ?_⟩
    simp only [Stk]; rw [hl]; exact hC.2.fresh
  case callReturn H φ K tr φs v H' evs hu =>
    have hl := plainUnwind_length hu
    have h2 : Rec H.length (Stk K ++ φs.scope ++ φ.scope) := hC.2
    exact ⟨hC.1, (h2.sublist (List.sublist_append_left _ _)).mono (by omega)⟩
  case ret H φ K tr v φs K' H' evs hk hu =>
    have hl := plainUnwind_length hu
    obtain ⟨hn, hs⟩ := Nest.toCall (show Nest φ.scope K from hC.1) hk
    have h2 : Rec H.length (Stk K ++ φ.scope) := hC.2
    refine ⟨hn, (h2.sublist ?_).mono (by omega)⟩
    rw [hs]; exact List.sublist_append_left _ _
  case loopEnter H φ K tr e => exact ⟨⟨rfl, hC.1⟩, hC.2⟩
  case loopIter H φ K tr e φs H' evs hu =>
    have hl := plainUnwind_length hu
    obtain ⟨heq, hn⟩ := hC.1
    refine ⟨⟨rfl, hn⟩, ?_⟩
    have := hC.2.mono (show H.length ≤ H'.length by omega)
    simpa [Stk, heq] using this
  case brk H φ K tr φs K' H' evs hk hu =>
    have hl := plainUnwind_length hu
    obtain ⟨⟨m, hm⟩, hn, hs⟩ := Nest.toLoop hC.1 hk
    refine ⟨hn, (hC.2.sublist ?_).mono (by omega)⟩
    rw [hs, hm, ← List.append_assoc]; exact List.sublist_append_left _ _
  all_goals first
    | trivial
    | exact ⟨hC.1, hC.2.mono (by first | exact Nat.le_refl _ | simp)⟩

/-- **The nesting holds everywhere the machine goes** (§6.7, §6.9, §6.10):
in every configuration reachable from §6.12's initial one. In particular
(D-EndScope)'s pop by count always removes the marker's own cells
(`Frame.popScope_tail`). No typing hypothesis. -/
theorem reachable_nested {M : FloatOps} {P : Program} {C : Config}
    (h : Steps M P Config.init C) : C.Nested := by
  have key : ∀ {C₁ C₂ : Config}, Steps M P C₁ C₂ → C₁.Nested → C₂.Nested := by
    intro C₁ C₂ hs
    induction hs with
    | refl => exact id
    | step h₁ _ ih => exact fun hC => ih (step_nested h₁ hC)
  exact key h ⟨trivial, ⟨.nil, by simp [Stk]⟩⟩

/-- **Every step is last-in first-out** (§6.7, §6.9, §6.10): from a nested
configuration, a step appends `evs` to the trace and either keeps the
registration stack as a prefix of the new one, or cuts it back and drops
only cells of the cut suffix, newest first (`Lifo`). -/
theorem step_lifo {M : FloatOps} {P : Program} {C C' : Config} (h : Step M P C C')
    (hC : C.Nested) :
    ∃ evs, C'.trace = C.trace ++ evs ∧ Lifo C.stack C'.stack (dropLocs evs) := by
  have keep : ∀ {evs : List Event} {S S' : List Nat}, S <+: S' → Lifo S S' (dropLocs evs) :=
    fun h => .inl h
  have pre : ∀ A B D : List Nat, A ++ B <+: A ++ (B ++ D) := fun A B D => by
    rw [← List.append_assoc]; exact List.prefix_append _ _
  cases h
  case «match» => exact ⟨_, rfl, keep (pre _ _ _)⟩
  case letBind => exact ⟨[], by simp [Config.trace], keep (pre _ _ _)⟩
  case endScope H φ K tr ℓs v H' evs hu =>
    obtain ⟨sc', hsc, _⟩ := hC.1
    refine ⟨_, rfl, ?_⟩
    simp only [Config.stack, Stk, Frame.popScope_tail φ hsc, hsc, ← List.append_assoc]
    exact Lifo.cut (plainUnwind_locs hu)
  case call => exact ⟨[], by simp [Config.trace], keep (List.prefix_append _ _)⟩
  case callReturn H φ K tr φs v H' evs hu =>
    refine ⟨_, rfl, ?_⟩
    simp only [Config.stack, Stk]
    exact Lifo.cut (plainUnwind_locs hu)
  case ret H φ K tr v φs K' H' evs hk hu =>
    obtain ⟨_, hs⟩ := Nest.toCall hC.1 hk
    refine ⟨_, rfl, ?_⟩
    simp only [Config.stack, hs]
    exact Lifo.cut (plainUnwind_locs hu)
  case loopIter H φ K tr e φs H' evs hu =>
    obtain ⟨heq, _⟩ := hC.1
    refine ⟨_, rfl, ?_⟩
    simp only [Config.stack, Stk, heq]
    exact Lifo.same
  case brk H φ K tr φs K' H' evs hk hu =>
    obtain ⟨⟨m, hm⟩, _, hs⟩ := Nest.toLoop hC.1 hk
    refine ⟨_, rfl, ?_⟩
    have hd : φ.scope.drop φs.scope.length = m := by rw [hm]; simp
    rw [hd] at hu
    simp only [Config.stack, hs, hm, ← List.append_assoc]
    exact Lifo.cut (plainUnwind_locs hu)
  all_goals first
    | exact ⟨_, rfl, Lifo.same⟩
    | exact ⟨[], by simp [Config.trace], Lifo.same⟩
    | exact ⟨[], by simp [Config.trace], .inr ⟨List.nil_prefix, List.nil_sublist _⟩⟩

/-- **Last-in first-out, on every reachable step** (§6.7, §6.9, §6.10): the
registration stack is in location order, and a step that deregisters cells
drops only cells it deregistered, each newer than every cell still
registered. So a cell dropped at one teardown and a cell dropped at a later
one, still registered at the first, drop newest first: `{ let a; let b; }`
drops `b` before `a` over its two (D-EndScope) steps. No typing
hypothesis. -/
theorem reachable_lifo {M : FloatOps} {P : Program} {C C' : Config}
    (hr : Steps M P Config.init C) (h : Step M P C C') :
    ∃ evs, C'.trace = C.trace ++ evs ∧ Lifo C.stack C'.stack (dropLocs evs) ∧
      (¬ C.stack <+: C'.stack → ∀ ℓ ∈ dropLocs evs, ℓ ∉ C'.stack ∧ ∀ ℓ' ∈ C'.stack, ℓ' < ℓ) := by
  have hn := reachable_nested hr
  obtain ⟨evs, ht, hl⟩ := step_lifo h hn
  refine ⟨evs, ht, hl, fun hcut => hl.newer ?_ hcut⟩
  cases C with
  | run H φ K f tr => exact hn.2.1
  | panic => exact .nil

/-! ## `drop_order` -/

/-- `Blocks` on §6's terminal configurations: a finished `Step` run's trace
is the one `eval` answers (`eval_complete`), so it is in the block grammar
(helper). -/
theorem step_blocks (M : FloatModel) {P : Program} (h : ProgramTyped P) :
    (∀ H φ v tr, Steps M.toFloatOps P Config.init (.run H φ [] (.ret v) tr) → Blocks P.decls tr) ∧
    (∀ κ tr, Steps M.toFloatOps P Config.init (.panic κ tr) → Blocks P.decls tr) := by
  obtain ⟨hv, hp⟩ := eval_complete M h
  have hdt := h.wf.decls.dtorNotCopy
  refine ⟨fun H φ v tr hs => ?_, fun κ tr hs => ?_⟩
  · obtain ⟨n, hn⟩ := hv H φ v tr hs
    have := run_blocks M.toFloatOps hdt (n + 1)
    rw [hn (n + 1) (Nat.lt_succ_self n)] at this
    exact this
  · obtain ⟨n, hn⟩ := hp κ tr hs
    have := run_blocks M.toFloatOps hdt (n + 1)
    rw [hn (n + 1) (Nat.lt_succ_self n)] at this
    exact this

/-- **Drop order** (§3.9, §6.7, §6.9, §6.10, §6.11; §7's "no use-after-drop /
no leak of drops" bullet, its *when*), over §6's relation, for a program the
checker accepts:

* **within a value**: every finished run's trace — a terminal value or a
  trap — is in §6.11's block grammar (`step_blocks`): every destructor event
  sits inside the walk of the drop marker before it — destructor first
  (`3.9:28`), fields in declaration order (`3.9:13`), array elements
  ascending (`3.9:15`), an enum's stored (active) payload only (`6.3:20`) —
  and nowhere else;
* **across cells**, at every step from a reachable configuration: its `drop`
  markers name one cell or distinct cells newest first (`NewestFirst`), and
  the step is last-in first-out on the registration stack (`Lifo`): it keeps
  the stack, or cuts it back and drops only cells it deregistered — each
  newer than every cell still registered, by `reachable_nested`'s location
  order (`reachable_lifo`, `Lifo.newer`). So across all its exit steps a
  scope's cells drop newest first, and before any older scope's.

Only the first half reads the typing hypothesis, through `eval_complete` and
`DtorNotCopy`. -/
theorem drop_order (M : FloatModel) {P : Program} (h : ProgramTyped P) :
    (∀ H φ v tr, Steps M.toFloatOps P Config.init (.run H φ [] (.ret v) tr) → Blocks P.decls tr) ∧
    (∀ κ tr, Steps M.toFloatOps P Config.init (.panic κ tr) → Blocks P.decls tr) ∧
    ∀ C C', Steps M.toFloatOps P Config.init C → Step M.toFloatOps P C C' →
      ∃ evs, C'.trace = C.trace ++ evs ∧ NewestFirst (dropLocs evs) ∧
        Lifo C.stack C'.stack (dropLocs evs) ∧ (C.stack.Pairwise (· < ·)) := by
  refine ⟨(step_blocks M h).1, (step_blocks M h).2, fun C C' hr hs => ?_⟩
  obtain ⟨evs, ht, hn⟩ := reachable_drop_order hr hs
  obtain ⟨evs', ht', hl⟩ := step_lifo hs (reachable_nested hr)
  have : evs' = evs := List.append_cancel_left (ht'.symm.trans ht)
  subst this
  refine ⟨evs', ht, hn, hl, ?_⟩
  have hN := reachable_nested hr
  cases C with
  | run H φ K f tr => exact hN.2.1
  | panic => exact .nil

/-! ## What the statements reject

Each half rejects something a wrong machine would produce. -/

/-- Inverting a drop block: what follows a marker is §6.11's walk of what it
names (helper). -/
theorem Blocks.drop_inv {D : Decls} {ℓ : Nat} {c : Contents} {t : List Event}
    (h : Blocks D (.drop ℓ c :: t)) : ∃ t', t = dropEvents D c ++ t' ∧ Blocks D t' := by
  cases h with
  | drop h' => exact ⟨_, rfl, h'⟩

/-- **No destructor outside a drop**: a trace that opens with a destructor
event is not in the grammar (§6.11). -/
theorem Blocks.not_dtor {D : Decls} {s : Nat} {c : Contents} {t : List Event} :
    ¬ Blocks D (.dtor s c :: t) := by
  intro h; cases h

open Examples in
/-- **Fields in declaration order, or the grammar rejects the trace**
(`3.9:13`): `struct_field_drop_order` drops `S7 { S1 {1}#0, S1 {2}#1 }#2`,
whose walk runs `#0`'s destructor and then `#1`'s. The same marker followed by
the two destructors the other way round is not in the grammar. -/
theorem fieldsSwapped_rejected :
    ¬ Blocks (prog tI64 structFieldOrder).decls
      [.drop 3 (.struct sTwoAffine 2 [cA 0 1, cA 1 2]), .dtor sAffine (cA 1 2),
        .dtor sAffine (cA 0 1)] := by
  intro h
  obtain ⟨t', ht, _⟩ := Blocks.drop_inv h
  have : dropEvents (prog tI64 structFieldOrder).decls (.struct sTwoAffine 2 [cA 0 1, cA 1 2])
      = [.dtor sAffine (cA 0 1), .dtor sAffine (cA 1 2)] := by rfl
  rw [this] at ht
  simp [cA, sAffine] at ht

open Examples in
/-- **Newest-first on a reachable step** (§6.9's (D-Return)):
`return_past_affine` returns past two live affine bindings, `ℓ1` then `ℓ3`,
and the one step that runs the frame's σ-walk drops `ℓ3` then `ℓ1` — two
distinct cells, newest first, the teardown `reachable_drop_order` speaks
of. -/
theorem returnPastAffine_newestFirst :
    ∃ C C' evs, Steps demoOps returnPastAffine Config.init C ∧
      Step demoOps returnPastAffine C C' ∧ C'.trace = C.trace ++ evs ∧ dropLocs evs = [3, 1] :=
  ⟨stepN demoOps returnPastAffine 18 Config.init, _, _, stepN_steps,
    step_iff.mpr rfl, rfl, rfl⟩

open Examples in
/-- **The invariant is what orders a teardown.** A frame whose scope record
is *not* in location order — `[3, 1]`, a configuration `reachable_ordered`
says no run reaches — pops (D-Return-Value) and drops `ℓ1` before `ℓ3`: the
step is a real step of §6's relation, and its markers are not newest-first.
So `step_drop_order`'s hypothesis is load-bearing, and `reachable_ordered`
is what discharges it. -/
theorem unorderedRecord_rejected :
    ∃ C C' evs, ¬ C.Ordered ∧ Step demoOps returnPastAffine C C' ∧
      C'.trace = C.trace ++ evs ∧ ¬ NewestFirst (dropLocs evs) := by
  refine ⟨.run [.dead, .full (cA 0 3), .dead, .full (cA 2 4)] { env := [3, 1], scope := [3, 1] }
      [.call { env := [], scope := [] }] (.ret (v64 7)) [], _, _,
    fun h => ?_, .callReturn rfl, rfl, fun h => ?_⟩
  · have := h.1.1; simp at this
  · change NewestFirst [1, 3] at h
    rcases h with ⟨ℓ, hℓ⟩ | h
    · have h1 := hℓ 1 (by decide); have h3 := hℓ 3 (by decide); omega
    · simp at h

open Examples in
/-- Two nested `let`s' pending markers, swapped: `endscope [1]` above
`endscope [3]` in a frame whose record is `[1, 3]` (the review's Probe 2)
(helper). -/
def swappedMarkers : Config :=
  .run [.dead, .full (cA 0 3), .dead, .full (cA 2 4)] { env := [3, 1], scope := [1, 3] }
    [.endscope [1], .endscope [3], .call { env := [], scope := [] }] (.ret (v64 7)) []

open Examples in
/-- **The nesting is what orders sibling scopes** (§6.7). With the two
markers swapped, every record is still in location order (`Config.Ordered`)
and every step drops one cell, so per-step order says nothing; yet the run
drops `ℓ1` before `ℓ3`, oldest first, and the first (D-EndScope) pops `ℓ3` off
the record while its marker drops `ℓ1`. `Config.Nested` rejects the
configuration, so `reachable_nested` says no run reaches it. -/
theorem swappedMarkers_rejected :
    swappedMarkers.Ordered ∧ ¬ swappedMarkers.Nested ∧
      ∃ C, Steps demoOps returnPastAffine swappedMarkers C ∧ dropLocs C.trace = [1, 3] := by
  refine ⟨⟨⟨by decide, by decide⟩, fun k hk => ?_⟩, fun h => ?_,
    ⟨stepN demoOps returnPastAffine 3 swappedMarkers, stepN_steps, rfl⟩⟩
  · simp only [List.mem_cons, List.not_mem_nil, or_false] at hk
    rcases hk with rfl | rfl | rfl <;> simp [Kont.Ordered, Rec] <;> decide
  · obtain ⟨⟨sc', hsc, _⟩, _⟩ := h
    have := congrArg List.reverse hsc
    simp at this

/-! ## The corpus, read through the theorems

The order-witnessing corpus cases, each run at the export model: the
identities the destructors ran on, in order (`dtorIds`, the order the printed
destructors print their payloads in, which the bridge already checks against
the compiler); the cells the `drop` markers name, in order (`dropLocs`); and
the owned identities the markers end (`freedIds`), each exactly once. -/

/-- A trace, read through the theorems: destructor identities, drop marker
cells, ended identities (helper). -/
def orderView (D : Decls) (tr : List Event) : List Nat × List Nat × List Nat :=
  (dtorIds tr, dropLocs tr, freedIds D tr)

/-- A corpus program's trace at the export model (helper). -/
def corpusTrace (P : Program) : List Event :=
  (run Corpus.exportOps P Examples.demoFuel).trace

open Examples in
/-- `struct_nested_dtor_drop`: the outer destructor (`#1`) before its field's
(`#0`), `3.9:28`. -/
example : orderView (prog tI64 structNestedDrop).decls (corpusTrace (prog tI64 structNestedDrop))
    = ([1, 0], [2], [1, 0]) := by rfl

open Examples in
/-- `struct_field_drop_order`: fields in declaration order, `3.9:13`. -/
example : orderView (prog tI64 structFieldOrder).decls (corpusTrace (prog tI64 structFieldOrder))
    = ([0, 1], [3], [2, 0, 1]) := by rfl

open Examples in
/-- `array_drop_order`: elements ascending, `3.9:15`. -/
example : orderView (prog tI64 arrayAffineDropOrder).decls
      (corpusTrace (prog tI64 arrayAffineDropOrder)) = ([0, 1, 2], [4], [3, 0, 1, 2]) := by rfl

open Examples in
/-- `array_elem_move_rest_ascending`: the moved element (`#1`) is dropped by
its new binding first, then the array's rest ascending, `3.8:73`. -/
example : orderView (prog tI64 arrayElemMove).decls (corpusTrace (prog tI64 arrayElemMove))
    = ([1, 0, 2], [5, 4], [1, 3, 0, 2]) := by rfl

open Examples in
/-- `enum_match_affine`: the shell (`#1`) consumed, the payload (`#0`)
dropped once by the arm's binding, `6.3:20`. -/
example : orderView (enumProg tI64 enumMatchAffine).decls
      (corpusTrace (enumProg tI64 enumMatchAffine)) = ([0], [3], [1, 0]) := by rfl

open Examples in
/-- `enum_two_payload_bindings`: the arm's two payload cells torn down in one
step, newest first (`ℓ5`, `ℓ4`). -/
example : orderView (enumProg tI64 enumTwoPayloadBindings).decls
      (corpusTrace (enumProg tI64 enumTwoPayloadBindings)) = ([1, 0], [5, 4], [2, 1, 0]) := by rfl

open Examples in
/-- `destructure_residue_order`: the residue in declaration order around the
leaf, both under `ℓ3`'s markers, §6.3. -/
example : orderView (destrProg tI64 destructureResidueOrder).decls
      (corpusTrace (destrProg tI64 destructureResidueOrder)) = ([0, 1], [3, 3], [0, 1, 2]) := by
  rfl

open Examples in
/-- `destructure_nested_residue`: nested residue before a later sibling. -/
example : orderView (destrProg tI64 destructureNestedResidue).decls
      (corpusTrace (destrProg tI64 destructureNestedResidue)) = ([0, 2], [4, 4], [0, 2, 3, 1]) := by
  rfl

open Examples in
/-- `nested_scopes`, the bridge case's own program (helper). -/
def nestedScopes : Program :=
  prog tI64 <| .letIn false (resA (lit 1)) (.letIn false (resA (lit 2)) (lit 0))

/-- It is the corpus case's program. -/
example : (Corpus.cases.find? (·.name == "nested_scopes")).map (·.prog.fns.map (·.body)) =
    some (nestedScopes.fns.map (·.body)) := by rfl

/-- `nested_scopes`: two sibling `let`s exit over two (D-EndScope) steps, the
inner (`ℓ3`) before the outer (`ℓ1`) — the cross-step order `reachable_lifo`
fixes, bridge-checked. -/
example : orderView nestedScopes.decls (corpusTrace nestedScopes) = ([2, 0], [3, 1], [2, 0]) := by
  rfl

open Examples in
/-- `return_past_affine`: the σ-walk newest first, `ℓ3` then `ℓ1`, §6.9. -/
example : orderView returnPastAffine.decls (corpusTrace returnPastAffine) = ([2, 0], [3, 1], [2, 0]) :=
  by rfl

open Examples in
/-- `two_params_dropped_at_pop`: the callee's frame pop tears its by-value
parameters down last-parameter first, `b` (`ℓ4`, the `S5` `#2` and its field
`#1`) before `a` (`ℓ3`, `#0`) — the LIFO half of `drop_order` at a frame,
bridge-checked. -/
example : orderView twoParamsDroppedAtPop.decls (corpusTrace twoParamsDroppedAtPop)
    = ([2, 1, 0], [4, 3], [2, 1, 0]) := by rfl

open Examples in
/-- `three_params_dropped_at_pop`: three parameters, `ℓ5`, `ℓ4`, `ℓ3`. -/
example : orderView threeParamsDroppedAtPop.decls (corpusTrace threeParamsDroppedAtPop)
    = ([2, 1, 0], [5, 4, 3], [2, 1, 0]) := by rfl

open Examples in
/-- `param_moved_other_dropped`: the first parameter, moved into `f2`, is
dropped by `f2`'s pop (`ℓ4`, `#0`); `f1`'s pop then owes only the second
(`ℓ3`, `#1`). -/
example : orderView paramMovedOtherDropped.decls (corpusTrace paramMovedOtherDropped)
    = ([0, 1], [4, 3], [0, 1]) := by rfl

open Examples in
/-- `loop_break_past_local`: the first turn's binding (`ℓ2`) is dropped by its
(D-EndScope) at the turn's end, and the second turn's (`ℓ4`) by the `break`'s
unwind, §6.10. -/
example : orderView (prog tI64 loopBreakPastLocal).decls
      (corpusTrace (prog tI64 loopBreakPastLocal)) = ([1, 3], [2, 4], [1, 3]) := by rfl

end RueCore
