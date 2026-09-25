import RueCore.Trace
import RueCore.Adequacy

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

/-- A fresh aggregate owns exactly its members, apart from its own identity:
a `Copy` node owns nothing, and — copy-closed — neither do its members
(helper). -/
theorem Contents.own_struct_fresh {D : Decls} {s i : Nat} {cs : List Contents}
    (h : (Contents.struct s i cs).copyClosed D = true) {a : Nat} (ha : a ≠ i) :
    ((Contents.struct s i cs).own D).count a = (Contents.ownList D cs).count a := by
  simp only [Contents.copyClosed] at h
  simp only [Contents.own]
  split
  · rename_i hc; rw [if_pos hc] at h; simp [Contents.allCopyList_own h]
  · simp [Ne.symm ha]

/-- The same at an enum (helper). -/
theorem Contents.own_enum_fresh {D : Decls} {e k i : Nat} {cs : List Contents}
    (h : (Contents.enum e k i cs).copyClosed D = true) {a : Nat} (ha : a ≠ i) :
    ((Contents.enum e k i cs).own D).count a = (Contents.ownList D cs).count a := by
  simp only [Contents.copyClosed] at h
  simp only [Contents.own]
  split
  · rename_i hc; rw [if_pos hc] at h; simp [Contents.allCopyList_own h]
  · simp [Ne.symm ha]

/-- The same at an array (helper). -/
theorem Contents.own_array_fresh {D : Decls} {T : Ty} {i : Nat} {cs : List Contents}
    (h : (Contents.array T i cs).copyClosed D = true) {a : Nat} (ha : a ≠ i) :
    ((Contents.array T i cs).own D).count a = (Contents.ownList D cs).count a := by
  simp only [Contents.copyClosed] at h
  simp only [Contents.own]
  split
  · rename_i hc; rw [if_pos hc] at h; simp [Contents.allCopyList_own h]
  · simp [Ne.symm ha]

