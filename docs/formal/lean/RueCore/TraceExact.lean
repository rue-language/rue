import RueCore.Trace

/-!
# RueCore.TraceExact — every owned value ends exactly once (§7)

`no_double_free` (`Trace.lean`) is §7's no-double-free bullet: no identity is
freed, or destroyed, twice. This module proves the other half of the
"no use-after-drop / no leak of drops" bullet: **exactly once**. Every owned
value present when an evaluation starts is, when the evaluation ends normally
or unwinds (by `return` or by `break`), in exactly one place — still in the
store, part of the result, or ended exactly once in the trace — so a value is
dropped on the normal path (the binding's `endscope`, §6.7) or on the unwind
path (the σ-walk of §6.9 and §6.10), never both and never neither.

## What "ended" means

`freedIds` (`Trace.lean`) reads the trace's markers, and since RUE-2427 every
way an owned value's life ends has one:

* a binding's drop, at scope exit, `@drop` or an overwrite (`drop ℓ c`,
  §6.11, §6.7, §6.8), and a declared-`linear` destructure's residue (§6.3),
  which drops each retained subtree with the same marker;
* a discarded temporary (`dropTemp v`, §6.7);
* a **consumption** (`consume c`): the shell a `match` leaves once its payload
  is bound (§6.6), and the path from `d` to the leaf a destructure leaves once
  the leaf is handed on and the residue dropped (§6.3). Neither runs a drop of
  its own — every member has already gone somewhere — but the value's life
  ends there, and without the event the count could not be an equality.

## Why "at the start"

A run starts from the empty store, so every identity it mentions is minted
during the run, and nothing in the run's result says which store indices were
minted for owned values. The statement therefore quantifies over the
identities an evaluation **starts** with — `a < H.length`, the cells' and the
held operands' — and holds at every evaluation, of every expression, from
every store. Every sub-evaluation of a checked run is a typed configuration,
so `drop_exactly_once`, stated over typed configurations, covers every moment
of every checked run: whatever is in a cell at that moment ends exactly once
by the time the enclosing evaluation does, or is still there, or is its
result. §7's bullet is about places, which live in a store; this is that
reading.

## The carve-outs

* **A trap ends nothing** (§6.12, `@panic`): a panic carries a trace but no
  store, and §5.7's `⊥_panic` runs no drop, so the claim is about the three
  results that carry a store — a value, an unwinding `return` and an
  unwinding `break`.
* **Pending siblings (RUE-2316).** A value already computed for one operand
  that a later operand destroys by `return` or `break` is discarded with the
  evaluation context and dropped by nothing — the calculus as written
  (`Dynamics.lean`, "Pending values"). `Expr.pendingSafe` excludes it
  syntactically: no operand after the first of a call, a literal or a binary
  operator, and no index of an indexed assignment, may contain `return` or
  `break`. `affine_lost_at_call_arg_breaks_exactness` below shows the
  hypothesis is load-bearing on a program the checker accepts.

The proof is the conservation law of `Trace.lean` read as an **equality**
(`eval_exact`), by the same fuel induction. It needs no typing derivation:
copy closure, which the machine maintains, and the two shape premises
RUE-2427 adds — `@dbg`'s operand is observable and a loop body's value is
`⟨⟩` — are what close every case where a value could otherwise vanish
unrecorded. Typing enters only through `soundness`: a checked configuration's
evaluation is never refused.
-/

namespace RueCore

/-! ## The syntactic carve-out -/

mutual
/-- Whether an expression contains a `return` anywhere — including under a
loop (helper). -/
def Expr.returns : Expr → Bool
  | .ret _ => true
  | .brk => false
  | .intLit _ _ _ | .floatLit _ _ | .boolLit _ | .unitLit | .use _ | .panic _
  | .drop _ => false
  | .binop _ e₁ e₂ | .letIn _ e₁ e₂ | .seq e₁ e₂ => e₁.returns || e₂.returns
  | .unop _ e | .intCast _ _ e | .fintrin _ e | .dbg e | .repeatArray _ e _
  | .assign _ e | .loop e => e.returns
  | .mkStruct _ args | .mkEnum _ _ args | .mkArray _ args | .call _ args
  | .indexRead _ args _ | .indexDrop _ args _ => Expr.returnsList args
  | .indexWrite _ idx _ e => e.returns || Expr.returnsList idx
  | .ite c e₁ e₂ => c.returns || e₁.returns || e₂.returns
  | .«match» scrut arms => scrut.returns || Expr.returnsList arms

/-- `Expr.returns` over a list (helper). -/
def Expr.returnsList : List Expr → Bool
  | [] => false
  | e :: es => e.returns || Expr.returnsList es
end

