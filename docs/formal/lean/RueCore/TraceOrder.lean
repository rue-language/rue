module

public import RueCore.TraceExact

@[expose] public section

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
is a sequence of blocks, each a `@dbg` line, a consume event, or a drop marker
(`drop ℓ c`, `dropTemp v`) followed by **exactly** §6.11's walk of what the
marker names. The grammar has no other place for a destructor event, so a
trace in it runs every destructor inside the walk of the marker before it,
in §6.11's order, and no destructor anywhere else.

`run_blocks` proves every terminating run's trace is in the grammar. It reads no
typing derivation, only `DtorNotCopy` (a destructor-bearing struct is not
`Copy`, `3.9:31`), which a declared-linear destructure's `Copy` residue
subtree needs: that subtree is dropped with no marker, as `@drop` of a `Copy`
place is, and its walk is empty only because nothing under a `Copy` node owns
anything (Copy containment, which the machine maintains) and no `Copy` struct
declares a destructor.

## Across cells: last-in first-out, over §6's relation (§6.7, §6.9, §6.10)

The order *between* cells is not a property of the trace alone: every
`drop ℓ c` block is a valid block by itself, so "the trace can be cut into
newest-first groups" says nothing. What gives it content is the machine's
drop scopes, so this half is stated over `Step`, from every configuration
reachable from `Config.init`:

* `reachable_ordered`: every drop scope in a reachable configuration — the
  current activation record's, every suspended caller's and loop boundary's, and every
  pending `endscope` marker's — lists its cells in strictly increasing
  location order, below the store's length. Records are only ever extended
  with freshly allocated cells, so **registration order is location order**.
  It is a corollary of the nesting below: every drop scope is a part of the
  registration stack (`Config.Nested.ordered`).
* `step_drop_order`: every step's `drop` markers either all name one cell
  (an overwrite, `@drop`, or a destructure's residue, several sub-positions
  of one binding) or name distinct cells in **strictly decreasing** location
  order — newest registered first.
* `reachable_nested`: the scopes **nest**. An activation record's pending `endscope`
  markers are exactly the tail of its record, innermost last, and the whole
  registration stack — every suspended caller's record, then the current
  activation record's — is in location order. So (D-EndScope)'s pop by count removes the
  marker's own cells (`Activation.unwindScope_tail`).
* `reachable_stackDiscipline`: every step is **last-in first-out** on that stack
  (`StackDiscipline`). It keeps the stack as a prefix of the new one, or cuts it back
  and drops only cells of the suffix it cut, newest first; each such cell is
  newer than every cell still registered. That orders drops *across* steps:
  `{ let a; let b; }` exits over two (D-EndScope) steps, and `b` drops
  first. `swappedMarkers_rejected` is a configuration with the two markers
  swapped: every record is in order and every step drops one cell, yet it
  drops oldest first — and it is not `Nested`, so no run reaches it.

`drop_order` states both halves over `Step`: the within-value half reaches
`Step`'s terminating runs through `eval_small_to_big` (`step_blocks`).

## What the grammar does not constrain

`Blocks` ties each marker to its walk, not to the cell: fidelity to what the
cell held is `dropCell` reading `H(ℓ)` and the exactly-once ledger
(`TraceExact.lean`). A consume event carries no walk, so on a program the
checker rejects a destructor-bearing node can be consumed without its
destructor running (`destructure_under_dtor`, which `3.9:34` makes E0456);
the grammar accepts that trace. And "an enum's active payload only" is how
`Contents.enum` stores a value — it holds the active variant's payload and
no other — rather than a clause of the grammar.

The definitions the statements here are written in (`Blocks`,
`Config.Ordered`, `Config.Nested`, `StrictStackOrder`, `StackDiscipline`) are in
`Trace/Defs.lean`, the definitions layer; the witnesses on example and corpus
programs are in `Witnesses.lean` (README, "Layers").
-/

namespace RueCore

/-! ## Within a value: the block grammar -/

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
  | .movedOut, _, h | .int _ _ _, _, h | .float _ _, _, h | .bool _, _, h | .unit, _, h => by
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
  | .movedOut, _ | .int _ _ _, _ | .float _ _, _ | .bool _, _ | .unit, _ => rfl
  | .struct s i cs, hac => by
      simp only [Contents.allCopy, Bool.and_eq_true, decide_eq_true_eq] at hac
      simp only [dropEvents, dropEventsList_allCopy hdt hac.2, List.append_nil]
      split
      · rename_i sd hd
        have : sd.dtor = false := Bool.eq_false_iff.mpr fun hsd => absurd hac.1 (hdt s sd hd hsd)
        simp [this]
      · rfl
  | .enum _ _ _ cs, hac | .array _ _ cs, hac => by
      simp only [Contents.allCopy, Bool.and_eq_true] at hac
      exact dropEventsList_allCopy hdt hac.2

/-- The same over a list (helper). -/
theorem dropEventsList_allCopy {D : Decls} (hdt : DtorNotCopy D) :
    ∀ {cs : List Contents}, Contents.allCopyList D cs = true → dropEventsList D cs = []
  | [], _ => rfl
  | c :: cs, hac => by
      simp only [Contents.allCopyList, Bool.and_eq_true] at hac
      simp [dropEventsList, dropEvents_allCopy hdt hac.1, dropEventsList_allCopy hdt hac.2]
end

/-! ## Within a value: the machine's walk is §6.11's rules

`DropGlue` (`Trace/Defs.lean`) states §6.11's drop rule by rule, over the
declarations, without the machine's walk (RUE-2487). The walk `dropContents`
meets it whenever it succeeds (`dropContents_glue`), and the rules determine
the events, which are then `dropEvents`'s (`DropGlue.eq_dropEvents`). So the
block grammar the proofs below build, `DropGlueBlocks`, is the one stated in
§6.11's own terms, and `Blocks` follows from it (`DropGlueBlocks.toBlocks`). -/