/-- Integer index values own nothing (helper). -/
theorem Val.ints_own {D : Decls} : ∀ {vs : List Val} {is : List Int},
    Val.ints vs = some is → Contents.ownList D (Contents.ofVals vs) = []
  | [], _, _ => rfl
  | .int _ _ _ :: vs, is, h => by
      simp only [Val.ints] at h
      split at h
      · rename_i is' h'
        simp [Contents.ofVals, Contents.ownList, Contents.ofVal, Contents.own, Val.ints_own h']
      · cases h
  | .float _ _ :: _, _, h | .bool _ :: _, _, h | .unit :: _, _, h | .struct _ _ _ :: _, _, h
  | .enum _ _ _ _ :: _, _, h | .array _ _ _ :: _, _, h => by simp [Val.ints] at h

/-- `dynPlace` lands only after its indices were integers (helper). -/
theorem dynPlace_ints {H : Store} {φ : Frame} {p : Place} {vs : List Val} {πs : List (List Nat)}
    (h : ∀ w, dynPlace H φ p vs πs ≠ .stuck w) : ∃ is, Val.ints vs = some is := by
  unfold dynPlace at h
  split at h
  · exact absurd rfl (h .typeConfusion)
  · rename_i is his; exact ⟨is, his⟩

/-! ## The ledger, as an equality -/

/-- **The exact ledger for one evaluation** from store `H`, holding the owned
identities `X` besides it (a pending operand's value): for every identity the
evaluation **starts** with (`a < H.length`), the result's store, its value
and the identities the trace ends together hold it exactly as often as `H` and
`X` did — it is still in the store, in the result, or ended once in the trace,
and nothing is lost or duplicated. Identities minted during the evaluation
(`≥ H.length`) are not counted; `no_double_free` bounds those. A trap, a
refusal and exhausted fuel promise nothing (helper). -/
def Exact (D : Decls) (H : Store) (X : List Nat) : EvalRes → Prop
  | .ok H' v tr | .returned H' v tr =>
      H.length ≤ H'.length ∧ StoreCC D H' ∧ (Contents.ofVal v).copyClosed D = true ∧
      ∀ a, a < H.length → (storeOwn D H').count a + (v.own D).count a + (freedIds D tr).count a
        = (storeOwn D H).count a + X.count a
  | .broke H' _ tr =>
      H.length ≤ H'.length ∧ StoreCC D H' ∧
      ∀ a, a < H.length → (storeOwn D H').count a + (freedIds D tr).count a
        = (storeOwn D H).count a + X.count a
  | .panic _ _ | .stuck _ | .outOfFuel => True

/-- **Composition**: a step from `H` to `H₁` that ended `tr` and left `Y`
held, followed by an evaluation from `H₁` that keeps its own ledger, keeps the
ledger from `H` (helper). -/
theorem Exact.prefix {D : Decls} {H H₁ : Store} {X Y : List Nat} {tr : List Event} {r : EvalRes}
    (hle : H.length ≤ H₁.length)
    (hI : ∀ a, a < H.length → (storeOwn D H₁).count a + Y.count a + (freedIds D tr).count a
      = (storeOwn D H).count a + X.count a)
    (hr : Exact D H₁ Y r) : Exact D H X (r.withTrace tr) := by
  cases r with
  | ok H₂ v tr₂ =>
      obtain ⟨h1, h2, h3, h4⟩ := hr
      refine ⟨Nat.le_trans hle h1, h2, h3, fun a ha => ?_⟩
      have := hI a ha; have := h4 a (by omega)
      rw [freedIds_append, List.count_append]
      omega
  | returned H₂ v tr₂ =>
      obtain ⟨h1, h2, h3, h4⟩ := hr
      refine ⟨Nat.le_trans hle h1, h2, h3, fun a ha => ?_⟩
      have := hI a ha; have := h4 a (by omega)
      rw [freedIds_append, List.count_append]
      omega
  | broke H₂ sc tr₂ =>
      obtain ⟨h1, h2, h4⟩ := hr
      refine ⟨Nat.le_trans hle h1, h2, fun a ha => ?_⟩
      have := hI a ha; have := h4 a (by omega)
      rw [freedIds_append, List.count_append]
      omega
  | panic k tr₂ => trivial
  | stuck w => trivial
  | outOfFuel => trivial

/-- A step that ended nothing (helper). -/
theorem Exact.shift {D : Decls} {H H₁ : Store} {X Y : List Nat} {r : EvalRes}
    (hle : H.length ≤ H₁.length)
    (hI : ∀ a, a < H.length → (storeOwn D H₁).count a + Y.count a
      = (storeOwn D H).count a + X.count a)
    (hr : Exact D H₁ Y r) : Exact D H X r := by
  have := Exact.prefix (tr := []) hle (fun a ha => by simpa [freedIds] using hI a ha) hr
  cases r <;> simpa [EvalRes.withTrace] using this

/-- **§6.2's search, as an exact ledger** (helper). -/
theorem Exact.bind {D : Decls} {H : Store} {X : List Nat} {r : EvalRes}
    {k : Store → Val → EvalRes} (hr : Exact D H X r)
    (hk : ∀ H₁ v tr, r = .ok H₁ v tr → StoreCC D H₁ → (Contents.ofVal v).copyClosed D = true →
      Exact D H₁ (v.own D) (k H₁ v)) :
    Exact D H X (r.andThen k) := by
  cases r with
  | ok H₁ v tr =>
      obtain ⟨h1, h2, h3, h4⟩ := hr
      exact Exact.prefix h1 h4 (hk H₁ v tr rfl h2 h3)
  | returned H₁ v tr => exact hr
  | broke H₁ sc tr => exact hr
  | panic k tr => trivial
  | stuck w => trivial
  | outOfFuel => trivial

/-- **A later operand under a held value** (helper): the operand does not
unwind — `pendingSafe` — so the held value `Y` is never abandoned, and the
context receives both. -/
theorem Exact.bindHeld {D : Decls} {H : Store} {Y : List Nat} {r : EvalRes}
    {k : Store → Val → EvalRes} (hr : Exact D H [] r) (hq : r.NoRet ∧ r.NoBrk)
    (hk : ∀ H₁ v tr, r = .ok H₁ v tr → StoreCC D H₁ → (Contents.ofVal v).copyClosed D = true →
      Exact D H₁ (Y ++ v.own D) (k H₁ v)) :
    Exact D H Y (r.andThen k) := by
  cases r with
  | ok H₁ v tr =>
      obtain ⟨h1, h2, h3, h4⟩ := hr
      refine Exact.prefix h1 (fun a ha => ?_) (hk H₁ v tr rfl h2 h3)
      have := h4 a ha
      simp only [List.count_append, List.count_nil] at *
      omega
  | returned H₁ v tr => exact hq.1.elim
  | broke H₁ sc tr => exact hq.2.elim
  | panic k tr => trivial
  | stuck w => trivial
  | outOfFuel => trivial

/-- §6.9's call boundary, as an exact ledger (helper). -/
theorem Exact.absorb {D : Decls} {H : Store} {X : List Nat} {r : EvalRes}
    {k : Store → Val → EvalRes} (hr : Exact D H X r)
    (hk : ∀ H₁ v tr, r = .ok H₁ v tr → StoreCC D H₁ → (Contents.ofVal v).copyClosed D = true →
      Exact D H₁ (v.own D) (k H₁ v)) :
    Exact D H X (r.absorb k) := by
  cases r with
  | ok H₁ v tr =>
      obtain ⟨h1, h2, h3, h4⟩ := hr
      exact Exact.prefix h1 h4 (hk H₁ v tr rfl h2 h3)
  | returned H₁ v tr => exact hr
  | broke H₁ sc tr => trivial
  | panic k tr => trivial
  | stuck w => trivial
  | outOfFuel => trivial

/-- A value produced where the store is, owning exactly what was held
(helper). -/
theorem Exact.pure {D : Decls} {H : Store} {X : List Nat} {v : Val} (hcc : StoreCC D H)
    (hv : (Contents.ofVal v).copyClosed D = true)
    (ho : ∀ a, a < H.length → (v.own D).count a = X.count a) : Exact D H X (.ok H v []) :=
  ⟨Nat.le_refl _, hcc, hv, fun a ha => by have := ho a ha; simp [freedIds]; omega⟩

/-- A scalar produced where the store is, from held operands that were
scalars too (helper). -/
theorem Exact.scalar {D : Decls} {H : Store} {X : List Nat} {v : Val} (hcc : StoreCC D H)
    (hs : v.scalar) (hX : ∀ a, a < H.length → X.count a = 0) : Exact D H X (.ok H v []) :=
  Exact.pure hcc (Val.scalar_own hs).2 (fun a ha => by
    rw [(Val.scalar_own (D := D) hs).1, hX a ha]; rfl)

/-- A scalar operator's outcome (helper). -/
theorem Exact.opRes {D : Decls} {H : Store} {X : List Nat} {o : OpRes} (hcc : StoreCC D H)
    (hs : ∀ v, o = .val v → v.scalar ∧ ∀ a, a < H.length → X.count a = 0) :
    Exact D H X (o.toRes H) := by
  cases o with
  | val v => exact Exact.scalar hcc (hs v rfl).1 (hs v rfl).2
  | trap k => trivial
  | confused => trivial

/-- **Aggregate introduction, exactly** (`introVal`): the new value owns
exactly its members, apart from its own fresh identity (helper). -/
theorem Exact.intro {D : Decls} {H : Store} {Y : List Nat} {mk : Nat → Val} (hcc : StoreCC D H)
    (ho : (Contents.ofVal (mk H.length)).copyClosed D = true →
      ∀ a, a < H.length → ((mk H.length).own D).count a = Y.count a) :
    Exact D H Y (introVal D H mk) := by
  unfold introVal
  split
  · rename_i hv
    refine ⟨by simp, hcc.append StoreCC.dead, hv, fun a ha => ?_⟩
    have := ho hv a ha
    rw [storeOwn_append]
    simp [storeOwn, Cell.own, freedIds]
    omega
  · trivial

/-- The ledger's promise about an argument list (helper). -/
def ArgsExact (D : Decls) (H : Store) : ArgsRes → Prop
  | .ok H' vs tr =>
      H.length ≤ H'.length ∧ StoreCC D H' ∧
      Contents.copyClosedList D (Contents.ofVals vs) = true ∧
      ∀ a, a < H.length → (storeOwn D H').count a + (Contents.ownList D (Contents.ofVals vs)).count a +
          (freedIds D tr).count a = (storeOwn D H).count a
  | .abort r => Exact D H [] r

/-- A result that neither completes nor unwinds keeps every ledger, with any
trace prefixed (helper). -/
theorem Exact.of_quiet {D : Decls} {H : Store} {X : List Nat} {r : EvalRes} {tr : List Event}
    (hq : r.NoRet ∧ r.NoBrk) (hok : ∀ H' v tr', r ≠ .ok H' v tr') : Exact D H X (r.withTrace tr) := by
  cases r with
  | ok H' v tr' => exact absurd rfl (hok H' v tr')
  | returned => exact hq.1.elim
  | broke => exact hq.2.elim
  | _ => trivial

/-- **A quiet argument list keeps the exact ledger** (§6.2's left-to-right
search), and aborts only with a trap, a refusal or exhausted fuel: no member
unwinds, so no built value is ever abandoned (helper). -/
theorem evalArgs_exactQuiet {D : Decls} {ev : Store → Expr → EvalRes} :
    ∀ {es : List Expr},
      (∀ H e, e ∈ es → StoreCC D H → Exact D H [] (ev H e)) →
      (∀ H e, e ∈ es → (ev H e).NoRet ∧ (ev H e).NoBrk) →
      ∀ H, StoreCC D H → ArgsExact D H (evalArgs ev H es) ∧
        ∀ r, evalArgs ev H es = .abort r → r.NoRet ∧ r.NoBrk
  | [], _, _, H, hcc => by
      refine ⟨⟨Nat.le_refl _, hcc, rfl, fun a _ => ?_⟩, fun r h => by simp [evalArgs] at h⟩
      simp [Contents.ofVals, Contents.ownList, freedIds]
  | e :: es, hev, hq, H, hcc => by
      have h₁ := hev H e List.mem_cons_self hcc
      have q₁ := hq H e List.mem_cons_self
      simp only [evalArgs]
      cases hr : ev H e with
      | ok H₁ v tr =>
          rw [hr] at h₁
          obtain ⟨l₁, c₁, v₁, i₁⟩ := h₁
          have ih := evalArgs_exactQuiet (es := es)
            (fun H e' hm => hev H e' (List.mem_cons_of_mem _ hm))
            (fun H e' hm => hq H e' (List.mem_cons_of_mem _ hm)) H₁ c₁
          dsimp only
          cases hra : evalArgs ev H₁ es with
          | ok H₂ vs tr₂ =>
              rw [hra] at ih
              obtain ⟨⟨l₂, c₂, v₂, i₂⟩, _⟩ := ih
              refine ⟨⟨Nat.le_trans l₁ l₂, c₂, ?_, fun a ha => ?_⟩, fun r h => by cases h⟩
              · simp [Contents.ofVals, Contents.copyClosedList, v₁, v₂]
              · have := i₁ a ha; have := i₂ a (by omega)
                simp only [Contents.ownList_ofVals_cons, freedIds_append, List.count_append,
                  List.count_nil] at *
                omega
          | abort r =>
              rw [hra] at ih
              have hq' := ih.2 r rfl
              refine ⟨Exact.of_quiet hq' (evalArgs_abort_ne_ok hra), fun r' h => ?_⟩
              cases h
              exact ⟨EvalRes.withTrace_noRet hq'.1, EvalRes.withTrace_noBrk hq'.2⟩
      | returned H₁ v tr => rw [hr] at q₁; exact q₁.1.elim
      | broke H₁ sc tr => rw [hr] at q₁; exact q₁.2.elim
      | panic k tr => exact ⟨trivial, fun r h => by cases h; exact ⟨trivial, trivial⟩⟩
      | stuck w => exact ⟨trivial, fun r h => by cases h; exact ⟨trivial, trivial⟩⟩
      | outOfFuel => exact ⟨trivial, fun r h => by cases h; exact ⟨trivial, trivial⟩⟩

/-- **An argument list keeps the exact ledger** where only its first member
may unwind: nothing is pending when the first does (helper). -/
theorem evalArgs_exact {D : Decls} {ev : Store → Expr → EvalRes} {es : List Expr}
    (hev : ∀ H e, e ∈ es → StoreCC D H → Exact D H [] (ev H e))
    (hq : ∀ H e, e ∈ es.tail → (ev H e).NoRet ∧ (ev H e).NoBrk) :
    ∀ H, StoreCC D H → ArgsExact D H (evalArgs ev H es) := by
  intro H hcc
  cases es with
  | nil => exact (evalArgs_exactQuiet (es := []) (by simp) (by simp) H hcc).1
  | cons e es =>
      have h₁ := hev H e List.mem_cons_self hcc
      simp only [evalArgs]
      cases hr : ev H e with
      | ok H₁ v tr =>
          rw [hr] at h₁
          obtain ⟨l₁, c₁, v₁, i₁⟩ := h₁
          have ih := evalArgs_exactQuiet (es := es)
            (fun H e' hm => hev H e' (List.mem_cons_of_mem _ hm)) (fun H e' hm => hq H e' hm) H₁ c₁
          dsimp only
          cases hra : evalArgs ev H₁ es with
          | ok H₂ vs tr₂ =>
              rw [hra] at ih
              obtain ⟨⟨l₂, c₂, v₂, i₂⟩, _⟩ := ih
              refine ⟨Nat.le_trans l₁ l₂, c₂, ?_, fun a ha => ?_⟩
              · simp [Contents.ofVals, Contents.copyClosedList, v₁, v₂]
              · have := i₁ a ha; have := i₂ a (by omega)
                simp only [Contents.ownList_ofVals_cons, freedIds_append, List.count_append,
                  List.count_nil] at *
                omega
          | abort r =>
              rw [hra] at ih
              exact Exact.of_quiet (ih.2 r rfl) (evalArgs_abort_ne_ok hra)
      | returned H₁ v tr => rw [hr] at h₁; exact h₁
      | broke H₁ sc tr => rw [hr] at h₁; exact h₁
      | panic k tr => trivial
      | stuck w => trivial
      | outOfFuel => trivial

/-! ## The forms' exact ledgers -/

/-- **(D-Use-Move) §6.3, exactly**: the value handed on owns exactly what the
place owned, and the place holds `⊘` (helper). -/
theorem Exact.move {D : Decls} {H : Store} {ℓ : Nat} {c c' sub : Contents} {π : List Nat}
    {v : Val} (hcc : StoreCC D H) (hc : H[ℓ]? = some (.full c)) (hr : c.readAt π = .ok sub)
    (hw : c.writeAt π .hole = some c') (hv : sub.toVal = some v) :
    Exact D H [] (.ok (H.set ℓ (.full c')) v []) := by
  have hccc := hcc ℓ c hc
  have hc' := Contents.writeAt_copyClosed π hccc rfl hw
  have hsub : Contents.ofVal v = sub := Contents.ofVal_toVal hv
  refine ⟨by simp, hcc.set hc', ?_, fun a _ => ?_⟩
  · rw [hsub]; exact Contents.readAt_copyClosed π hccc hr
  · have h1 := storeOwn_set_count D a (.full c') hc
    have h2 := Contents.writeAt_own_eq a π hccc hc' hr hw
    simp only [Cell.own, Contents.own, List.count_nil] at h1 h2
    simp only [Val.own, hsub, freedIds, List.flatMap_nil, List.count_nil]
    omega

/-- **(D-Use-Declared-Linear) §6.3, exactly**: the leaf handed on, the residue
dropped and the shell consumed are exactly the consumed place, which becomes
`⊘` (helper). -/
theorem Exact.destructure {D : Decls} {H : Store} {ℓ : Nat} {c c' cd leaf : Contents}
    {πd πs : List Nat} {v : Val} {evs : List Event} (hcc : StoreCC D H)
    (hc : H[ℓ]? = some (.full c)) (hr : c.readAt πd = .ok cd)
    (hd : cd.destructure D ℓ πs = .ok (leaf, evs)) (hv : leaf.toVal = some v)
    (hw : c.writeAt πd .hole = some c') :
    Exact D H [] (.ok (H.set ℓ (.full c')) v evs) := by
  have hccc := hcc ℓ c hc
  have hcd := Contents.readAt_copyClosed πd hccc hr
  have hc' := Contents.writeAt_copyClosed πd hccc rfl hw
  have hsub : Contents.ofVal v = leaf := Contents.ofVal_toVal hv
  refine ⟨by simp, hcc.set hc', by rw [hsub]; exact (Contents.destructure_exact hcd hd 0).2,
    fun a _ => ?_⟩
  have h1 := storeOwn_set_count D a (.full c') hc
  have h2 := Contents.writeAt_own_eq a πd hccc hc' hr hw
  have h3 := (Contents.destructure_exact hcd hd a).1
  simp only [Cell.own, Contents.own, List.count_nil] at h1 h2
  simp only [Val.own, hsub, List.count_nil]
  omega

/-- **§6.11's `@drop`, exactly**: the place's residue is freed and the place
becomes `⊘` (helper). -/
theorem Exact.dropPlace {D : Decls} {H : Store} {ℓ : Nat} {c c' sub : Contents} {π : List Nat}
    {evs : List Event} (hcc : StoreCC D H) (hc : H[ℓ]? = some (.full c))
    (hr : c.readAt π = .ok sub) (hd : dropCell D ℓ sub = .ok evs)
    (hw : c.writeAt π .hole = some c') :
    Exact D H [] (.ok (H.set ℓ (.full c')) .unit evs) := by
  have hccc := hcc ℓ c hc
  have hc' := Contents.writeAt_copyClosed π hccc rfl hw
  refine ⟨by simp, hcc.set hc', rfl, fun a _ => ?_⟩
  have h1 := storeOwn_set_count D a (.full c') hc
  have h2 := Contents.writeAt_own_eq a π hccc hc' hr hw
  rw [dropCell_freed hd]
  simp only [Cell.own, Contents.own, List.count_nil] at h1 h2
  simp only [Val.own, Contents.ofVal, Contents.own, List.count_nil]
  omega

/-- **§6.11's `@drop` at a declared plan, exactly** (helper). -/
theorem Exact.dropDeclared {D : Decls} {H : Store} {ℓ : Nat} {c c' cd leaf : Contents}
    {πd πs : List Nat} {evs levs : List Event} (hcc : StoreCC D H)
    (hc : H[ℓ]? = some (.full c)) (hr : c.readAt πd = .ok cd)
    (hd : cd.destructure D ℓ πs = .ok (leaf, evs)) (hl : dropCell D ℓ leaf = .ok levs)
    (hw : c.writeAt πd .hole = some c') :
    Exact D H [] (.ok (H.set ℓ (.full c')) .unit (evs ++ levs)) := by
  have hccc := hcc ℓ c hc
  have hcd := Contents.readAt_copyClosed πd hccc hr
  have hc' := Contents.writeAt_copyClosed πd hccc rfl hw
  refine ⟨by simp, hcc.set hc', rfl, fun a _ => ?_⟩
  have h1 := storeOwn_set_count D a (.full c') hc
  have h2 := Contents.writeAt_own_eq a πd hccc hc' hr hw
  have h3 := (Contents.destructure_exact hcd hd a).1
  rw [freedIds_append, List.count_append, dropCell_freed hl]
  simp only [Cell.own, Contents.own, List.count_nil] at h1 h2
  simp only [Val.own, Contents.ofVal, Contents.own, List.count_nil]
  omega

/-- **(D-Assign) §6.8, exactly**: the old contents at the place is freed and
the held value takes its position (helper). -/
theorem Exact.assign {D : Decls} {H : Store} {ℓ : Nat} {c c' old : Contents} {π : List Nat}
    {v : Val} {evs : List Event} (hcc : StoreCC D H) (hc : H[ℓ]? = some (.full c))
    (hr : c.readAt π = .ok old) (hd : dropCell D ℓ old = .ok evs)
    (hw : c.writeAt π (Contents.ofVal v) = some c') (hc' : c'.copyClosed D = true) :
    Exact D H (v.own D) (.ok (H.set ℓ (.full c')) .unit evs) := by
  have hccc := hcc ℓ c hc
  refine ⟨by simp, hcc.set hc', rfl, fun a _ => ?_⟩
  have h1 := storeOwn_set_count D a (.full c') hc
  have h2 := Contents.writeAt_own_eq a π hccc hc' hr hw
  rw [dropCell_freed hd]
  simp only [Cell.own] at h1
  simp only [Val.own_unit, List.count_nil]
  simp only [Val.own] at *
  omega

/-- **(D-Assign) below a dynamic index, exactly** (helper). -/
theorem Exact.assignDyn {D : Decls} {H : Store} {ℓ : Nat} {c c' sub sub' old : Contents}
    {π ρ : List Nat} {v : Val} {evs : List Event} (hcc : StoreCC D H)
    (hc : H[ℓ]? = some (.full c)) (hr : c.readAt π = .ok sub) (hr' : sub.readAt ρ = .ok old)
    (hd : dropCell D ℓ old = .ok evs) (hw' : sub.writeAt ρ (Contents.ofVal v) = some sub')
    (hw : c.writeAt π sub' = some c') (hc' : c'.copyClosed D = true) :
    Exact D H (v.own D) (.ok (H.set ℓ (.full c')) .unit evs) := by
  have hccc := hcc ℓ c hc
  have hsub := Contents.readAt_copyClosed π hccc hr
  have hsub' : sub'.copyClosed D = true :=
    Contents.readAt_copyClosed π hc' (Contents.readAt_writeAt π hw)
  refine ⟨by simp, hcc.set hc', rfl, fun a _ => ?_⟩
  have h1 := storeOwn_set_count D a (.full c') hc
  have h2 := Contents.writeAt_own_eq a π hccc hc' hr hw
  have h2' := Contents.writeAt_own_eq a ρ hsub hsub' hr' hw'
  rw [dropCell_freed hd]
  simp only [Cell.own] at h1
  simp only [Val.own_unit, List.count_nil]
  simp only [Val.own] at *
  omega

/-- A scope teardown after a value (`endscope` §6.7, the frame pop §6.9),
exactly (helper). -/
theorem Exact.unwind {D : Decls} {H : Store} {v : Val} {ls : List Nat} (hcc : StoreCC D H)
    (hv : (Contents.ofVal v).copyClosed D = true) :
    Exact D H (v.own D)
      (match unwindLocs D H ls with
       | .error w => .stuck w
       | .ok (H', evs) => .ok H' v evs) := by
  cases hu : unwindLocs D H ls with
  | error w => trivial
  | ok r =>
      obtain ⟨H', evs⟩ := r
      obtain ⟨i, l, c⟩ := unwindLocs_exact hcc hu
      exact ⟨by omega, c, hv, fun a _ => by have := i a; omega⟩

/-- §6.4's operators produce a value only from scalar operands (helper). -/
theorem evalBinOp_val_args {M : FloatOps} {op : BinOp} {a b v : Val}
    (h : evalBinOp M op a b = .val v) : a.scalar ∧ b.scalar := by
  unfold evalBinOp at h
  split at h
  · exact ⟨trivial, trivial⟩
  · exact ⟨trivial, trivial⟩
  · cases h

/-- The same for a unary operator (helper). -/
theorem evalUnOp_val_arg {op : UnOp} {a v : Val} (h : evalUnOp op a = .val v) : a.scalar := by
  unfold evalUnOp at h
  split at h <;> first | trivial | cases h

/-- The same for `@intCast` (helper). -/
theorem evalIntCast_val_arg {w : IntWidth} {sg : Sign} {a v : Val}
    (h : evalIntCast w sg a = .val v) : a.scalar := by
  unfold evalIntCast at h
  split at h <;> first | trivial | cases h

/-- The same for the float intrinsics (helper). -/
theorem evalFintrin_val_arg {M : FloatOps} {k : FloatIntrin} {a v : Val}
    (h : evalFintrin M k a = .val v) : a.scalar := by
  unfold evalFintrin at h
  split at h <;> first | trivial | cases h

/-- An observable value is a scalar (helper). -/
theorem Val.observable_scalar {v : Val} (h : v.observable = true) : v.scalar := by
  cases v <;> simp_all [Val.observable, Val.scalar]

/-- A dynamic read's value is `Copy`: the machine refuses any other (§6.3's
(D-Use-Untrackable-Dynamic-Copy), RUE-2400) (helper). -/
theorem eval_indexRead_copy {M : FloatOps} {P : Program} {n : Nat} {H H' : Store} {φ : Frame}
    {p : Place} {idx : List Expr} {πs : List (List Nat)} {v : Val} {tr : List Event}
    (h : eval M n P H φ (.indexRead p idx πs) = .ok H' v tr) : v.mult P.decls = .copy := by
  cases n with
  | zero => simp [eval] at h
  | succ n =>
      simp only [eval] at h
      split at h
      · rename_i r hra
        exact absurd h (evalArgs_abort_ne_ok hra _ _ _)
      · rename_i H₁ vs tr₀ hra
        cases hdp : dynPlace H₁ φ p vs πs with
        | stuck w => simp [hdp, EvalRes.withTrace] at h
        | bounds => simp [hdp, EvalRes.withTrace] at h
        | «at» ℓ c sub ρ =>
            simp only [hdp] at h
            split at h
            · simp [EvalRes.withTrace] at h
            · split at h
              · simp [EvalRes.withTrace] at h
              · split at h
                · rename_i hm
                  simp only [EvalRes.withTrace, EvalRes.ok.injEq] at h
                  obtain ⟨_, rfl, _⟩ := h
                  exact hm
                · simp [EvalRes.withTrace] at h

/-! ## The law, over the whole machine -/

/-- **The exact conservation law** (§7's no-leak-of-drops, the invariant
half): every evaluation, of every `pendingSafe` expression of a
`pendingSafe` program, from every copy-closed store, at every fuel, keeps
`Exact` — every owned identity it starts with is, at its end, in exactly one
of the store, the result, or the trace's ended identities. By fuel induction
over `eval`, one case per form, each closed by its exact ledger above; like
`eval_conserves` it reads no typing derivation. -/
theorem eval_exact (M : FloatOps) {P : Program} (hp : P.pendingSafe = true) :
    ∀ (fuel : Nat) (H : Store) (φ : Frame) (e : Expr), StoreCC P.decls H →
      e.pendingSafe = true → Exact P.decls H [] (eval M fuel P H φ e) := by
  have hbody : ∀ (f : Nat) (fd : FnDef), P.fns[f]? = some fd → fd.body.pendingSafe = true :=
    fun f fd h =>
    (List.all_eq_true.mp hp) fd (List.mem_of_getElem? h)
  intro fuel
  induction fuel with
  | zero => intro H φ e _ _; simp only [eval]; trivial
  | succ n ih =>
    intro H φ e hcc he
    have hq : ∀ H' e', e'.returns = false ∧ e'.breaks = false →
        (eval M n P H' φ e').NoRet ∧ (eval M n P H' φ e').NoBrk := fun H' e' h =>
      ⟨(eval_quiet M P n H' φ e').1 h.1, (eval_quiet M P n H' φ e').2 h.2⟩
    have hargs := fun (es : List Expr) (hps : Expr.pendingSafeList es = true)
        (hql : Expr.quietList es.tail = true) (H' : Store) (hc : StoreCC P.decls H') =>
      evalArgs_exact (ev := fun H'' e' => eval M n P H'' φ e') (es := es)
        (fun H'' e' hm hc' => ih H'' φ e' hc' (Expr.pendingSafeList_mem hps hm))
        (fun H'' e' hm => hq H'' e' (Expr.quietList_mem hql hm)) H' hc
    cases e with
    | intLit w sg m => exact Exact.scalar hcc trivial (fun _ _ => rfl)
    | floatLit w l => exact Exact.scalar hcc trivial (fun _ _ => rfl)
    | boolLit b => exact Exact.scalar hcc trivial (fun _ _ => rfl)
    | unitLit => exact Exact.scalar hcc trivial (fun _ _ => rfl)
    | panic msg => simp only [eval]; trivial
    | brk => simp only [eval]; exact ⟨Nat.le_refl _, hcc, fun a _ => by simp [freedIds]⟩
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
                      exact Exact.destructure hcc hc hr hd hv hw
            · split
              · trivial
              · rename_i sub hr
                split
                · trivial
                · rename_i v hv
                  split
                  · rename_i hm
                    refine Exact.pure hcc ?_ (fun a _ => by simp [Val.own_of_copy hm])
                    rw [Contents.ofVal_toVal hv]; exact Contents.readAt_copyClosed _ (hcc ℓ c hc) hr
                  · split
                    · trivial
                    · rename_i c' hw
                      exact Exact.move hcc hc hr hw hv
    | binop op e₁ e₂ =>
        simp only [eval]
        simp only [Expr.pendingSafe, Bool.and_eq_true, Bool.not_eq_eq_eq_not, Bool.not_true,
          Expr.unwinds, Bool.or_eq_false_iff] at he
        refine Exact.bind (ih H φ e₁ hcc he.1.1) (fun H₁ v₁ _ _ hc₁ _ => ?_)
        refine Exact.bindHeld (ih H₁ φ e₂ hc₁ he.1.2) (hq H₁ e₂ he.2) (fun H₂ v₂ _ _ hc₂ _ => ?_)
        refine Exact.opRes hc₂ (fun v h => ⟨evalBinOp_scalar h, fun a _ => ?_⟩)
        obtain ⟨s₁, s₂⟩ := evalBinOp_val_args h
        simp [(Val.scalar_own (D := P.decls) s₁).1, (Val.scalar_own (D := P.decls) s₂).1]
    | unop op e₁ =>
        simp only [eval]
        simp only [Expr.pendingSafe] at he
        refine Exact.bind (ih H φ e₁ hcc he) (fun H₁ v₁ _ _ hc₁ _ => ?_)
        refine Exact.opRes hc₁ (fun v h => ⟨evalUnOp_scalar h, fun a _ => ?_⟩)
        simp [(Val.scalar_own (D := P.decls) (evalUnOp_val_arg h)).1]
    | intCast w sg e₁ =>
        simp only [eval]
        simp only [Expr.pendingSafe] at he
        refine Exact.bind (ih H φ e₁ hcc he) (fun H₁ v₁ _ _ hc₁ _ => ?_)
        refine Exact.opRes hc₁ (fun v h => ⟨evalIntCast_scalar h, fun a _ => ?_⟩)
        simp [(Val.scalar_own (D := P.decls) (evalIntCast_val_arg h)).1]
    | fintrin k e₁ =>
        simp only [eval]
        simp only [Expr.pendingSafe] at he
        refine Exact.bind (ih H φ e₁ hcc he) (fun H₁ v₁ _ _ hc₁ _ => ?_)
        refine Exact.opRes hc₁ (fun v h => ⟨evalFintrin_scalar h, fun a _ => ?_⟩)
        simp [(Val.scalar_own (D := P.decls) (evalFintrin_val_arg h)).1]
    | dbg e₁ =>
        simp only [eval]
        simp only [Expr.pendingSafe] at he
        refine Exact.bind (ih H φ e₁ hcc he) (fun H₁ v₁ _ _ hc₁ _ => ?_)
        split
        · rename_i hobs
          refine ⟨Nat.le_refl _, hc₁, rfl, fun a _ => ?_⟩
          simp [freedIds, Event.freed, (Val.scalar_own (D := P.decls) (Val.observable_scalar hobs)).1]
        · trivial
    | mkStruct s args =>
        simp only [eval]
        simp only [Expr.pendingSafe, Bool.and_eq_true] at he
        have ka := hargs args he.1 he.2 H hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          obtain ⟨l₁, c₁, _, i₁⟩ := ka
          refine Exact.prefix (Y := Contents.ownList P.decls (Contents.ofVals vs)) l₁
            (fun a ha => by have := i₁ a ha; simp only [List.count_nil] at *; omega) ?_
          split
          · trivial
          · split
            · exact Exact.intro c₁ (fun hv a ha => Contents.own_struct_fresh hv (by omega))
            · trivial
    | mkEnum e k args =>
        simp only [eval]
        simp only [Expr.pendingSafe, Bool.and_eq_true] at he
        have ka := hargs args he.1 he.2 H hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          obtain ⟨l₁, c₁, _, i₁⟩ := ka
          refine Exact.prefix (Y := Contents.ownList P.decls (Contents.ofVals vs)) l₁
            (fun a ha => by have := i₁ a ha; simp only [List.count_nil] at *; omega) ?_
          split
          · trivial
          · split
            · trivial
            · split
              · exact Exact.intro c₁ (fun hv a ha => Contents.own_enum_fresh hv (by omega))
              · trivial
    | mkArray T args =>
        simp only [eval]
        simp only [Expr.pendingSafe, Bool.and_eq_true] at he
        have ka := hargs args he.1 he.2 H hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          obtain ⟨l₁, c₁, _, i₁⟩ := ka
          refine Exact.prefix (Y := Contents.ownList P.decls (Contents.ofVals vs)) l₁
            (fun a ha => by have := i₁ a ha; simp only [List.count_nil] at *; omega) ?_
          exact Exact.intro c₁ (fun hv a ha => Contents.own_array_fresh hv (by omega))
    | «match» scrut arms =>
        simp only [eval]
        simp only [Expr.pendingSafe, Bool.and_eq_true] at he
        refine Exact.bind (ih H φ scrut hcc he.1) (fun H₀ v _ _ hc₀ hv => ?_)
        cases v with
        | enum e k i vs =>
          dsimp only
          split
          · trivial
          · rename_i body harm
            have hpay := Contents.enum_payload hv
            refine Exact.prefix (H₁ := (mintParams H₀ vs).1) (Y := []) ?_ ?_ ?_
            · rw [mintParams_length]; omega
            · intro a _
              have := matchConsume_exact hv a
              rw [storeOwn_mintParams]
              simp only [List.count_append, List.count_nil]
              show _ = _ + ((Contents.enum e k i (Contents.ofVals vs)).own P.decls).count a
              omega
            · exact Exact.bind (ih _ _ body (hc₀.mintParams (hpay 0).2)
                  (Expr.pendingSafeList_mem he.2 (List.mem_of_getElem? harm)))
                (fun H₂ v₂ _ _ hc₂ hv₂ => Exact.unwind hc₂ hv₂)
        | _ => trivial
    | repeatArray T e₁ m =>
        simp only [eval]
        simp only [Expr.pendingSafe] at he
        refine Exact.bind (ih H φ e₁ hcc he) (fun H₁ v₁ _ _ hc₁ _ => ?_)
        split
        · rename_i hm
          refine Exact.intro hc₁ (fun hv a ha => ?_)
          have := Contents.own_array_fresh hv (a := a) (by omega)
          rw [Contents.ownList_replicate hm] at this
          show ((Contents.array T H₁.length (Contents.ofVals (List.replicate m v₁))).own
            P.decls).count a = _
          rw [this, Val.own_of_copy hm]
        · trivial
    | indexRead p idx πs =>
        simp only [eval]
        simp only [Expr.pendingSafe, Bool.and_eq_true] at he
        have ka := hargs idx he.1 he.2 H hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          obtain ⟨l₁, c₁, _, i₁⟩ := ka
          refine Exact.prefix (Y := Contents.ownList P.decls (Contents.ofVals vs)) l₁
            (fun a ha => by have := i₁ a ha; simp only [List.count_nil] at *; omega) ?_
          split
          · trivial
          · trivial
          · rename_i ℓ c sub ρ hdp
            obtain ⟨hc, hr⟩ := dynPlace_at hdp
            obtain ⟨is, his⟩ := dynPlace_ints (fun w h => by rw [hdp] at h; cases h)
            split
            · trivial
            · rename_i leaf hr'
              split
              · trivial
              · rename_i v hv
                split
                · rename_i hm
                  refine Exact.pure c₁ ?_ (fun a _ => by
                    simp [Val.own_of_copy hm, Val.ints_own (D := P.decls) his])
                  rw [Contents.ofVal_toVal hv]
                  exact Contents.readAt_copyClosed ρ
                    (Contents.readAt_copyClosed _ (c₁ ℓ c hc) hr) hr'
                · trivial
    | indexDrop p idx πs =>
        simp only [eval]
        have hps : (Expr.indexRead p idx πs).pendingSafe = true := by
          simpa [Expr.pendingSafe] using he
        refine Exact.bind (ih H φ _ hcc hps) (fun H₁ v₁ tr hr hc₁ _ => ?_)
        exact Exact.pure hc₁ rfl (fun a _ => by simp [Val.own_of_copy (eval_indexRead_copy hr)])
    | indexWrite p idx πs e₁ =>
        simp only [eval]
        simp only [Expr.pendingSafe, Bool.and_eq_true] at he
        refine Exact.bind (ih H φ e₁ hcc he.1.1) (fun H₁ v _ _ hc₁ hv => ?_)
        have ka := evalArgs_exactQuiet (ev := fun H'' e' => eval M n P H'' φ e') (es := idx)
          (fun H'' e' hm hc' => ih H'' φ e' hc' (Expr.pendingSafeList_mem he.1.2 hm))
          (fun H'' e' hm => hq H'' e' (Expr.quietList_mem he.2 hm)) H₁ hc₁
        split
        · rename_i r hra
          rw [hra] at ka
          have hqr := ka.2 r rfl
          have hno := evalArgs_abort_ne_ok hra
          cases r with
          | ok => exact absurd rfl (hno _ _ _)
          | returned => exact hqr.1.elim
          | broke => exact hqr.2.elim
          | _ => trivial
        · rename_i H₂ vs tr hra
          rw [hra] at ka
          obtain ⟨⟨l₂, c₂, _, i₂⟩, _⟩ := ka
          refine Exact.prefix
            (Y := v.own P.decls ++ Contents.ownList P.decls (Contents.ofVals vs)) l₂
            (fun a ha => by have := i₂ a ha; simp only [List.count_append] at *; omega) ?_
          split
          · trivial
          · trivial
          · rename_i ℓ c sub ρ hdp
            obtain ⟨hc, hr⟩ := dynPlace_at hdp
            obtain ⟨is, his⟩ := dynPlace_ints (fun w h => by rw [hdp] at h; cases h)
            rw [Val.ints_own his, List.append_nil]
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
                        exact Exact.assignDyn c₂ hc hr hr' hd hw' hw hc'
                      · trivial
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
                        exact Exact.dropDeclared hcc hc hr hd hl hw
            · split
              · trivial
              · rename_i sub hr
                split
                · trivial
                · split
                  · trivial
                  · rename_i evs hd
                    split
                    · exact Exact.scalar hcc trivial (fun _ _ => rfl)
                    · split
                      · trivial
                      · rename_i c' hw
                        exact Exact.dropPlace hcc hc hr hd hw
    | letIn m e₁ e₂ =>
        simp only [eval]
        simp only [Expr.pendingSafe, Bool.and_eq_true] at he
        refine Exact.bind (ih H φ e₁ hcc he.1) (fun H₁ v₁ _ _ hc₁ hv₁ => ?_)
        refine Exact.shift (H₁ := H₁ ++ [.full (Contents.ofVal v₁)]) (Y := []) (by simp)
          (fun a _ => ?_) ?_
        · rw [storeOwn_append]
          simp [storeOwn, Cell.own, List.count_append]
        · refine Exact.bind (ih _ _ e₂ (hc₁.append (StoreCC.single hv₁)) he.2)
            (fun H₂ v₂ _ _ hc₂ hv₂ => ?_)
          split
          · trivial
          · rename_i H₃ evs hdr
            obtain ⟨i, l, c⟩ := dropRetire_exact hc₂ hdr
            exact ⟨by omega, c, hv₂, fun a _ => by have := i a; omega⟩
    | assign p e₁ =>
        simp only [eval]
        simp only [Expr.pendingSafe] at he
        refine Exact.bind (ih H φ e₁ hcc he) (fun H₁ v _ _ hc₁ hv => ?_)
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
                      exact Exact.assign hc₁ hc hr hd hw hc'
                    · trivial
    | seq e₁ e₂ =>
        simp only [eval]
        simp only [Expr.pendingSafe, Bool.and_eq_true] at he
        refine Exact.bind (ih H φ e₁ hcc he.1) (fun H₁ v₁ _ _ hc₁ hv₁ => ?_)
        split
        · trivial
        · split
          · trivial
          · rename_i evs hd
            refine Exact.prefix (Y := []) (Nat.le_refl _) (fun a _ => ?_) (ih H₁ φ e₂ hc₁ he.2)
            have := dropContents_freed hd
            simp [freedIds, Event.freed, this]
        · rename_i hm
          exact Exact.shift (Nat.le_refl _) (fun a _ => by simp [Val.own_of_copy hm])
            (ih H₁ φ e₂ hc₁ he.2)
    | ite c e₁ e₂ =>
        simp only [eval]
        simp only [Expr.pendingSafe, Bool.and_eq_true] at he
        refine Exact.bind (ih H φ c hcc he.1.1) (fun H₀ v₀ _ _ hc₀ _ => ?_)
        split
        · split
          · exact Exact.shift (Nat.le_refl _) (fun a _ => by simp [Val.own, Contents.ofVal, Contents.own])
              (ih H₀ φ e₁ hc₀ he.1.2)
          · exact Exact.shift (Nat.le_refl _) (fun a _ => by simp [Val.own, Contents.ofVal, Contents.own])
              (ih H₀ φ e₂ hc₀ he.2)
        · trivial
    | call f args =>
        simp only [eval]
        simp only [Expr.pendingSafe, Bool.and_eq_true] at he
        have ka := hargs args he.1 he.2 H hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          obtain ⟨l₁, c₁, cv₁, i₁⟩ := ka
          refine Exact.prefix (Y := Contents.ownList P.decls (Contents.ofVals vs)) l₁
            (fun a ha => by have := i₁ a ha; simp only [List.count_nil] at *; omega) ?_
          split
          · trivial
          · rename_i fd hfd
            split
            · refine Exact.shift (H₁ := (mintParams H₁ vs).1) (Y := []) ?_ ?_ ?_
              · rw [mintParams_length]; omega
              · intro a _
                rw [storeOwn_mintParams]
                simp only [List.count_append, List.count_nil]
                omega
              · refine Exact.absorb (ih _ _ fd.body (c₁.mintParams cv₁) (hbody f fd hfd))
                  (fun H₃ v _ _ hc₃ hv₃ => ?_)
                simp only [runAllScopeDrops]
                exact Exact.unwind hc₃ hv₃
            · trivial
    | ret e₁ =>
        simp only [eval]
        simp only [Expr.pendingSafe] at he
        refine Exact.bind (ih H φ e₁ hcc he) (fun H₁ v _ _ hc₁ hv => ?_)
        simp only [runAllScopeDrops]
        split
        · trivial
        · rename_i H₂ evs hu
          obtain ⟨i, l, c⟩ := unwindLocs_exact hc₁ hu
          exact ⟨by omega, c, hv, fun a _ => by have := i a; omega⟩
    | loop e₁ =>
        simp only [eval]
        have hbe : e₁.pendingSafe = true := by simpa [Expr.pendingSafe] using he
        have hb := ih H φ e₁ hcc hbe
        split
        · rename_i H₁ tr hr
          rw [hr] at hb
          obtain ⟨l, c, _, i⟩ := hb
          exact Exact.prefix (Y := []) l (fun a ha => by have := i a ha; simp at *; omega)
            (ih H₁ φ (.loop e₁) c he)
        · trivial
        · rename_i H₁ sc tr hr
          rw [hr] at hb
          obtain ⟨l, c, i⟩ := hb
          split
          · trivial
          · rename_i H₂ evs hu
            obtain ⟨i', l', c'⟩ := unwindLocs_exact c hu
            refine ⟨by omega, c', rfl, fun a ha => ?_⟩
            have := i a ha; have := i' a
            simp only [freedIds_append, List.count_append, Val.own_unit, List.count_nil] at *
            omega
        · exact hb

/-! ## §7: every owned value ends exactly once -/

/-- **No leak of drops: every owned value ends exactly once** (§7's
"no use-after-drop / no leak of drops" bullet, the "exactly once" half;
§6.7, §6.9, §6.10, §6.11). Take any well-typed configuration of a checked
program — an expression typed in `Γ`, run in a frame and store that agree
with `Γ` — whose program and expression are `pendingSafe`. Its evaluation is
never refused, and every owned identity the store holds when it starts is,
when it finishes normally or unwinds by `return` or `break`, in exactly one
place: still in the store, part of the result, or ended in the trace exactly
as many times as it was held — by a drop, a discarded temporary, a residue
drop, or a consumption (`Exact`). So a binding's value is dropped at its
scope's end on the normal path (`endscope`, §6.7) or by the σ-walk of an
unwind (§6.9, §6.10) — never both, since the two share one trace and one
count, and never neither.

The carve-outs, stated where they apply:

* **`@panic`** (§6.12): a trap carries no store and runs no drop, so `Exact`
  promises nothing about a `panic` result — the values live at the trap are
  abandoned, which is §5.7's `⊥_panic` edge;
* **RUE-2316**: a value computed for an earlier operand that a later operand
  abandons by `return` or `break` is dropped by nothing, in the calculus as in
  the compiler; `pendingSafe` excludes the shape, and
  `pendingSafe_needed` shows a checked program on which the conclusion fails
  without it.

Every sub-evaluation of a checked run is such a configuration, so the theorem
speaks about every moment of every checked, `pendingSafe` run. -/
theorem drop_exactly_once (M : FloatModel) {P : Program} (h : ProgramTyped P)
    (hp : P.pendingSafe = true) {fuel : Nat} {R : Ty} {Γ : Ctx} {e : Expr} {T : Ty} {Ω : Out}
    {φ : Frame} {H : Store} (ht : Typed P R Γ e T Ω) (hfm : FrameMatches P.decls Γ φ H)
    (hcc : StoreCC P.decls H) (he : e.pendingSafe = true) :
    (∀ w, eval M.toFloatOps fuel P H φ e ≠ .stuck w) ∧
      Exact P.decls H [] (eval M.toFloatOps fuel P H φ e) := by
  refine ⟨fun w hw => ?_, eval_exact M.toFloatOps hp fuel H φ e hcc he⟩
  have := soundness M h.wf fuel ht hfm
  rw [hw] at this
  exact this

/-! ## The RUE-2316 carve-out, witnessed

`S0` declares a destructor. `g` takes an `S0` by value and passes it to `f`
as the first argument while the second argument returns:

```rue
fn main() -> i64 { g(S0 { x0: 7 }) }
fn g(x: S0) -> i64 { f(x, return 0) }
fn f(a: S0, b: i64) -> i64 { @drop(b); b }
```

The checker accepts it. At `g`'s body — a typed configuration whose store
holds `x`'s `S0` — the argument list moves `x` into the pending first slot,
the second argument's `return` unwinds `g`'s frame, whose scope record still
names `x`'s cell but finds it `⊘`, and the moved `S0` is dropped by nothing:
it is in neither the store, the result nor the trace. `drop_exactly_once`'s
conclusion fails there, which is why it carries `pendingSafe`. -/

/-- `S0`, affine, with a destructor (helper). -/
def lostDecls : Decls :=
  Decls.ofStructs [{ attr := .none, fields := [.int .w64 .signed], dtor := true, cls := .affine }]

/-- The program above (helper). -/
def lostProgram : Program :=
  { decls := lostDecls,
    fns := [{ params := [], ret := .int .w64 .signed,
              body := .call 1 [.mkStruct 0 [.intLit .w64 .signed 7]] },
            { params := [⟨.struct 0, false⟩], ret := .int .w64 .signed,
              body := .call 2 [.use (.var 0), .ret (.intLit .w64 .signed 0)] },
            { params := [⟨.struct 0, false⟩, ⟨.int .w64 .signed, false⟩],
              ret := .int .w64 .signed,
              body := .seq (.drop (.var 1)) (.use (.var 0)) }] }

/-- `g`'s body (helper). -/
def lostBody : Expr := .call 2 [.use (.var 0), .ret (.intLit .w64 .signed 0)]

/-- `g`'s entry context: `x`, owned (helper). -/
def lostCtx : Ctx := [{ ty := .struct 0, mu := false, st := .owned }]

/-- `g`'s entry store: the identity slot `S0` was minted at, and `x`'s cell
(helper). -/
def lostStore : Store := [.dead, .full (.struct 0 0 [.int .w64 .signed 7])]

/-- `g`'s entry frame (helper). -/
def lostFrame : Frame := { env := [1], scope := [1] }

/-- The checker accepts the program (helper). -/
theorem lostProgram_typed : ProgramTyped lostProgram := checkProgram_sound (by rfl)

/-- **The RUE-2316 carve-out is load-bearing**: at a typed configuration of a
checked program that is not `pendingSafe`, the exact ledger fails — `x`'s
`S0` (identity `0`) is held once when `g`'s body starts and is nowhere when
its `return` has unwound: not in the store, not in the result, not in the
trace. -/
theorem pendingSafe_needed (M : FloatOps) :
    Typed lostProgram (.int .w64 .signed) lostCtx lostBody (.int .w64 .signed) ⟨none, []⟩ ∧
      FrameMatches lostProgram.decls lostCtx lostFrame lostStore ∧
      StoreCC lostProgram.decls lostStore ∧
      lostBody.pendingSafe = false ∧
      ¬ Exact lostProgram.decls lostStore [] (eval M 100 lostProgram lostStore lostFrame lostBody) := by
  refine ⟨check_sound _ (by rfl) _ (by rfl), ⟨.cons (by rfl) ⟨_, rfl, ?_⟩ (by simp) .nil, rfl⟩,
    ?_, by rfl, ?_⟩
  · exact ContentsMatches.ofVal (v := .struct 0 0 [.int .w64 .signed 7])
      (HasTy.struct (by rfl) (.cons (.int (w := .w64) (s := .signed) (n := 7) (by decide)) .nil))
  · intro ℓ c hc
    match ℓ, hc with
    | 0, hc => simp [lostStore] at hc
    | 1, hc => simp [lostStore] at hc; subst hc; rfl
    | _ + 2, hc => simp [lostStore] at hc
  · have hev : eval M 100 lostProgram lostStore lostFrame lostBody
        = .returned [.dead, .dead] (.int .w64 .signed 0) [] := by rfl
    rw [hev]
    intro ⟨_, _, _, h4⟩
    have := h4 0 (by decide)
    simp [storeOwn, lostStore, Cell.own, Contents.own, Contents.ownList, lostProgram, lostDecls,
      Decls.classOf, Decls.ofStructs, freedIds, Val.own, Contents.ofVal] at this

end RueCore