/-- Whether evaluating an expression can **unwind** past its context: it
contains a `return`, or a `break` its own loops do not catch
(`Expr.breaks`) (helper). -/
def Expr.unwinds (e : Expr) : Bool := e.returns || e.breaks

/-- No expression of the list unwinds (helper). -/
def Expr.quietList (es : List Expr) : Bool := es.all fun e => !e.unwinds

mutual
/-- **The RUE-2316 carve-out, syntactically**: no value computed for one
operand is pending while a later operand of the same form can unwind — a
call's arguments, a struct, enum or array literal's members, an index list,
a binary operator's two operands, and an indexed assignment's right-hand side
before its indices (`5.2:14`). The first operand may unwind: nothing is
pending yet. `binop` is in the list although RUE-2316's text does not name it:
its left operand is a scalar under (Arith) §5.8, so nothing owned is lost
there on a checked program, but the carve-out is syntactic and cannot see the
type. -/
def Expr.pendingSafe : Expr → Bool
  | .intLit _ _ _ | .floatLit _ _ | .boolLit _ | .unitLit | .use _ | .panic _
  | .drop _ | .brk => true
  | .binop _ e₁ e₂ => e₁.pendingSafe && e₂.pendingSafe && !e₂.unwinds
  | .unop _ e | .intCast _ _ e | .fintrin _ e | .dbg e | .repeatArray _ e _
  | .assign _ e | .ret e | .loop e => e.pendingSafe
  | .mkStruct _ args | .mkEnum _ _ args | .mkArray _ args | .call _ args
  | .indexRead _ args _ | .indexDrop _ args _ =>
      Expr.pendingSafeList args && Expr.quietList args.tail
  | .indexWrite _ idx _ e => e.pendingSafe && Expr.pendingSafeList idx && Expr.quietList idx
  | .letIn _ e₁ e₂ | .seq e₁ e₂ => e₁.pendingSafe && e₂.pendingSafe
  | .ite c e₁ e₂ => c.pendingSafe && e₁.pendingSafe && e₂.pendingSafe
  | .«match» scrut arms => scrut.pendingSafe && Expr.pendingSafeList arms

/-- `Expr.pendingSafe` over a list (helper). -/
def Expr.pendingSafeList : List Expr → Bool
  | [] => true
  | e :: es => e.pendingSafe && Expr.pendingSafeList es
end

/-- Every function body of the program is `pendingSafe` (helper). -/
def Program.pendingSafe (P : Program) : Bool := P.fns.all fun fd => fd.body.pendingSafe

/-- A member of a `pendingSafe` list is `pendingSafe` (helper). -/
theorem Expr.pendingSafeList_mem : ∀ {es : List Expr} {e : Expr},
    Expr.pendingSafeList es = true → e ∈ es → e.pendingSafe = true
  | [], _, _, h => by cases h
  | e :: es, e', h, hm => by
      simp only [Expr.pendingSafeList, Bool.and_eq_true] at h
      cases hm with
      | head => exact h.1
      | tail _ hm => exact Expr.pendingSafeList_mem h.2 hm

/-- A member of a list with no `return` has none (helper). -/
theorem Expr.returnsList_mem : ∀ {es : List Expr} {e : Expr},
    Expr.returnsList es = false → e ∈ es → e.returns = false
  | [], _, _, h => by cases h
  | e :: es, e', h, hm => by
      simp only [Expr.returnsList, Bool.or_eq_false_iff] at h
      cases hm with
      | head => exact h.1
      | tail _ hm => exact Expr.returnsList_mem h.2 hm

/-- A member of a list with no free `break` has none (helper). -/
theorem Expr.breaksList_mem : ∀ {es : List Expr} {e : Expr},
    Expr.breaksList es = false → e ∈ es → e.breaks = false
  | [], _, _, h => by cases h
  | e :: es, e', h, hm => by
      simp only [Expr.breaksList, Bool.or_eq_false_iff] at h
      cases hm with
      | head => exact h.1
      | tail _ hm => exact Expr.breaksList_mem h.2 hm

/-- A member of a quiet list does not unwind (helper). -/
theorem Expr.quietList_mem {es : List Expr} {e : Expr} (h : Expr.quietList es = true)
    (hm : e ∈ es) : e.returns = false ∧ e.breaks = false := by
  simp only [Expr.quietList, List.all_eq_true, Bool.not_eq_eq_eq_not, Bool.not_true] at h
  have := h e hm
  simp only [Expr.unwinds, Bool.or_eq_false_iff] at this
  exact this

/-! ## Results that do not unwind -/

/-- A result that is not an unwinding `return` (helper). -/
def EvalRes.NoRet : EvalRes → Prop
  | .returned _ _ _ => False
  | _ => True

/-- A result that is not an unwinding `break` (helper). -/
def EvalRes.NoBrk : EvalRes → Prop
  | .broke _ _ _ => False
  | _ => True