mutual
/-- **The machine's walk follows §6.11's rules**: whenever `dropContents`
succeeds, its events are the ones `DropGlue` gives the contents — the
destructor first, then the fields in declaration order, the elements
ascending, an enum's stored payload, every `⊘` skipped. -/
theorem dropContents_glue {D : Decls} : ∀ {c : Contents} {evs : List Event},
    dropContents D c = .ok evs → DropGlue D c evs
  | .movedOut, _, h => by simp [dropContents] at h; subst h; exact .movedOut
  | .int _ _ _, _, h => by simp [dropContents] at h; subst h; exact .int
  | .float _ _, _, h => by simp [dropContents] at h; subst h; exact .float
  | .bool _, _, h => by simp [dropContents] at h; subst h; exact .bool
  | .unit, _, h => by simp [dropContents] at h; subst h; exact .unit
  | .struct s i cs, evs, h => by
      simp only [dropContents] at h
      split at h
      · cases h
      · rename_i sd hd
        split at h
        · cases h
        · rename_i evs' hl
          cases h
          cases hdt : sd.dtor
          · simpa [hdt] using DropGlue.struct hd hdt (dropContentsList_glue hl)
          · simpa [hdt] using DropGlue.structDtor hd hdt (dropContentsList_glue hl)
  | .enum _ _ _ cs, _, h => by
      simp only [dropContents] at h; exact .enum (dropContentsList_glue h)
  | .array _ _ cs, _, h => by
      simp only [dropContents] at h; exact .array (dropContentsList_glue h)

/-- The same over a list (helper). -/
theorem dropContentsList_glue {D : Decls} : ∀ {cs : List Contents} {evs : List Event},
    dropContentsList D cs = .ok evs → DropGlueSeq D cs evs
  | [], _, h => by simp [dropContentsList] at h; subst h; exact .nil
  | c :: cs, _, h => by
      simp only [dropContentsList] at h
      split at h
      · cases h
      · rename_i e₁ h₁
        split at h
        · cases h
        · rename_i e₂ h₂
          cases h
          exact .cons (dropContents_glue h₁) (dropContentsList_glue h₂)
end

mutual
/-- §6.11's rules determine the events, and they are `dropEvents`'s (helper). -/
theorem DropGlue.eq_dropEvents {D : Decls} : ∀ {c : Contents} {evs : List Event},
    DropGlue D c evs → evs = dropEvents D c
  | _, _, .movedOut | _, _, .int | _, _, .float | _, _, .bool | _, _, .unit => rfl
  | _, _, .struct hd hdt hl => by simp [dropEvents, hd, hdt, DropGlueSeq.eq_dropEventsList hl]
  | _, _, .structDtor hd hdt hl => by simp [dropEvents, hd, hdt, DropGlueSeq.eq_dropEventsList hl]
  | _, _, .array hl => by simp only [dropEvents]; exact DropGlueSeq.eq_dropEventsList hl
  | _, _, .enum hl => by simp only [dropEvents]; exact DropGlueSeq.eq_dropEventsList hl

/-- The same over a list (helper). -/
theorem DropGlueSeq.eq_dropEventsList {D : Decls} : ∀ {cs : List Contents} {evs : List Event},
    DropGlueSeq D cs evs → evs = dropEventsList D cs
  | _, _, .nil => rfl
  | _, _, .cons h₁ h₂ => by
      simp [dropEventsList, DropGlue.eq_dropEvents h₁, DropGlueSeq.eq_dropEventsList h₂]
end

/-- A trace in §6.11's own grammar is in `Blocks` (helper). -/
theorem DropGlueBlocks.toBlocks {D : Decls} {t : List Event} (h : DropGlueBlocks D t) : Blocks D t := by
  induction h with
  | nil => exact .nil
  | dbg _ ih => exact .dbg ih
  | consume _ ih => exact .consume ih
  | drop hg _ ih => rw [DropGlue.eq_dropEvents hg]; exact .drop ih
  | dropTemp hg _ ih => rw [DropGlue.eq_dropEvents hg]; exact .dropTemp ih

/-- Two block sequences of §6.11's grammar, one after the other, are one
(helper). -/
theorem DropGlueBlocks.append {D : Decls} {t u : List Event} (h₁ : DropGlueBlocks D t)
    (h₂ : DropGlueBlocks D u) : DropGlueBlocks D (t ++ u) := by
  induction h₁ with
  | nil => exact h₂
  | dbg _ ih => exact .dbg ih
  | consume _ ih => exact .consume ih
  | drop hg _ ih => simpa only [List.cons_append, List.append_assoc] using DropGlueBlocks.drop hg ih
  | dropTemp hg _ ih =>
      simpa only [List.cons_append, List.append_assoc] using DropGlueBlocks.dropTemp hg ih

/-- A binding's drop (`dropCell`) is one block, or nothing for `Copy`
contents (helper). -/
theorem dropCell_blocks {D : Decls} {ℓ : Nat} {c : Contents} {evs : List Event}
    (h : dropCell D ℓ c = .ok evs) : DropGlueBlocks D evs := by
  unfold dropCell at h
  split at h
  · cases h; exact .nil
  · split at h
    · cases h
    · rename_i evs' hw
      cases h
      simpa using DropGlueBlocks.drop (ℓ := ℓ) (dropContents_glue hw) (DropGlueBlocks.nil (D := D))