/-- `andThen` unwinds only where its operand or its context does (helper). -/
theorem EvalRes.andThen_noRet {r : EvalRes} {k : Store → Val → EvalRes} (hr : r.NoRet)
    (hk : ∀ H v tr, r = .ok H v tr → (k H v).NoRet) : (r.andThen k).NoRet := by
  cases r with
  | ok H v tr =>
      have := hk H v tr rfl
      simp only [EvalRes.andThen]
      cases h : k H v <;> simp_all [EvalRes.withTrace, EvalRes.NoRet]
  | _ => simp_all [EvalRes.andThen, EvalRes.NoRet]

/-- The same for `break` (helper). -/
theorem EvalRes.andThen_noBrk {r : EvalRes} {k : Store → Val → EvalRes} (hr : r.NoBrk)
    (hk : ∀ H v tr, r = .ok H v tr → (k H v).NoBrk) : (r.andThen k).NoBrk := by
  cases r with
  | ok H v tr =>
      have := hk H v tr rfl
      simp only [EvalRes.andThen]
      cases h : k H v <;> simp_all [EvalRes.withTrace, EvalRes.NoBrk]
  | _ => simp_all [EvalRes.andThen, EvalRes.NoBrk]

/-- A prefixed trace does not change whether a result unwinds (helper). -/
theorem EvalRes.withTrace_noRet {r : EvalRes} {tr : List Event} (h : r.NoRet) :
    (r.withTrace tr).NoRet := by
  cases r <;> simp_all [EvalRes.withTrace, EvalRes.NoRet]

/-- The same for `break` (helper). -/
theorem EvalRes.withTrace_noBrk {r : EvalRes} {tr : List Event} (h : r.NoBrk) :
    (r.withTrace tr).NoBrk := by
  cases r <;> simp_all [EvalRes.withTrace, EvalRes.NoBrk]

/-- An argument list whose members do not return aborts with no `return`
(helper). -/
theorem evalArgs_noRet {ev : Store → Expr → EvalRes} :
    ∀ {es : List Expr} (H : Store), (∀ H e, e ∈ es → (ev H e).NoRet) →
      ∀ r, evalArgs ev H es = .abort r → r.NoRet
  | [], H, _, r, h => by simp [evalArgs] at h
  | e :: es, H, hq, r, h => by
      simp only [evalArgs] at h
      split at h
      · rename_i H₁ v tr he
        split at h
        · cases h
        · rename_i r' hr'
          cases h
          exact EvalRes.withTrace_noRet
            (evalArgs_noRet H₁ (fun H e' hm => hq H e' (List.mem_cons_of_mem _ hm)) r' hr')
      · cases h; exact hq H e List.mem_cons_self

/-- The same for `break` (helper). -/
theorem evalArgs_noBrk {ev : Store → Expr → EvalRes} :
    ∀ {es : List Expr} (H : Store), (∀ H e, e ∈ es → (ev H e).NoBrk) →
      ∀ r, evalArgs ev H es = .abort r → r.NoBrk
  | [], H, _, r, h => by simp [evalArgs] at h
  | e :: es, H, hq, r, h => by
      simp only [evalArgs] at h
      split at h
      · rename_i H₁ v tr he
        split at h
        · cases h
        · rename_i r' hr'
          cases h
          exact EvalRes.withTrace_noBrk
            (evalArgs_noBrk H₁ (fun H e' hm => hq H e' (List.mem_cons_of_mem _ hm)) r' hr')
      · cases h; exact hq H e List.mem_cons_self

/-- `introVal` produces a value or a refusal (helper). -/
theorem introVal_quiet {D : Decls} {H : Store} {mk : Nat → Val} :
    (introVal D H mk).NoRet ∧ (introVal D H mk).NoBrk := by
  unfold introVal; split <;> exact ⟨trivial, trivial⟩

/-- An operator's outcome is a value, a trap or a refusal (helper). -/
theorem OpRes.toRes_quiet {H : Store} {o : OpRes} : (o.toRes H).NoRet ∧ (o.toRes H).NoBrk := by
  cases o <;> exact ⟨trivial, trivial⟩

/-- §6.9's call boundary never hands on an unwinding `return` or `break`
(helper). -/
theorem EvalRes.absorb_quiet {r : EvalRes} {k : Store → Val → EvalRes}
    (hk : ∀ H v, (k H v).NoRet ∧ (k H v).NoBrk) :
    (r.absorb k).NoRet ∧ (r.absorb k).NoBrk := by
  cases r with
  | ok H v tr =>
      simp only [EvalRes.absorb]
      exact ⟨EvalRes.withTrace_noRet (hk H v).1, EvalRes.withTrace_noBrk (hk H v).2⟩
  | _ => exact ⟨trivial, trivial⟩