/-- `drop-retire` (§6.1) is one block or nothing (helper). -/
theorem dropRetire_blocks {D : Decls} {H H' : Store} {ℓ : Nat} {evs : List Event}
    (h : dropRetire D H ℓ = .ok (H', evs)) : DropGlueBlocks D evs := by
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
      unwindLocs D H ls = .ok (H', evs) → DropGlueBlocks D evs
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
    ∀ {rs : List Contents} {evs : List Event}, Contents.copyContainedList D rs = true →
      dropResidue D ℓ rs = .ok evs → DropGlueBlocks D evs
  | [], _, _, h => by simp [dropResidue] at h; subst h; exact .nil
  | r :: rs, evs, hcc, h => by
      simp only [Contents.copyContainedList, Bool.and_eq_true] at hcc
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
            unfold residueDropEvent
            split
            · rename_i hm
              rw [dropContents_eq h₁, dropEvents_allCopy hdt (Contents.copyContained_allCopy hcc.1 hm)]
              simpa using ih
            · simpa using DropGlueBlocks.drop (ℓ := ℓ) (dropContents_glue h₁) ih

/-- §6.3's destructure is a sequence of blocks: the residue's, then the
consumed dead remainder (helper). -/
theorem destructure_blocks {D : Decls} (hdt : DtorNotCopy D) {ℓ : Nat} {cd leaf : Contents}
    {πs : List Nat} {evs : List Event} (hcc : cd.copyContained D = true)
    (h : cd.destructure D ℓ πs = .ok (leaf, evs)) : DropGlueBlocks D evs := by
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

/-- (D-Match)'s consume event is a block or nothing (helper). -/
theorem matchConsume_blocks {D : Decls} {e k i : Nat} {vs : List Val} :
    DropGlueBlocks D (matchConsume D e k i vs) := by
  unfold matchConsume
  split
  · exact .nil
  · exact .consume .nil

/-! ## Within a value: every evaluation's trace is blocks -/

/-- A prefix of blocks before a result's blocks (helper). -/
theorem DropGlueBlocks.withTrace {D : Decls} {tr : List Event} {r : EvalRes} (h₁ : DropGlueBlocks D tr)
    (h₂ : DropGlueBlocks D r.trace) : DropGlueBlocks D (r.withTrace tr).trace := by
  cases r <;> simp only [EvalRes.withTrace, EvalRes.trace] at h₂ ⊢ <;>
    first | exact h₁.append h₂ | exact .nil

/-- §6.2's search keeps the grammar (helper). -/
theorem DropGlueBlocks.bind {D : Decls} {r : EvalRes} {k : Store → Val → EvalRes}
    (hr : DropGlueBlocks D r.trace) (hk : ∀ H₁ v tr, r = .ok H₁ v tr → DropGlueBlocks D (k H₁ v).trace) :
    DropGlueBlocks D (r.bind k).trace := by
  cases r with
  | ok H₁ v tr => exact DropGlueBlocks.withTrace hr (hk H₁ v tr rfl)
  | _ => exact hr

/-- §6.9's call boundary keeps the grammar (helper). -/
theorem DropGlueBlocks.absorb {D : Decls} {r : EvalRes} {k : Store → Val → EvalRes}
    (hr : DropGlueBlocks D r.trace) (hk : ∀ H₁ v tr, r = .ok H₁ v tr → DropGlueBlocks D (k H₁ v).trace) :
    DropGlueBlocks D (r.bindCall k).trace := by
  cases r with
  | ok H₁ v tr => exact DropGlueBlocks.withTrace hr (hk H₁ v tr rfl)
  | broke => exact .nil
  | _ => exact hr

/-- An operator's outcome emits nothing (helper). -/
theorem DropGlueBlocks.opRes {D : Decls} {H : Store} {o : OpRes} : DropGlueBlocks D (o.toRes H).trace := by
  cases o <;> exact .nil

/-- Aggregate introduction emits nothing (helper). -/
theorem DropGlueBlocks.intro {D D' : Decls} {H : Store} {mk : Nat → Val} :
    DropGlueBlocks D (introVal D' H mk).trace := by
  unfold introVal; split <;> exact .nil

/-- A copy-contained store is one step further along an evaluation that reached
a value (helper). -/
theorem eval_ok_cc (M : FloatSig) {P : Program} {n : Nat} {H H₁ : Store} {φ : Activation} {e : Expr}
    {v : Val} {tr : List Event} (hcc : StoreCC P.decls H)
    (hr : eval M n P H φ e = .ok H₁ v tr) :
    StoreCC P.decls H₁ ∧ (Contents.ofVal v).copyContained P.decls = true := by
  have h := eval_conserves M (freed_measure P.decls) n H φ e hcc
  rw [hr] at h
  exact ⟨h.2.1, h.2.2.1⟩

/-- The grammar's promise about an argument list (helper). -/
def ArgsDropGlueBlocks (D : Decls) : ArgsRes → Prop
  | .ok _ _ tr => DropGlueBlocks D tr
  | .abort r => DropGlueBlocks D r.trace

/-- An argument list keeps the grammar (helper). -/
theorem evalArgs_blocks {D : Decls} {ev : Store → Expr → EvalRes}
    (hev : ∀ H e, StoreCC D H → DropGlueBlocks D (ev H e).trace)
    (hcc : ∀ H e H₁ v tr, StoreCC D H → ev H e = .ok H₁ v tr → StoreCC D H₁) :
    ∀ (H : Store) (es : List Expr), StoreCC D H → ArgsDropGlueBlocks D (evalArgs ev H es)
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
          | ok H₂ vs tr₂ => rw [hra] at h₂; exact DropGlueBlocks.append h₁ h₂
          | abort r => rw [hra] at h₂; exact DropGlueBlocks.withTrace h₁ h₂
      | _ => rw [hr] at h₁; exact h₁

/-- **Every evaluation's trace is in §6.11's block grammar** (§3.9, §6.11):
every evaluation, of every expression, from every copy-contained store, at every
fuel. By fuel induction over `eval`; no typing derivation, only
`DtorNotCopy`, which a destructure's `Copy` residue needs (module
docstring). -/
theorem eval_glue_blocks (M : FloatSig) {P : Program} (hdt : DtorNotCopy P.decls) :
    ∀ (fuel : Nat) (H : Store) (φ : Activation) (e : Expr), StoreCC P.decls H →
      DropGlueBlocks P.decls (eval M fuel P H φ e).trace := by
  intro fuel
  induction fuel with
  | zero => intro H φ e _; exact .nil
  | succ n ih =>
    intro H φ e hcc
    have hok := fun {H' : Store} {φ' : Activation} {e' : Expr} {H₁ : Store} {v : Val}
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
                    (Contents.getAt_copyContained _ (hcc _ c hc) hr) hd
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
                    (Contents.getAt_copyContained _ (hcc _ c hc) hr) hd
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
        refine DropGlueBlocks.bind (ih H φ e₁ hcc) (fun H₁ _ _ hr => ?_)
        exact DropGlueBlocks.bind (ih H₁ φ e₂ (hok hcc hr).1) (fun _ _ _ _ => DropGlueBlocks.opRes)
    | unop _ e₁ | intCast _ _ e₁ | fintrin _ e₁ =>
        simp only [eval]
        exact DropGlueBlocks.bind (ih H φ e₁ hcc) (fun _ _ _ _ => DropGlueBlocks.opRes)
    | dbg e₁ =>
        simp only [eval]
        refine DropGlueBlocks.bind (ih H φ e₁ hcc) (fun _ _ _ _ => ?_)
        split
        · exact .dbg .nil
        · exact .nil
    | repeatArray T e₁ m =>
        simp only [eval]
        refine DropGlueBlocks.bind (ih H φ e₁ hcc) (fun _ _ _ _ => ?_)
        split
        · exact DropGlueBlocks.intro
        · exact .nil
    | mkStruct _ args | mkEnum _ _ args | mkArray _ args =>
        simp only [eval]
        have ka := hargs H args hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          refine DropGlueBlocks.withTrace ka ?_
          (repeat' split) <;> first | exact .nil | exact DropGlueBlocks.intro
    | indexRead p idx πs =>
        simp only [eval]
        have ka := hargs H idx hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          refine DropGlueBlocks.withTrace ka ?_
          (repeat' split) <;> exact .nil
    | indexDrop p idx πs =>
        simp only [eval]
        exact DropGlueBlocks.bind (ih H φ _ hcc) (fun _ _ _ _ => .nil)
    | indexWrite p idx πs e₁ =>
        simp only [eval]
        refine DropGlueBlocks.bind (ih H φ e₁ hcc) (fun H₁ _ _ hr => ?_)
        have ka := hargs H₁ idx (hok hcc hr).1
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₂ vs tr hra
          rw [hra] at ka
          refine DropGlueBlocks.withTrace ka ?_
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
        refine DropGlueBlocks.bind (ih H φ scrut hcc) (fun H₀ v _ hr => ?_)
        obtain ⟨hc₀, hv⟩ := hok hcc hr
        cases v with
        | enum e k i vs =>
          dsimp only
          split
          · exact .nil
          · rename_i body _
            refine DropGlueBlocks.withTrace matchConsume_blocks ?_
            refine DropGlueBlocks.bind (ih _ _ body (hc₀.freshParams (Contents.enum_payload hv 0).2))
              (fun _ _ _ _ => ?_)
            split
            · exact .nil
            · rename_i H₃ evs hu
              exact unwindLocs_blocks hu
        | _ => exact .nil
    | letIn m e₁ e₂ =>
        simp only [eval]
        refine DropGlueBlocks.bind (ih H φ e₁ hcc) (fun H₁ v₁ _ hr => ?_)
        obtain ⟨hc₁, hv₁⟩ := hok hcc hr
        refine DropGlueBlocks.bind (ih _ _ e₂ (hc₁.append (StoreCC.single hv₁))) (fun _ _ _ _ => ?_)
        split
        · exact .nil
        · rename_i H₃ evs hdr
          exact dropRetire_blocks hdr
    | assign p e₁ =>
        simp only [eval]
        refine DropGlueBlocks.bind (ih H φ e₁ hcc) (fun _ _ _ _ => ?_)
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
        refine DropGlueBlocks.bind (ih H φ e₁ hcc) (fun H₁ v₁ _ hr => ?_)
        have hc₁ := (hok hcc hr).1
        split
        · exact .nil
        · split
          · exact .nil
          · rename_i evs hd
            refine DropGlueBlocks.withTrace ?_ (ih H₁ φ e₂ hc₁)
            simpa using DropGlueBlocks.dropTemp (v := v₁) (dropContents_glue hd) (DropGlueBlocks.nil (D := P.decls))
        · exact ih H₁ φ e₂ hc₁
    | ite c e₁ e₂ =>
        simp only [eval]
        refine DropGlueBlocks.bind (ih H φ c hcc) (fun H₀ _ _ hr => ?_)
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
          refine DropGlueBlocks.withTrace ka ?_
          split
          · exact .nil
          · rename_i fd _
            split
            · refine DropGlueBlocks.absorb (ih _ _ fd.body (c₁.freshParams cv₁)) (fun _ _ _ _ => ?_)
              split
              · exact .nil
              · rename_i H₄ evs hu
                exact unwindLocs_blocks hu
            · exact .nil
    | ret e₁ =>
        simp only [eval]
        refine DropGlueBlocks.bind (ih H φ e₁ hcc) (fun _ _ _ _ => ?_)
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
          exact DropGlueBlocks.withTrace hb (ih H₁ φ (.loop e₁) (hok hcc hr).1)
        · exact .nil
        · rename_i H₁ sc tr hr
          rw [hr] at hb
          split
          · exact .nil
          · rename_i H₂ evs hu
            exact DropGlueBlocks.append hb (unwindLocs_blocks hu)
        · exact hb

/-- **Every terminating run's trace is in §6.11's block grammar** (§3.9, §6.11):
every destructor event of every run sits inside §6.11's walk of the drop
marker before it — the value's own destructor first (`3.9:28`), then its
fields in declaration order (`3.9:13`), an array's elements ascending
(`3.9:15`), an enum's active payload only (`6.3:20`), every `⊘` skipped —
and nowhere else. It needs only `DtorNotCopy`, which `WfDecls` gives. -/
theorem run_glue_blocks (M : FloatSig) {P : Program} (hdt : DtorNotCopy P.decls) (fuel : Nat) :
    DropGlueBlocks P.decls (run M P fuel).trace :=
  eval_glue_blocks M hdt fuel [] _ _ (fun ℓ c hc => by simp at hc)

/-- **Every terminating run's trace is in the block grammar `Blocks`** (§3.9,
§6.11), each drop marker followed by exactly `dropEvents` of what it names:
`run_glue_blocks` read through `DropGlueBlocks.toBlocks`. -/
theorem run_blocks (M : FloatSig) {P : Program} (hdt : DtorNotCopy P.decls) (fuel : Nat) :
    Blocks P.decls (run M P fuel).trace :=
  (run_glue_blocks M hdt fuel).toBlocks

/-! ## Across cells: records in location order -/

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

/-- `freshParams`' store and cells, as the invariant reads them (helper). -/
theorem freshParams_eq {H H' : Store} {vs : List Val} {ls : List Nat}
    (h : freshParams H vs = (H', ls)) :
    H'.length = H.length + vs.length ∧ ls = List.range' H.length vs.length := by
  have h₁ := freshParams_length H vs
  have h₂ := freshParams_locs H vs
  rw [h] at h₁ h₂
  exact ⟨h₁, h₂⟩

/-! ## Across cells: every step's drops are newest-first -/

/-- `dropLocs` distributes over concatenation (helper). -/
theorem dropLocs_append (l₁ l₂ : List Event) : dropLocs (l₁ ++ l₂) = dropLocs l₁ ++ dropLocs l₂ :=
  List.filterMap_append

mutual
/-- §6.11's walk names no cell: it emits destructor events only (helper). -/
theorem dropLocs_dropEvents (D : Decls) : ∀ c : Contents, dropLocs (dropEvents D c) = []
  | .movedOut | .int _ _ _ | .float _ _ | .bool _ | .unit => rfl
  | .struct s i cs => by
      simp only [dropEvents, dropLocs_append, dropLocs_dropEventsList D cs, List.append_nil]
      split
      · split <;> rfl
      · rfl
  | .enum _ _ _ cs | .array _ _ cs => dropLocs_dropEventsList D cs

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
          · unfold residueDropEvent at hx
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

/-- A teardown of an ordered record drops newest-first (helper). -/
theorem StrictStackOrder.teardown {n : Nat} {ls ls' : List Nat} (h : Rec n ls)
    (hs : ls'.Sublist ls.reverse) : StrictStackOrder ls' :=
  .inr ((List.pairwise_reverse.mpr h.1).sublist hs)

/-- **Every step of §6's relation from an ordered configuration drops
newest-first** (§6.7, §6.9, §6.10, §6.11): it appends to the trace, and the
`drop` markers it appends name one cell or name distinct cells in strictly
decreasing location order. A teardown — (D-EndScope), (D-Return-Value)'s
activation record pop, (D-Return)'s σ-walk, (D-Loop-Iter)'s end of a turn and
(D-Break)'s unwind — walks an ordered record backwards
(`StrictStackOrder.teardown`). -/
theorem step_drop_order {M : FloatSig} {P : Program} {C C' : Config} (h : Step M P C C')
    (hC : C.Ordered) : ∃ evs, C'.trace = C.trace ++ evs ∧ StrictStackOrder (dropLocs evs) := by
  have one : ∀ {evs : List Event} (ℓ : Nat), (∀ x ∈ dropLocs evs, x = ℓ) →
      StrictStackOrder (dropLocs evs) := fun ℓ h => .inl ⟨ℓ, h⟩
  have none : ∀ {evs : List Event}, dropLocs evs = [] → StrictStackOrder (dropLocs evs) :=
    fun h => .inl ⟨0, by simp [h]⟩
  cases h
  case useDeclared H φ K tr p ℓ c πd πs cd leaf evs v c' _ _ _ hd _ _ =>
    exact ⟨_, rfl, one ℓ (plainDestructure_locs hd)⟩
  case dbg => exact ⟨_, rfl, none rfl⟩
  case «match» =>
    refine ⟨_, rfl, none ?_⟩
    unfold matchConsume; split <;> rfl
  case endScope H φ K tr ℓs v H' evs hu =>
    exact ⟨_, rfl, StrictStackOrder.teardown (hC.2 _ List.mem_cons_self) (plainUnwind_locs hu)⟩
  case seqDrop hd =>
    refine ⟨_, rfl, none ?_⟩
    rw [show ∀ v evs, (Event.dropTemp v :: evs) = [.dropTemp v] ++ evs from fun _ _ => rfl,
      dropLocs_append, dropContents_eq hd, dropLocs_dropEvents]; rfl
  case assign H φ K tr p v ℓ c old evs c' _ _ hd _ => exact ⟨_, rfl, one ℓ (dropCell_locs hd)⟩
  case indexWrite H φ K tr p πs v vs ℓ c sub ρ old evs sub' c' _ _ hd _ _ => exact ⟨_, rfl, one ℓ (dropCell_locs hd)⟩
  case dropDeclared H φ K tr p ℓ c πd πs cd leaf evs levs c' _ _ _ hd hl _ =>
    refine ⟨_, rfl, one ℓ (fun x hx => ?_)⟩
    rw [dropLocs_append, List.mem_append] at hx
    exact hx.elim (plainDestructure_locs hd x) (dropCell_locs hl x)
  case dropMove H φ K tr p ℓ c sub evs c' _ _ _ _ hd _ => exact ⟨_, rfl, one ℓ (dropCell_locs hd)⟩
  case callReturn hu | ret hu => exact ⟨_, rfl, StrictStackOrder.teardown hC.1 (plainUnwind_locs hu)⟩
  case loopIter hu | brk hu =>
    exact ⟨_, rfl, StrictStackOrder.teardown (hC.1.sublist (List.drop_sublist _ _))
      (plainUnwind_locs hu)⟩
  all_goals exact ⟨[], by simp [Config.trace], none rfl⟩

/-! ## Across steps: scopes nest, and teardown is last-in first-out

`step_drop_order` orders the drops of one step. A source block
`{ let a; let b; }` exits over **two** (D-EndScope) steps, so ordering them
needs more: that the pending `endscope` markers are exactly the tail of the
drop scope, innermost last (`Config.Nested`). Then every teardown removes a
suffix of the machine's whole registration stack — the suspended callers'
records, then the current activation record's — and drops cells only from that suffix,
newest first (`StackDiscipline`). Since the stack is in location order, a cell a
teardown deregisters is newer than every cell still registered
(`StackDiscipline.newer`): across steps, across scopes and across activation records, the machine
drops last-in first-out. Every drop scope is a part of that stack, so the
nesting also gives the per-record order `step_drop_order` reads
(`reachable_ordered`). -/

/-- (D-EndScope)'s pop by count removes exactly the marker's cells when they
are the record's tail (helper). -/
theorem Activation.unwindScope_tail (φ : Activation) {sc ls : List Nat} (h : φ.scope = sc ++ ls) :
    (φ.unwindScope ls.length).scope = sc := by
  simp [Activation.unwindScope, h]

/-- (D-Return)'s search, read by the nesting: the caller's record is the
next one, and the stack below it is the rest (helper). -/
theorem Nest.toCall {K K' : List Kont} {φ : Activation} :
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
prefix of the activation record's, the rest being the cells the body registered, and no
caller's record is crossed (helper). -/
theorem Nest.toLoop {K K' : List Kont} {φ : Activation} :
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

/-- **What a teardown deregisters is newer than everything still
registered**: on a stack in location order, a step that cuts the stack back
drops only cells it deregistered, each newer than every cell still
registered (helper). -/
theorem StackDiscipline.newer {S S' ls : List Nat} (hS : S.Pairwise (· < ·)) (h : StackDiscipline S S' ls)
    (hcut : ¬ S <+: S') : ∀ ℓ ∈ ls, ℓ ∉ S' ∧ ∀ ℓ' ∈ S', ℓ' < ℓ := by
  rcases h with h | ⟨⟨rest, rfl⟩, hs⟩
  · exact absurd h hcut
  · intro ℓ hℓ
    have hr : ℓ ∈ rest := by simpa using hs.subset hℓ
    have hlt : ∀ ℓ' ∈ S', ℓ' < ℓ := fun ℓ' hℓ' =>
      (List.pairwise_append.mp hS).2.2 ℓ' hℓ' ℓ hr
    exact ⟨fun hm => Nat.lt_irrefl ℓ (hlt ℓ hm), hlt⟩

/-- A step that keeps the activation record and the callers keeps the stack (helper). -/
theorem StackDiscipline.same {S ls : List Nat} : StackDiscipline S S ls := .inl (List.prefix_refl S)

/-- A teardown of the current activation record's tail (helper). -/
theorem StackDiscipline.cut {A m ls : List Nat} (h : ls.Sublist m.reverse) : StackDiscipline (A ++ m) A ls :=
  .inr ⟨List.prefix_append A m, by simpa using h⟩

/-- **Every step keeps the scopes nested** (§6.7, §6.9, §6.10): (D-Let) and
(D-Match) push a marker equal to the cells they append to the record,
(D-EndScope) pops both together, a call starts an activation record of its own, and a
return, a loop turn's end and a `break` restore a record the stack held. -/
theorem step_nested {M : FloatSig} {P : Program} {C C' : Config} (h : Step M P C C')
    (hC : C.Nested) : C'.Nested := by
  cases h
  case «match» H φ K tr arms e k i vs body H' ls _ hm =>
    obtain ⟨hl, rfl⟩ := freshParams_eq hm
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
    have hp := Activation.unwindScope_tail φ hsc
    have h2 : Rec H.length (Stk K ++ φ.scope) := hC.2
    refine ⟨by rw [hp]; exact hn, ?_⟩
    show Rec H'.length (Stk K ++ (φ.unwindScope ℓs.length).scope)
    rw [hp]
    refine (h2.sublist ?_).mono (by omega)
    rw [hsc, ← List.append_assoc]; exact List.sublist_append_left _ _
  case call H φ K tr f vs fd H' ls _ _ hm =>
    obtain ⟨hl, rfl⟩ := freshParams_eq hm
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
(`Activation.unwindScope_tail`). No typing hypothesis. -/
theorem reachable_nested {M : FloatSig} {P : Program} {C : Config}
    (h : Steps M P Config.init C) : C.Nested :=
  Steps.invariant step_nested h ⟨trivial, ⟨.nil, by simp [Stk]⟩⟩

/-- The nesting orders every frame's record: a pending marker's cells and a
loop boundary's record are parts of the activation record they sit on, and a
suspended caller's record is part of the stack below it, so each is a part of
the registration stack in its order (helper). -/
theorem Nest.ordered {n : Nat} {K : List Kont} :
    ∀ {sc : List Nat}, Nest sc K → Rec n (Stk K ++ sc) → ∀ k ∈ K, k.Ordered n := by
  induction K with
  | nil => intro _ _ _ _ hk; cases hk
  | cons k K ih =>
      intro sc hn hr k' hk
      cases k
      case endscope ls =>
        obtain ⟨sc', rfl, hn'⟩ := hn
        have hr' : Rec n (Stk K ++ sc' ++ ls) := by rw [List.append_assoc]; exact hr
        rcases List.mem_cons.mp hk with rfl | hk
        · exact hr'.sublist (List.sublist_append_right _ _)
        · exact ih hn' (hr'.sublist (List.sublist_append_left _ _)) k' hk
      case loop e φs =>
        obtain ⟨rfl, hn'⟩ := hn
        rcases List.mem_cons.mp hk with rfl | hk
        · exact hr.sublist (List.sublist_append_right _ _)
        · exact ih hn' hr k' hk
      case call φs =>
        have hr' : Rec n (Stk K ++ φs.scope ++ sc) := hr
        rcases List.mem_cons.mp hk with rfl | hk
        · exact hr'.sublist ((List.sublist_append_right _ _).trans (List.sublist_append_left _ _))
        · exact ih hn (hr'.sublist (List.sublist_append_left _ _)) k' hk
      all_goals
        rcases List.mem_cons.mp hk with rfl | hk
        · trivial
        · exact ih hn hr k' hk

/-- **Nested scopes are ordered ones**: every drop scope of a nested
configuration is a part of its registration stack, in the stack's order
(helper). -/
theorem Config.Nested.ordered {C : Config} (h : C.Nested) : C.Ordered := by
  cases C with
  | run H φ K f tr => exact ⟨h.2.sublist (List.sublist_append_right _ _), Nest.ordered h.1 h.2⟩
  | panic => trivial

/-- **Registration order is location order, everywhere the machine goes**
(§6.1, §6.7, §6.9, §6.10): in every configuration reachable from §6.12's
initial one, every drop scope — the current activation record's, every suspended
caller's and loop boundary's, and every pending `endscope` marker's — lists
its cells in strictly increasing location order. No typing hypothesis. -/
theorem reachable_ordered {M : FloatSig} {P : Program} {C : Config}
    (h : Steps M P Config.init C) : C.Ordered :=
  (reachable_nested h).ordered

/-- **Newest-first teardown, on every reachable step** (§6.7, §6.9, §6.10):
from every configuration reachable from §6.12's initial one, every step's
`drop` markers name one cell or distinct cells newest-first. No typing
hypothesis. -/
theorem reachable_drop_order {M : FloatSig} {P : Program} {C C' : Config}
    (hr : Steps M P Config.init C) (h : Step M P C C') :
    ∃ evs, C'.trace = C.trace ++ evs ∧ StrictStackOrder (dropLocs evs) :=
  step_drop_order h (reachable_ordered hr)

/-- **Every step is last-in first-out** (§6.7, §6.9, §6.10): from a nested
configuration, a step appends `evs` to the trace and either keeps the
registration stack as a prefix of the new one, or cuts it back and drops
only cells of the cut suffix, newest first (`StackDiscipline`). -/
theorem step_stackDiscipline {M : FloatSig} {P : Program} {C C' : Config} (h : Step M P C C')
    (hC : C.Nested) :
    ∃ evs, C'.trace = C.trace ++ evs ∧ StackDiscipline C.stack C'.stack (dropLocs evs) := by
  have keep : ∀ {evs : List Event} {S S' : List Nat}, S <+: S' → StackDiscipline S S' (dropLocs evs) :=
    fun h => .inl h
  have pre : ∀ A B D : List Nat, A ++ B <+: A ++ (B ++ D) := fun A B D => by
    rw [← List.append_assoc]; exact List.prefix_append _ _
  cases h
  case «match» => exact ⟨_, rfl, keep (pre _ _ _)⟩
  case letBind => exact ⟨[], by simp [Config.trace], keep (pre _ _ _)⟩
  case endScope H φ K tr ℓs v H' evs hu =>
    obtain ⟨sc', hsc, _⟩ := hC.1
    refine ⟨_, rfl, ?_⟩
    simp only [Config.stack, Stk, Activation.unwindScope_tail φ hsc, hsc, ← List.append_assoc]
    exact StackDiscipline.cut (plainUnwind_locs hu)
  case call => exact ⟨[], by simp [Config.trace], keep (List.prefix_append _ _)⟩
  case callReturn H φ K tr φs v H' evs hu =>
    refine ⟨_, rfl, ?_⟩
    simp only [Config.stack, Stk]
    exact StackDiscipline.cut (plainUnwind_locs hu)
  case ret H φ K tr v φs K' H' evs hk hu =>
    obtain ⟨_, hs⟩ := Nest.toCall hC.1 hk
    refine ⟨_, rfl, ?_⟩
    simp only [Config.stack, hs]
    exact StackDiscipline.cut (plainUnwind_locs hu)
  case loopIter H φ K tr e φs H' evs hu =>
    obtain ⟨heq, _⟩ := hC.1
    refine ⟨_, rfl, ?_⟩
    simp only [Config.stack, Stk, heq]
    exact StackDiscipline.same
  case brk H φ K tr φs K' H' evs hk hu =>
    obtain ⟨⟨m, hm⟩, _, hs⟩ := Nest.toLoop hC.1 hk
    refine ⟨_, rfl, ?_⟩
    have hd : φ.scope.drop φs.scope.length = m := by rw [hm]; simp
    rw [hd] at hu
    simp only [Config.stack, hs, hm, ← List.append_assoc]
    exact StackDiscipline.cut (plainUnwind_locs hu)
  all_goals first
    | exact ⟨_, rfl, StackDiscipline.same⟩
    | exact ⟨[], by simp [Config.trace], StackDiscipline.same⟩
    | exact ⟨[], by simp [Config.trace], .inr ⟨List.nil_prefix, List.nil_sublist _⟩⟩

/-- **Last-in first-out, on every reachable step** (§6.7, §6.9, §6.10): the
registration stack is in location order, and a step that deregisters cells
drops only cells it deregistered, each newer than every cell still
registered. So a cell dropped at one teardown and a cell dropped at a later
one, still registered at the first, drop newest first: `{ let a; let b; }`
drops `b` before `a` over its two (D-EndScope) steps. No typing
hypothesis. -/
theorem reachable_stackDiscipline {M : FloatSig} {P : Program} {C C' : Config}
    (hr : Steps M P Config.init C) (h : Step M P C C') :
    ∃ evs, C'.trace = C.trace ++ evs ∧ StackDiscipline C.stack C'.stack (dropLocs evs) ∧
      (¬ C.stack <+: C'.stack → ∀ ℓ ∈ dropLocs evs, ℓ ∉ C'.stack ∧ ∀ ℓ' ∈ C'.stack, ℓ' < ℓ) := by
  have hn := reachable_nested hr
  obtain ⟨evs, ht, hl⟩ := step_stackDiscipline h hn
  refine ⟨evs, ht, hl, fun hcut => hl.newer ?_ hcut⟩
  cases C with
  | run H φ K f tr => exact hn.2.1
  | panic => exact .nil

/-! ## `drop_order` -/

/-- `Blocks` on §6's terminal configurations: a terminating `Step` run's trace
is the one `eval` answers (`eval_small_to_big`), so it is in the block grammar
(helper). -/
theorem step_blocks (M : FloatLaws) {P : Program} (h : ProgramTyped P) :
    (∀ H φ v tr, Steps M.toFloatSig P Config.init (.run H φ [] (.ret v) tr) → Blocks P.decls tr) ∧
    (∀ κ tr, Steps M.toFloatSig P Config.init (.panic κ tr) → Blocks P.decls tr) := by
  obtain ⟨hv, hp⟩ := eval_small_to_big M h
  have hdt := h.wf.decls.dtorNotCopy
  refine ⟨fun H φ v tr hs => ?_, fun κ tr hs => ?_⟩
  · obtain ⟨n, hn⟩ := hv H φ v tr hs
    have := run_blocks M.toFloatSig hdt (n + 1)
    rw [hn (n + 1) (Nat.lt_succ_self n)] at this
    exact this
  · obtain ⟨n, hn⟩ := hp κ tr hs
    have := run_blocks M.toFloatSig hdt (n + 1)
    rw [hn (n + 1) (Nat.lt_succ_self n)] at this
    exact this

/-- **§6.11's order within a value, in §6.11's own terms** (§3.9, §6.11;
RUE-2487), over §6's relation, for a program the checker accepts: every
terminating run's trace — a terminal value or a trap — is in the block grammar
`DropGlueBlocks`, whose drop blocks are §6.11's rules (`DropGlue`) rather than the
machine's walk. So each drop marker is followed by the value's own destructor
first (`3.9:28`), then its fields in declaration order (`3.9:13`), an array's
elements ascending (`3.9:15`), an enum's active payload only (`6.3:20`), every
`⊘` skipped. `drop_order`'s first half says the same of `dropEvents`; this
statement does not go through it, so a machine whose walk and `dropEvents`
change together still fails it. -/
theorem drop_glue_order (M : FloatLaws) {P : Program} (h : ProgramTyped P) :
    (∀ H φ v tr, Steps M.toFloatSig P Config.init (.run H φ [] (.ret v) tr) →
      DropGlueBlocks P.decls tr) ∧
    (∀ κ tr, Steps M.toFloatSig P Config.init (.panic κ tr) → DropGlueBlocks P.decls tr) := by
  obtain ⟨hv, hp⟩ := eval_small_to_big M h
  have hdt := h.wf.decls.dtorNotCopy
  refine ⟨fun H φ v tr hs => ?_, fun κ tr hs => ?_⟩
  · obtain ⟨n, hn⟩ := hv H φ v tr hs
    have := run_glue_blocks M.toFloatSig hdt (n + 1)
    rw [hn (n + 1) (Nat.lt_succ_self n)] at this
    exact this
  · obtain ⟨n, hn⟩ := hp κ tr hs
    have := run_glue_blocks M.toFloatSig hdt (n + 1)
    rw [hn (n + 1) (Nat.lt_succ_self n)] at this
    exact this

/-- **Drop order** (§3.9, §6.7, §6.9, §6.10, §6.11; §7's "no use-after-drop /
no leak of drops" bullet, its *when*), over §6's relation, for a program the
checker accepts:

* **within a value**: every terminating run's trace — a terminal value or a
  trap — is in §6.11's block grammar (`step_blocks`): every destructor event
  sits inside the walk of the drop marker before it — destructor first
  (`3.9:28`), fields in declaration order (`3.9:13`), array elements
  ascending (`3.9:15`), an enum's stored (active) payload only (`6.3:20`) —
  and nowhere else;
* **across cells**, at every step from a reachable configuration: its `drop`
  markers name one cell or distinct cells newest first (`StrictStackOrder`), and
  the step is last-in first-out on the registration stack (`StackDiscipline`): it keeps
  the stack, or cuts it back and drops only cells it deregistered — each
  newer than every cell still registered, by `reachable_nested`'s location
  order (`reachable_stackDiscipline`, `StackDiscipline.newer`). So across all its exit steps a
  scope's cells drop newest first, and before any older scope's.

Only the first half reads the typing hypothesis, through `eval_small_to_big` and
`DtorNotCopy`. -/
theorem drop_order (M : FloatLaws) {P : Program} (h : ProgramTyped P) :
    (∀ H φ v tr, Steps M.toFloatSig P Config.init (.run H φ [] (.ret v) tr) → Blocks P.decls tr) ∧
    (∀ κ tr, Steps M.toFloatSig P Config.init (.panic κ tr) → Blocks P.decls tr) ∧
    ∀ C C', Steps M.toFloatSig P Config.init C → Step M.toFloatSig P C C' →
      ∃ evs, C'.trace = C.trace ++ evs ∧ StrictStackOrder (dropLocs evs) ∧
        StackDiscipline C.stack C'.stack (dropLocs evs) ∧ (C.stack.Pairwise (· < ·)) := by
  refine ⟨(step_blocks M h).1, (step_blocks M h).2, fun C C' hr hs => ?_⟩
  obtain ⟨evs, ht, hn⟩ := reachable_drop_order hr hs
  obtain ⟨evs', ht', hl⟩ := step_stackDiscipline hs (reachable_nested hr)
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

mutual
/-- **§6.11's rules determine a drop's events** (RUE-2487): two derivations of
`DropGlue` for one contents give the same events. Stated without `dropEvents`,
so it says the rules themselves fix the order (helper). -/
theorem DropGlue.det {D : Decls} : ∀ {c : Contents} {e₁ e₂ : List Event},
    DropGlue D c e₁ → DropGlue D c e₂ → e₁ = e₂
  | _, _, _, .movedOut, .movedOut | _, _, _, .int, .int | _, _, _, .float, .float
  | _, _, _, .bool, .bool | _, _, _, .unit, .unit => rfl
  | _, _, _, .struct hd₁ _ hl₁, .struct hd₂ _ hl₂ => DropGlueSeq.det hl₁ hl₂
  | _, _, _, .structDtor hd₁ _ hl₁, .structDtor hd₂ _ hl₂ => by
      rw [DropGlueSeq.det hl₁ hl₂]
  | _, _, _, .struct hd₁ h₁ _, .structDtor hd₂ h₂ _ => by
      rw [hd₁] at hd₂; cases hd₂; rw [h₁] at h₂; cases h₂
  | _, _, _, .structDtor hd₁ h₁ _, .struct hd₂ h₂ _ => by
      rw [hd₁] at hd₂; cases hd₂; rw [h₁] at h₂; cases h₂
  | _, _, _, .array hl₁, .array hl₂ => DropGlueSeq.det hl₁ hl₂
  | _, _, _, .enum hl₁, .enum hl₂ => DropGlueSeq.det hl₁ hl₂

/-- The same over a list (helper). -/
theorem DropGlueSeq.det {D : Decls} : ∀ {cs : List Contents} {e₁ e₂ : List Event},
    DropGlueSeq D cs e₁ → DropGlueSeq D cs e₂ → e₁ = e₂
  | _, _, _, .nil, .nil => rfl
  | _, _, _, .cons h₁ t₁, .cons h₂ t₂ => by rw [DropGlue.det h₁ h₂, DropGlueSeq.det t₁ t₂]
end

/-- A drop marker in §6.11's own grammar is followed by one `DropGlue` walk of
what it names (helper). -/
theorem DropGlueBlocks.drop_inv {D : Decls} {ℓ : Nat} {c : Contents} {t : List Event}
    (h : DropGlueBlocks D (.drop ℓ c :: t)) :
    ∃ evs t', DropGlue D c evs ∧ t = evs ++ t' ∧ DropGlueBlocks D t' := by
  cases h with
  | drop hg h' => exact ⟨_, _, hg, rfl, h'⟩

/-- **No destructor outside a drop**: a trace that opens with a destructor
event is not in the grammar (§6.11). -/
theorem Blocks.not_dtor {D : Decls} {s : Nat} {c : Contents} {t : List Event} :
    ¬ Blocks D (.dtor s c :: t) := by
  intro h; cases h

end RueCore