/-- **What does not unwind, does not unwind**: an expression with no `return`
never evaluates to an unwinding `return`, and one with no free `break` never
to an unwinding `break` — a call absorbs its callee's `return` (§6.9) and a
loop catches its body's `break` (§6.10) (helper). -/
theorem eval_quiet (M : FloatOps) (P : Program) : ∀ (fuel : Nat) (H : Store) (φ : Frame)
    (e : Expr), (e.returns = false → (eval M fuel P H φ e).NoRet) ∧
      (e.breaks = false → (eval M fuel P H φ e).NoBrk) := by
  intro fuel
  induction fuel with
  | zero => intro H φ e; exact ⟨fun _ => trivial, fun _ => trivial⟩
  | succ n ih =>
    intro H φ e
    have hargR := fun (H : Store) (es : List Expr) =>
      evalArgs_noRet (ev := fun H' e' => eval M n P H' φ e') (es := es) H
    have hargB := fun (H : Store) (es : List Expr) =>
      evalArgs_noBrk (ev := fun H' e' => eval M n P H' φ e') (es := es) H
    cases e with
    | intLit | floatLit | boolLit | unitLit | panic =>
        simp only [eval]; exact ⟨fun _ => trivial, fun _ => trivial⟩
    | use p | drop p =>
        simp only [eval]
        refine ⟨fun _ => ?_, fun _ => ?_⟩ <;> (repeat' split) <;> trivial
    | brk => simp only [eval]; exact ⟨fun _ => trivial, fun h => by simp [Expr.breaks] at h⟩
    | binop op e₁ e₂ =>
        simp only [eval]
        refine ⟨fun h => ?_, fun h => ?_⟩
        · simp only [Expr.returns, Bool.or_eq_false_iff] at h
          exact EvalRes.andThen_noRet ((ih H φ e₁).1 h.1) fun H₁ _ _ _ =>
            EvalRes.andThen_noRet ((ih H₁ φ e₂).1 h.2) fun _ _ _ _ => OpRes.toRes_quiet.1
        · simp only [Expr.breaks, Bool.or_eq_false_iff] at h
          exact EvalRes.andThen_noBrk ((ih H φ e₁).2 h.1) fun H₁ _ _ _ =>
            EvalRes.andThen_noBrk ((ih H₁ φ e₂).2 h.2) fun _ _ _ _ => OpRes.toRes_quiet.2
    | unop op e₁ | intCast w sg e₁ | fintrin k e₁ =>
        simp only [eval]
        refine ⟨fun h => ?_, fun h => ?_⟩
        · simp only [Expr.returns] at h
          exact EvalRes.andThen_noRet ((ih H φ e₁).1 h) fun _ _ _ _ => OpRes.toRes_quiet.1
        · simp only [Expr.breaks] at h
          exact EvalRes.andThen_noBrk ((ih H φ e₁).2 h) fun _ _ _ _ => OpRes.toRes_quiet.2
    | dbg e₁ =>
        simp only [eval]
        refine ⟨fun h => ?_, fun h => ?_⟩
        · simp only [Expr.returns] at h
          exact EvalRes.andThen_noRet ((ih H φ e₁).1 h) fun _ _ _ _ => by split <;> trivial
        · simp only [Expr.breaks] at h
          exact EvalRes.andThen_noBrk ((ih H φ e₁).2 h) fun _ _ _ _ => by split <;> trivial
    | repeatArray T e₁ m =>
        simp only [eval]
        refine ⟨fun h => ?_, fun h => ?_⟩
        · simp only [Expr.returns] at h
          exact EvalRes.andThen_noRet ((ih H φ e₁).1 h) fun _ _ _ _ => by
            split
            · exact introVal_quiet.1
            · trivial
        · simp only [Expr.breaks] at h
          exact EvalRes.andThen_noBrk ((ih H φ e₁).2 h) fun _ _ _ _ => by
            split
            · exact introVal_quiet.2
            · trivial
    | mkStruct s args | mkEnum e k args | mkArray T args | indexRead p args πs =>
        simp only [eval]
        refine ⟨fun h => ?_, fun h => ?_⟩
        · simp only [Expr.returns] at h
          split
          · rename_i r hr
            exact hargR H args (fun H' e' hm => (ih H' φ e').1 (Expr.returnsList_mem h hm)) r hr
          · apply EvalRes.withTrace_noRet
            (repeat' split) <;> first | trivial | exact introVal_quiet.1
        · simp only [Expr.breaks] at h
          split
          · rename_i r hr
            exact hargB H args (fun H' e' hm => (ih H' φ e').2 (Expr.breaksList_mem h hm)) r hr
          · apply EvalRes.withTrace_noBrk
            (repeat' split) <;> first | trivial | exact introVal_quiet.2
    | indexDrop p idx πs =>
        simp only [eval]
        refine ⟨fun h => ?_, fun h => ?_⟩
        · exact EvalRes.andThen_noRet ((ih H φ (.indexRead p idx πs)).1 (by
            simpa [Expr.returns] using h)) fun _ _ _ _ => trivial
        · exact EvalRes.andThen_noBrk ((ih H φ (.indexRead p idx πs)).2 (by
            simpa [Expr.breaks] using h)) fun _ _ _ _ => trivial
    | indexWrite p idx πs e₁ =>
        simp only [eval]
        refine ⟨fun h => ?_, fun h => ?_⟩
        · simp only [Expr.returns, Bool.or_eq_false_iff] at h
          refine EvalRes.andThen_noRet ((ih H φ e₁).1 h.1) fun H₁ _ _ _ => ?_
          split
          · rename_i r hr
            exact hargR H₁ idx (fun H' e' hm => (ih H' φ e').1 (Expr.returnsList_mem h.2 hm)) r hr
          · apply EvalRes.withTrace_noRet
            (repeat' split) <;> trivial
        · simp only [Expr.breaks, Bool.or_eq_false_iff] at h
          refine EvalRes.andThen_noBrk ((ih H φ e₁).2 h.1) fun H₁ _ _ _ => ?_
          split
          · rename_i r hr
            exact hargB H₁ idx (fun H' e' hm => (ih H' φ e').2 (Expr.breaksList_mem h.2 hm)) r hr
          · apply EvalRes.withTrace_noBrk
            (repeat' split) <;> trivial
    | «match» scrut arms =>
        simp only [eval]
        refine ⟨fun h => ?_, fun h => ?_⟩
        · simp only [Expr.returns, Bool.or_eq_false_iff] at h
          refine EvalRes.andThen_noRet ((ih H φ scrut).1 h.1) fun H₀ v _ _ => ?_
          split
          · split
            · trivial
            · rename_i body hb
              exact EvalRes.withTrace_noRet (EvalRes.andThen_noRet
                ((ih _ _ body).1 (Expr.returnsList_mem h.2 (List.mem_of_getElem? hb)))
                fun _ _ _ _ => by split <;> trivial)
          · trivial
        · simp only [Expr.breaks, Bool.or_eq_false_iff] at h
          refine EvalRes.andThen_noBrk ((ih H φ scrut).2 h.1) fun H₀ v _ _ => ?_
          split
          · split
            · trivial
            · rename_i body hb
              exact EvalRes.withTrace_noBrk (EvalRes.andThen_noBrk
                ((ih _ _ body).2 (Expr.breaksList_mem h.2 (List.mem_of_getElem? hb)))
                fun _ _ _ _ => by split <;> trivial)
          · trivial
    | letIn m e₁ e₂ =>
        simp only [eval]
        refine ⟨fun h => ?_, fun h => ?_⟩
        · simp only [Expr.returns, Bool.or_eq_false_iff] at h
          exact EvalRes.andThen_noRet ((ih H φ e₁).1 h.1) fun _ _ _ _ =>
            EvalRes.andThen_noRet ((ih _ _ e₂).1 h.2) fun _ _ _ _ => by split <;> trivial
        · simp only [Expr.breaks, Bool.or_eq_false_iff] at h
          exact EvalRes.andThen_noBrk ((ih H φ e₁).2 h.1) fun _ _ _ _ =>
            EvalRes.andThen_noBrk ((ih _ _ e₂).2 h.2) fun _ _ _ _ => by split <;> trivial
    | assign p e₁ =>
        simp only [eval]
        refine ⟨fun h => ?_, fun h => ?_⟩
        · simp only [Expr.returns] at h
          exact EvalRes.andThen_noRet ((ih H φ e₁).1 h) fun _ _ _ _ => by
            (repeat' split) <;> trivial
        · simp only [Expr.breaks] at h
          exact EvalRes.andThen_noBrk ((ih H φ e₁).2 h) fun _ _ _ _ => by
            (repeat' split) <;> trivial
    | seq e₁ e₂ =>
        simp only [eval]
        refine ⟨fun h => ?_, fun h => ?_⟩
        · simp only [Expr.returns, Bool.or_eq_false_iff] at h
          exact EvalRes.andThen_noRet ((ih H φ e₁).1 h.1) fun H₁ _ _ _ => by
            split
            · trivial
            · split
              · trivial
              · exact EvalRes.withTrace_noRet ((ih H₁ φ e₂).1 h.2)
            · exact (ih H₁ φ e₂).1 h.2
        · simp only [Expr.breaks, Bool.or_eq_false_iff] at h
          exact EvalRes.andThen_noBrk ((ih H φ e₁).2 h.1) fun H₁ _ _ _ => by
            split
            · trivial
            · split
              · trivial
              · exact EvalRes.withTrace_noBrk ((ih H₁ φ e₂).2 h.2)
            · exact (ih H₁ φ e₂).2 h.2
    | ite c e₁ e₂ =>
        simp only [eval]
        refine ⟨fun h => ?_, fun h => ?_⟩
        · simp only [Expr.returns, Bool.or_eq_false_iff] at h
          exact EvalRes.andThen_noRet ((ih H φ c).1 h.1.1) fun H₀ _ _ _ => by
            split
            · split
              · exact (ih H₀ φ e₁).1 h.1.2
              · exact (ih H₀ φ e₂).1 h.2
            · trivial
        · simp only [Expr.breaks, Bool.or_eq_false_iff] at h
          exact EvalRes.andThen_noBrk ((ih H φ c).2 h.1.1) fun H₀ _ _ _ => by
            split
            · split
              · exact (ih H₀ φ e₁).2 h.1.2
              · exact (ih H₀ φ e₂).2 h.2
            · trivial
    | call f args =>
        simp only [eval]
        refine ⟨fun h => ?_, fun h => ?_⟩
        · simp only [Expr.returns] at h
          split
          · rename_i r hr
            exact hargR H args (fun H' e' hm => (ih H' φ e').1 (Expr.returnsList_mem h hm)) r hr
          · apply EvalRes.withTrace_noRet
            split
            · trivial
            · split
              · exact (EvalRes.absorb_quiet fun _ _ => by constructor <;> (split <;> trivial)).1
              · trivial
        · simp only [Expr.breaks] at h
          split
          · rename_i r hr
            exact hargB H args (fun H' e' hm => (ih H' φ e').2 (Expr.breaksList_mem h hm)) r hr
          · apply EvalRes.withTrace_noBrk
            split
            · trivial
            · split
              · exact (EvalRes.absorb_quiet fun _ _ => by constructor <;> (split <;> trivial)).2
              · trivial
    | ret e₁ =>
        simp only [eval]
        refine ⟨fun h => by simp [Expr.returns] at h, fun h => ?_⟩
        simp only [Expr.breaks] at h
        exact EvalRes.andThen_noBrk ((ih H φ e₁).2 h) fun _ _ _ _ => by
          split <;> trivial
    | loop e₁ =>
        simp only [eval]
        refine ⟨fun h => ?_, fun _ => ?_⟩
        · simp only [Expr.returns] at h
          have hb := (ih H φ e₁).1 h
          cases hr : eval M n P H φ e₁ with
          | ok H₁ v tr =>
              cases v with
              | unit => exact EvalRes.withTrace_noRet ((ih H₁ φ (.loop e₁)).1 h)
              | _ => trivial
          | broke H₁ sc tr => dsimp only; split <;> trivial
          | returned => rw [hr] at hb; exact hb
          | panic => trivial
          | stuck => trivial
          | outOfFuel => trivial
        · cases hr : eval M n P H φ e₁ with
          | ok H₁ v tr =>
              cases v with
              | unit => exact EvalRes.withTrace_noBrk ((ih H₁ φ (.loop e₁)).2 rfl)
              | _ => trivial
          | broke H₁ sc tr => dsimp only; split <;> trivial
          | returned => trivial
          | panic => trivial
          | stuck => trivial
          | outOfFuel => trivial

/-! ## The equalities the ledger is made of -/

/-- `freedIds` distributes over concatenation (helper). -/
theorem freedIds_append (D : Decls) (l₁ l₂ : List Event) :
    freedIds D (l₁ ++ l₂) = freedIds D l₁ ++ freedIds D l₂ := by
  simp [freedIds, List.flatMap_append]

/-- A path written is a path read back (helper). -/
theorem Contents.readAt_writeAt : ∀ (π : List Nat) {c new c' : Contents},
    c.writeAt π new = some c' → c'.readAt π = .ok new
  | [], c, new, c', hw => by simp [Contents.writeAt] at hw; subst hw; rfl
  | f :: π, c, new, c', hw => by
      cases c with
      | struct s i cs =>
          simp only [Contents.writeAt] at hw
          split at hw
          · rename_i cf hcf
            simp only [Option.map_eq_some_iff] at hw
            obtain ⟨cf', hw', rfl⟩ := hw
            have hlt : f < cs.length := (List.getElem?_eq_some_iff.mp hcf).1
            simp only [Contents.readAt, List.getElem?_set_self hlt]
            exact Contents.readAt_writeAt π hw'
          · cases hw
      | array T i cs =>
          simp only [Contents.writeAt] at hw
          split at hw
          · rename_i cf hcf
            simp only [Option.map_eq_some_iff] at hw
            obtain ⟨cf', hw', rfl⟩ := hw
            have hlt : f < cs.length := (List.getElem?_eq_some_iff.mp hcf).1
            simp only [Contents.readAt, List.getElem?_set_self hlt]
            exact Contents.readAt_writeAt π hw'
          · cases hw
      | _ => simp [Contents.writeAt] at hw

/-- **A write at a path, counted exactly**: the contents after the write owns
what it owned before, less what sat at the path, plus what was written —
§6.3's move and §6.8's store, read as an equation. Below a `Copy` node both
sides own nothing, which is where copy closure of both the old and the new
contents is needed (helper). -/
theorem Contents.writeAt_own_eq {D : Decls} (a : Nat) : ∀ (π : List Nat) {c sub new c' : Contents},
    c.copyClosed D = true → c'.copyClosed D = true → c.readAt π = .ok sub →
    c.writeAt π new = some c' →
    (c'.own D).count a + (sub.own D).count a = (c.own D).count a + (new.own D).count a
  | [], c, sub, new, c', _, _, hr, hw => by
      simp [Contents.readAt] at hr; simp [Contents.writeAt] at hw; subst hr; subst hw; omega
  | f :: π, c, sub, new, c', hcc, hcc', hr, hw => by
      have hnew : new.copyClosed D = true :=
        Contents.readAt_copyClosed (f :: π) hcc' (Contents.readAt_writeAt (f :: π) hw)
      cases c with
      | struct s i cs =>
          simp only [Contents.readAt] at hr
          simp only [Contents.writeAt] at hw
          split at hr
          · rename_i cf hcf
            rw [hcf] at hw
            simp only [Option.map_eq_some_iff] at hw
            obtain ⟨cf', hw', rfl⟩ := hw
            have hlt : f < cs.length := (List.getElem?_eq_some_iff.mp hcf).1
            simp only [Contents.copyClosed] at hcc hcc'
            by_cases hc : D.classOf s = .copy
            · rw [if_pos hc] at hcc hcc'
              have hsub := Contents.readAt_allCopy π (Contents.allCopyList_index hcc hcf) hr
              have hcf' : cf'.allCopy D = true :=
                Contents.allCopyList_index hcc' (List.getElem?_set_self hlt)
              have hn := Contents.readAt_allCopy π hcf' (Contents.readAt_writeAt π hw')
              simp [Contents.own, hc, Contents.allCopy_own hsub, Contents.allCopy_own hn]
            · rw [if_neg hc] at hcc hcc'
              have hcf' : cf'.copyClosed D = true :=
                Contents.copyClosedList_index hcc' (List.getElem?_set_self hlt)
              have ih := Contents.writeAt_own_eq a π (Contents.copyClosedList_index hcc hcf) hcf' hr hw'
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
            have hlt : f < cs.length := (List.getElem?_eq_some_iff.mp hcf).1
            simp only [Contents.copyClosed, List.length_set] at hcc hcc'
            by_cases hc : Ty.mult D (.array T cs.length) = .copy
            · rw [if_pos hc] at hcc hcc'
              have hsub := Contents.readAt_allCopy π (Contents.allCopyList_index hcc hcf) hr
              have hcf' : cf'.allCopy D = true :=
                Contents.allCopyList_index hcc' (List.getElem?_set_self hlt)
              have hn := Contents.readAt_allCopy π hcf' (Contents.readAt_writeAt π hw')
              simp [Contents.own, hc, Contents.allCopy_own hsub, Contents.allCopy_own hn]
            · rw [if_neg hc] at hcc hcc'
              have hcf' : cf'.copyClosed D = true :=
                Contents.copyClosedList_index hcc' (List.getElem?_set_self hlt)
              have ih := Contents.writeAt_own_eq a π (Contents.copyClosedList_index hcc hcf) hcf' hr hw'
              have hset := Contents.ownList_set_count (D := D) a cf' hcf
              simp only [Contents.own, List.length_set, if_neg hc, List.count_cons]
              omega
          · cases hr
      | _ => simp [Contents.readAt] at hr

/-- **A binding's drop frees exactly the tree it names** (§6.11): the marker
names every owned node, the walk under it names none, and a `Copy` tree has
neither (helper). -/
theorem dropCell_freed {D : Decls} {ℓ : Nat} {c : Contents} {evs : List Event}
    (h : dropCell D ℓ c = .ok evs) : freedIds D evs = c.own D := by
  unfold dropCell at h
  split at h
  · rename_i hm; cases h; simp [freedIds, Contents.own_of_mult hm]
  · split at h
    · cases h
    · rename_i evs' hw
      cases h
      have := dropContents_freed hw
      simp [freedIds, Event.freed, this]

/-- One residue subtree's drop frees exactly the subtree (helper). -/
theorem residueMark_freed {D : Decls} {ℓ : Nat} {r : Contents} {evs : List Event}
    (h : dropContents D r = .ok evs) : freedIds D (residueMark D ℓ r ++ evs) = r.own D := by
  have := dropContents_freed h
  unfold residueMark
  split
  · rename_i hm; simp [freedIds, this, Contents.own_of_mult hm]
  · simp [freedIds, Event.freed, this]

/-- `drop*` over a destructure's residue frees exactly the residue (helper). -/
theorem dropResidue_freed {D : Decls} {ℓ : Nat} : ∀ {rs : List Contents} {evs : List Event},
    dropResidue D ℓ rs = .ok evs → freedIds D evs = Contents.ownList D rs
  | [], _, h => by simp [dropResidue] at h; subst h; rfl
  | r :: rs, evs, h => by
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
            rw [freedIds_append, residueMark_freed h₁, dropResidue_freed h₂]
            rfl

/-- **§6.3's destructure, counted exactly**: the leaf it hands on and what its
trace ends — the residue's drops and the path's consumption — are exactly
what the consumed place owned (helper). -/
theorem Contents.destructure_exact {D : Decls} {ℓ : Nat} {cd leaf : Contents} {πs : List Nat}
    {evs : List Event} (hcc : cd.copyClosed D = true) (h : cd.destructure D ℓ πs = .ok (leaf, evs))
    (a : Nat) :
    (leaf.own D).count a + (freedIds D evs).count a = (cd.own D).count a ∧
      leaf.copyClosed D = true := by
  unfold Contents.destructure at h
  split at h
  · cases h
  · rename_i leaf' rs hs
    split at h
    · cases h
    · rename_i evs' hd
      cases h
      refine ⟨?_, (Contents.splitResidue_own 0 πs hcc hs).2.1⟩
      have := Contents.skeleton_own a πs hcc hs
      rw [freedIds_append, dropResidue_freed hd]
      simp only [freedIds, List.flatMap_cons, List.flatMap_nil, Event.freed, List.append_nil,
        List.count_append] at this ⊢
      omega

/-- `drop-retire` (§6.1), counted exactly: the cell's owned identities leave
the store and exactly those reach the trace (helper). -/
theorem dropRetire_exact {D : Decls} {H H' : Store} {ℓ : Nat} {evs : List Event}
    (hcc : StoreCC D H) (h : dropRetire D H ℓ = .ok (H', evs)) :
    (∀ a, (storeOwn D H').count a + (freedIds D evs).count a = (storeOwn D H).count a) ∧
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
        rw [dropCell_freed hd]
        simp only [Cell.own, List.count_nil] at h1
        omega

/-- `run-scope-drops` (§6.1), counted exactly (helper). -/
theorem unwindLocs_exact {D : Decls} : ∀ {H H' : Store} {ls : List Nat} {evs : List Event},
    StoreCC D H → unwindLocs D H ls = .ok (H', evs) →
      (∀ a, (storeOwn D H').count a + (freedIds D evs).count a = (storeOwn D H).count a) ∧
        H'.length = H.length ∧ StoreCC D H'
  | H, H', [], evs, hcc, h => by
      simp [unwindLocs] at h; obtain ⟨rfl, rfl⟩ := h
      exact ⟨fun a => by simp [freedIds], rfl, hcc⟩
  | H, H', ℓ :: ls, evs, hcc, h => by
      simp only [unwindLocs] at h
      split at h
      · cases h
      · rename_i H₁ evs₁ h₁
        split at h
        · cases h
        · rename_i H₂ evs₂ h₂
          cases h
          obtain ⟨i₁, l₁, c₁⟩ := dropRetire_exact hcc h₁
          obtain ⟨i₂, l₂, c₂⟩ := unwindLocs_exact c₁ h₂
          refine ⟨fun a => ?_, by omega, c₂⟩
          have := i₁ a; have := i₂ a
          rw [freedIds_append, List.count_append]
          omega

/-- **(D-Match)'s consumption, counted exactly** (RUE-2427): the payload the
arm's cells receive and the shell `matchConsume` ends are exactly the
scrutinee (helper). -/
theorem matchConsume_exact {D : Decls} {e k i : Nat} {vs : List Val}
    (h : (Contents.enum e k i (Contents.ofVals vs)).copyClosed D = true) (a : Nat) :
    (Contents.ownList D (Contents.ofVals vs)).count a + (freedIds D (matchConsume D e k i vs)).count a
      = ((Contents.enum e k i (Contents.ofVals vs)).own D).count a := by
  unfold matchConsume
  simp only [Contents.copyClosed] at h
  split
  · rename_i hc
    rw [if_pos hc] at h
    simp [Contents.allCopyList_own h, Contents.own, hc, freedIds]
  · rename_i hc
    simp [freedIds, Event.freed, Contents.own, hc, Contents.ownList_holes, List.count_cons]

end RueCore
