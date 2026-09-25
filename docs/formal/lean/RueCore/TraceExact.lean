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

`freedIds` (`Trace/Defs.lean`) reads the trace's markers, and since RUE-2427 every
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

## Two statements, and what "still in the store" means

A run starts from the empty store, so every identity it mentions is minted
during the run, and nothing in the run's result says which store indices were
minted for owned values. The theorems therefore work at two grains, which
together cover every owned value a checked run ever holds, from the moment an
evaluation or a form holds it to the end of that evaluation or form:

* `drop_exactly_once`, at every well-typed **evaluation**: the identities it
  **starts** with (`a < H.length`, the cells');
* `rest_exactly_once`, at the **rest of every form** once its leading
  operands have produced their values: those values too, which is where a
  value minted inside an evaluation — `let x = S { .. }`, `S { .. };`,
  `g(S { .. })` — is ended (`Lead`, `rest_step`); a `loop`'s lead is its body
  breaking, so a `break`'s unwind of body-minted bindings is covered too.

What the two do not see is an end emitted *early*, inside the evaluation
that minted the value: no window holds the value yet, so only
`no_double_free`'s "at most once" bounds such an end, and `main`'s own result
is part of `run`'s result, handed to no form.

"Still in the store" is not a hiding place: `Tidy` (`eval_tidy`) says every
cell an evaluation allocates is retired by its end — §6.9's frame pop, §6.7's
`endscope`, §6.10's unwind — so a value counted as "in the store" is in a cell
the enclosing code can still reach (`orphan_rejected`).

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
  `break`. `pendingSafe_needed` below shows the hypothesis is load-bearing
  on a program the checker accepts.

The proof is the conservation law of `Trace.lean` read as an **equality**
(`eval_exact`), by the same fuel induction. It needs no typing derivation:
copy closure, which the machine maintains, and the two shape premises
RUE-2427 adds — `@dbg`'s operand is observable and a loop body's value is
`⟨⟩` — are what close every case where a value could otherwise vanish
unrecorded. Typing enters only through `soundness`: a checked configuration's
evaluation is never refused.

The definitions the statements here are written in (`Exact`, `Lead`,
`Tidy`, `Settled`, `Program.pendingSafe`) are in `Trace/Defs.lean`, the
definitions layer (README, "Layers").
-/

namespace RueCore

/-! ## The syntactic carve-out -/

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

/-! ## The rest of a form: values minted during an evaluation

`Exact` counts the identities an evaluation *starts* with. A value an operand
creates — `S { .. }` bound by a `let`, discarded by `;`, passed to a call —
is minted inside the enclosing form's evaluation, so no evaluation *starts*
holding it, and yet the form's own rest ends it. The proof checks that rest
at every form (`Exact.bind`'s continuation), and `Lead` and `rest_step` state
it: once a form's leading operand — or its argument list — has produced its
values, whatever the rest of the form yields keeps the exact ledger with
those values held. A `loop`'s lead is its body breaking, so a `break`'s
unwind of the bindings the body still held is a rest too. With `eval_exact`
this puts every place the machine ends a value inside a window that already
counts it (`rest_exactly_once`). -/

/-- Prefixing a trace is injective (helper). -/
theorem EvalRes.withTrace_inj {a b : EvalRes} {tr : List Event}
    (h : a.withTrace tr = b.withTrace tr) : a = b := by
  cases a <;> cases b <;> simp_all [EvalRes.withTrace]

/-- One value's list image owns what the value owns (helper). -/
theorem Contents.ownList_ofVals_single (D : Decls) (v : Val) :
    Contents.ownList D (Contents.ofVals [v]) = v.own D := by
  simp [Contents.ofVals, Contents.ownList]

/-- **The rest of every form keeps the exact ledger** (the continuation half
of `eval_exact`'s induction step): given the law at fuel `n`, once a form's
leading operands produced `vs`, whatever the rest of the form yields at
`n + 1` keeps `Exact` from `H₁` with `vs` held (helper). -/
theorem rest_step (M : FloatOps) {P : Program} (hp : P.pendingSafe = true) {n : Nat}
    (ih : ∀ (H : Store) (φ : Frame) (e : Expr), StoreCC P.decls H → e.pendingSafe = true →
      Exact P.decls H [] (eval M n P H φ e)) :
    ∀ {H : Store} {φ : Frame} {e : Expr} {H₁ : Store} {vs : List Val} {tr : List Event},
      e.pendingSafe = true → Lead M P n H φ H₁ vs tr e → StoreCC P.decls H₁ →
      Contents.copyClosedList P.decls (Contents.ofVals vs) = true →
      ∀ r, eval M (n + 1) P H φ e = r.withTrace tr →
        Exact P.decls H₁ (Contents.ownList P.decls (Contents.ofVals vs)) r := by
  have hbody : ∀ (f : Nat) (fd : FnDef), P.fns[f]? = some fd → fd.body.pendingSafe = true :=
    fun f fd h => (List.all_eq_true.mp hp) fd (List.mem_of_getElem? h)
  intro H φ e H₁ vs tr he hl hc₁ hvs r heq
  have hq : ∀ H' e', e'.returns = false ∧ e'.breaks = false →
      (eval M n P H' φ e').NoRet ∧ (eval M n P H' φ e').NoBrk := fun H' e' h =>
    ⟨(eval_quiet M P n H' φ e').1 h.1, (eval_quiet M P n H' φ e').2 h.2⟩
  have single : ∀ v, vs = [v] → (Contents.ofVal v).copyClosed P.decls = true := by
    intro v hv; subst hv; simpa [Contents.ofVals, Contents.copyClosedList] using hvs
  cases e with
  | intLit | floatLit | boolLit | unitLit | use | panic | drop | brk => exact hl.elim
  | loop e₁ =>
      obtain ⟨sc, rfl, hr⟩ := hl
      simp only [eval, hr] at heq
      split at heq
      · rename_i w _
        cases r <;> simp [EvalRes.withTrace] at heq
        trivial
      · rename_i H₂ evs hu
        cases r <;> simp [EvalRes.withTrace] at heq
        obtain ⟨rfl, rfl, rfl⟩ := heq
        obtain ⟨i, l, c⟩ := unwindLocs_exact hc₁ hu
        exact ⟨by omega, c, rfl, fun a _ => by have := i a; simp [Contents.ofVals, Contents.ownList]; omega⟩
  | binop op e₁ e₂ =>
      obtain ⟨v₁, rfl, hr⟩ := hl
      simp only [eval, hr, EvalRes.andThen] at heq
      obtain rfl := EvalRes.withTrace_inj heq
      rw [Contents.ownList_ofVals_single]
      simp only [Expr.pendingSafe, Bool.and_eq_true, Bool.not_eq_eq_eq_not, Bool.not_true,
        Expr.unwinds, Bool.or_eq_false_iff] at he
      refine Exact.bindHeld (ih H₁ φ e₂ hc₁ he.1.2) (hq H₁ e₂ he.2) (fun H₂ v₂ _ _ hc₂ _ => ?_)
      refine Exact.opRes hc₂ (fun v h => ⟨evalBinOp_scalar h, fun a _ => ?_⟩)
      obtain ⟨s₁, s₂⟩ := evalBinOp_val_args h
      simp [(Val.scalar_own (D := P.decls) s₁).1, (Val.scalar_own (D := P.decls) s₂).1]
  | unop op e₁ =>
      obtain ⟨v₁, rfl, hr⟩ := hl
      simp only [eval, hr, EvalRes.andThen] at heq
      obtain rfl := EvalRes.withTrace_inj heq
      rw [Contents.ownList_ofVals_single]
      refine Exact.opRes hc₁ (fun v h => ⟨evalUnOp_scalar h, fun a _ => ?_⟩)
      simp [(Val.scalar_own (D := P.decls) (evalUnOp_val_arg h)).1]
  | intCast w sg e₁ =>
      obtain ⟨v₁, rfl, hr⟩ := hl
      simp only [eval, hr, EvalRes.andThen] at heq
      obtain rfl := EvalRes.withTrace_inj heq
      rw [Contents.ownList_ofVals_single]
      refine Exact.opRes hc₁ (fun v h => ⟨evalIntCast_scalar h, fun a _ => ?_⟩)
      simp [(Val.scalar_own (D := P.decls) (evalIntCast_val_arg h)).1]
  | fintrin k e₁ =>
      obtain ⟨v₁, rfl, hr⟩ := hl
      simp only [eval, hr, EvalRes.andThen] at heq
      obtain rfl := EvalRes.withTrace_inj heq
      rw [Contents.ownList_ofVals_single]
      refine Exact.opRes hc₁ (fun v h => ⟨evalFintrin_scalar h, fun a _ => ?_⟩)
      simp [(Val.scalar_own (D := P.decls) (evalFintrin_val_arg h)).1]
  | dbg e₁ =>
      obtain ⟨v₁, rfl, hr⟩ := hl
      simp only [eval, hr, EvalRes.andThen] at heq
      obtain rfl := EvalRes.withTrace_inj heq
      rw [Contents.ownList_ofVals_single]
      split
      · rename_i hobs
        refine ⟨Nat.le_refl _, hc₁, rfl, fun a _ => ?_⟩
        simp [freedIds, Event.freed, (Val.scalar_own (D := P.decls) (Val.observable_scalar hobs)).1]
      · trivial
  | repeatArray T e₁ m =>
      obtain ⟨v₁, rfl, hr⟩ := hl
      simp only [eval, hr, EvalRes.andThen] at heq
      obtain rfl := EvalRes.withTrace_inj heq
      rw [Contents.ownList_ofVals_single]
      split
      · rename_i hm
        refine Exact.intro hc₁ (fun hv a ha => ?_)
        have := Contents.own_array_fresh hv (a := a) (by omega)
        rw [Contents.ownList_replicate hm] at this
        show ((Contents.array T H₁.length (Contents.ofVals (List.replicate m v₁))).own
          P.decls).count a = _
        rw [this, Val.own_of_copy hm]
      · trivial
  | mkStruct s args =>
      simp only [Lead] at hl
      simp only [eval, hl] at heq
      obtain rfl := EvalRes.withTrace_inj heq
      split
      · trivial
      · split
        · exact Exact.intro hc₁ (fun hv a ha => Contents.own_struct_fresh hv (by omega))
        · trivial
  | mkEnum e k args =>
      simp only [Lead] at hl
      simp only [eval, hl] at heq
      obtain rfl := EvalRes.withTrace_inj heq
      split
      · trivial
      · split
        · trivial
        · split
          · exact Exact.intro hc₁ (fun hv a ha => Contents.own_enum_fresh hv (by omega))
          · trivial
  | mkArray T args =>
      simp only [Lead] at hl
      simp only [eval, hl] at heq
      obtain rfl := EvalRes.withTrace_inj heq
      exact Exact.intro hc₁ (fun hv a ha => Contents.own_array_fresh hv (by omega))
  | «match» scrut arms =>
      obtain ⟨v₀, rfl, hr⟩ := hl
      simp only [eval, hr, EvalRes.andThen] at heq
      obtain rfl := EvalRes.withTrace_inj heq
      rw [Contents.ownList_ofVals_single]
      have hv := single v₀ rfl
      simp only [Expr.pendingSafe, Bool.and_eq_true] at he
      cases v₀ with
      | enum e k i vs =>
        dsimp only
        split
        · trivial
        · rename_i body harm
          have hpay := Contents.enum_payload hv
          refine Exact.prefix (H₁ := (mintParams H₁ vs).1) (Y := []) ?_ ?_ ?_
          · rw [mintParams_length]; omega
          · intro a _
            have := matchConsume_exact hv a
            rw [storeOwn_mintParams]
            simp only [List.count_append, List.count_nil]
            show _ = _ + ((Contents.enum e k i (Contents.ofVals vs)).own P.decls).count a
            omega
          · exact Exact.bind (ih _ _ body (hc₁.mintParams (hpay 0).2)
                (Expr.pendingSafeList_mem he.2 (List.mem_of_getElem? harm)))
              (fun H₂ v₂ _ _ hc₂ hv₂ => Exact.unwind hc₂ hv₂)
      | _ => trivial
  | indexRead p idx πs =>
      simp only [Lead] at hl
      simp only [eval, hl] at heq
      obtain rfl := EvalRes.withTrace_inj heq
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
              refine Exact.pure hc₁ ?_ (fun a _ => by
                simp [Val.own_of_copy hm, Val.ints_own (D := P.decls) his])
              rw [Contents.ofVal_toVal hv]
              exact Contents.readAt_copyClosed ρ
                (Contents.readAt_copyClosed _ (hc₁ ℓ c hc) hr) hr'
            · trivial
  | indexDrop p idx πs =>
      obtain ⟨v₁, rfl, hr⟩ := hl
      simp only [eval, hr, EvalRes.andThen] at heq
      obtain rfl := EvalRes.withTrace_inj heq
      rw [Contents.ownList_ofVals_single]
      exact Exact.pure hc₁ rfl (fun a _ => by simp [Val.own_of_copy (eval_indexRead_copy hr)])
  | indexWrite p idx πs e₁ =>
      obtain ⟨v, rfl, hr⟩ := hl
      simp only [eval, hr, EvalRes.andThen] at heq
      obtain rfl := EvalRes.withTrace_inj heq
      rw [Contents.ownList_ofVals_single]
      simp only [Expr.pendingSafe, Bool.and_eq_true] at he
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
  | letIn m e₁ e₂ =>
      obtain ⟨v₁, rfl, hr⟩ := hl
      simp only [eval, hr, EvalRes.andThen] at heq
      obtain rfl := EvalRes.withTrace_inj heq
      rw [Contents.ownList_ofVals_single]
      have hv₁ := single v₁ rfl
      simp only [Expr.pendingSafe, Bool.and_eq_true] at he
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
      obtain ⟨v, rfl, hr⟩ := hl
      simp only [eval, hr, EvalRes.andThen] at heq
      obtain rfl := EvalRes.withTrace_inj heq
      rw [Contents.ownList_ofVals_single]
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
      obtain ⟨v₁, rfl, hr⟩ := hl
      simp only [eval, hr, EvalRes.andThen] at heq
      obtain rfl := EvalRes.withTrace_inj heq
      rw [Contents.ownList_ofVals_single]
      simp only [Expr.pendingSafe, Bool.and_eq_true] at he
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
      obtain ⟨v₀, rfl, hr⟩ := hl
      simp only [eval, hr, EvalRes.andThen] at heq
      obtain rfl := EvalRes.withTrace_inj heq
      rw [Contents.ownList_ofVals_single]
      simp only [Expr.pendingSafe, Bool.and_eq_true] at he
      split
      · split
        · exact Exact.shift (Nat.le_refl _) (fun a _ => by simp [Val.own, Contents.ofVal, Contents.own])
            (ih H₁ φ e₁ hc₁ he.1.2)
        · exact Exact.shift (Nat.le_refl _) (fun a _ => by simp [Val.own, Contents.ofVal, Contents.own])
            (ih H₁ φ e₂ hc₁ he.2)
      · trivial
  | call f args =>
      simp only [Lead] at hl
      simp only [eval, hl] at heq
      obtain rfl := EvalRes.withTrace_inj heq
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
          · refine Exact.absorb (ih _ _ fd.body (hc₁.mintParams hvs) (hbody f fd hfd))
              (fun H₃ v _ _ hc₃ hv₃ => ?_)
            simp only [runAllScopeDrops]
            exact Exact.unwind hc₃ hv₃
        · trivial
  | ret e₁ =>
      obtain ⟨v, rfl, hr⟩ := hl
      simp only [eval, hr, EvalRes.andThen] at heq
      obtain rfl := EvalRes.withTrace_inj heq
      rw [Contents.ownList_ofVals_single]
      have hv := single v rfl
      simp only [runAllScopeDrops]
      split
      · trivial
      · rename_i H₂ evs hu
        obtain ⟨i, l, c⟩ := unwindLocs_exact hc₁ hu
        exact ⟨by omega, c, hv, fun a _ => by have := i a; omega⟩

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
        have he₁ : e₁.pendingSafe = true := by have h := he; simp only [Expr.pendingSafe, Bool.and_eq_true] at he; exact he.1.1
        simp only [eval]
        refine Exact.bind (ih H φ e₁ hcc he₁) (fun H₁ v₁ tr hr hc₁ hv₁ => ?_)
        rw [← Contents.ownList_ofVals_single]
        exact rest_step M hp ih (e := .binop op e₁ e₂) he ⟨v₁, rfl, hr⟩ hc₁
          (by simpa [Contents.ofVals, Contents.copyClosedList] using hv₁) _
          (by simp only [eval, hr, EvalRes.andThen])
    | unop op e₁ =>
        have he₁ : e₁.pendingSafe = true := by simpa [Expr.pendingSafe] using he
        simp only [eval]
        refine Exact.bind (ih H φ e₁ hcc he₁) (fun H₁ v₁ tr hr hc₁ hv₁ => ?_)
        rw [← Contents.ownList_ofVals_single]
        exact rest_step M hp ih (e := .unop op e₁) he ⟨v₁, rfl, hr⟩ hc₁
          (by simpa [Contents.ofVals, Contents.copyClosedList] using hv₁) _
          (by simp only [eval, hr, EvalRes.andThen])
    | intCast w sg e₁ =>
        have he₁ : e₁.pendingSafe = true := by simpa [Expr.pendingSafe] using he
        simp only [eval]
        refine Exact.bind (ih H φ e₁ hcc he₁) (fun H₁ v₁ tr hr hc₁ hv₁ => ?_)
        rw [← Contents.ownList_ofVals_single]
        exact rest_step M hp ih (e := .intCast w sg e₁) he ⟨v₁, rfl, hr⟩ hc₁
          (by simpa [Contents.ofVals, Contents.copyClosedList] using hv₁) _
          (by simp only [eval, hr, EvalRes.andThen])
    | fintrin k e₁ =>
        have he₁ : e₁.pendingSafe = true := by simpa [Expr.pendingSafe] using he
        simp only [eval]
        refine Exact.bind (ih H φ e₁ hcc he₁) (fun H₁ v₁ tr hr hc₁ hv₁ => ?_)
        rw [← Contents.ownList_ofVals_single]
        exact rest_step M hp ih (e := .fintrin k e₁) he ⟨v₁, rfl, hr⟩ hc₁
          (by simpa [Contents.ofVals, Contents.copyClosedList] using hv₁) _
          (by simp only [eval, hr, EvalRes.andThen])
    | dbg e₁ =>
        have he₁ : e₁.pendingSafe = true := by simpa [Expr.pendingSafe] using he
        simp only [eval]
        refine Exact.bind (ih H φ e₁ hcc he₁) (fun H₁ v₁ tr hr hc₁ hv₁ => ?_)
        rw [← Contents.ownList_ofVals_single]
        exact rest_step M hp ih (e := .dbg e₁) he ⟨v₁, rfl, hr⟩ hc₁
          (by simpa [Contents.ofVals, Contents.copyClosedList] using hv₁) _
          (by simp only [eval, hr, EvalRes.andThen])
    | mkStruct s args =>
        have he' := he
        simp only [Expr.pendingSafe, Bool.and_eq_true] at he'
        simp only [eval]
        have ka := hargs args he'.1 he'.2 H hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          obtain ⟨l₁, c₁, cv₁, i₁⟩ := ka
          refine Exact.prefix (Y := Contents.ownList P.decls (Contents.ofVals vs)) l₁
            (fun a ha => by have := i₁ a ha; simp only [List.count_nil] at *; omega) ?_
          exact rest_step M hp ih (e := .mkStruct s args) he hra c₁ cv₁ _ (by simp only [eval, hra])
    | mkEnum e k args =>
        have he' := he
        simp only [Expr.pendingSafe, Bool.and_eq_true] at he'
        simp only [eval]
        have ka := hargs args he'.1 he'.2 H hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          obtain ⟨l₁, c₁, cv₁, i₁⟩ := ka
          refine Exact.prefix (Y := Contents.ownList P.decls (Contents.ofVals vs)) l₁
            (fun a ha => by have := i₁ a ha; simp only [List.count_nil] at *; omega) ?_
          exact rest_step M hp ih (e := .mkEnum e k args) he hra c₁ cv₁ _ (by simp only [eval, hra])
    | mkArray T args =>
        have he' := he
        simp only [Expr.pendingSafe, Bool.and_eq_true] at he'
        simp only [eval]
        have ka := hargs args he'.1 he'.2 H hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          obtain ⟨l₁, c₁, cv₁, i₁⟩ := ka
          refine Exact.prefix (Y := Contents.ownList P.decls (Contents.ofVals vs)) l₁
            (fun a ha => by have := i₁ a ha; simp only [List.count_nil] at *; omega) ?_
          exact rest_step M hp ih (e := .mkArray T args) he hra c₁ cv₁ _ (by simp only [eval, hra])
    | «match» scrut arms =>
        have he₁ : scrut.pendingSafe = true := by have h := he; simp only [Expr.pendingSafe, Bool.and_eq_true] at he; exact he.1
        simp only [eval]
        refine Exact.bind (ih H φ scrut hcc he₁) (fun H₁ v₁ tr hr hc₁ hv₁ => ?_)
        rw [← Contents.ownList_ofVals_single]
        exact rest_step M hp ih (e := .«match» scrut arms) he ⟨v₁, rfl, hr⟩ hc₁
          (by simpa [Contents.ofVals, Contents.copyClosedList] using hv₁) _
          (by simp only [eval, hr, EvalRes.andThen])
    | repeatArray T e₁ m =>
        have he₁ : e₁.pendingSafe = true := by simpa [Expr.pendingSafe] using he
        simp only [eval]
        refine Exact.bind (ih H φ e₁ hcc he₁) (fun H₁ v₁ tr hr hc₁ hv₁ => ?_)
        rw [← Contents.ownList_ofVals_single]
        exact rest_step M hp ih (e := .repeatArray T e₁ m) he ⟨v₁, rfl, hr⟩ hc₁
          (by simpa [Contents.ofVals, Contents.copyClosedList] using hv₁) _
          (by simp only [eval, hr, EvalRes.andThen])
    | indexRead p idx πs =>
        have he' := he
        simp only [Expr.pendingSafe, Bool.and_eq_true] at he'
        simp only [eval]
        have ka := hargs idx he'.1 he'.2 H hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          obtain ⟨l₁, c₁, cv₁, i₁⟩ := ka
          refine Exact.prefix (Y := Contents.ownList P.decls (Contents.ofVals vs)) l₁
            (fun a ha => by have := i₁ a ha; simp only [List.count_nil] at *; omega) ?_
          exact rest_step M hp ih (e := .indexRead p idx πs) he hra c₁ cv₁ _ (by simp only [eval, hra])
    | indexDrop p idx πs =>
        have he₁ : (Expr.indexRead p idx πs).pendingSafe = true := by simpa [Expr.pendingSafe] using he
        simp only [eval]
        refine Exact.bind (ih H φ (Expr.indexRead p idx πs) hcc he₁) (fun H₁ v₁ tr hr hc₁ hv₁ => ?_)
        rw [← Contents.ownList_ofVals_single]
        exact rest_step M hp ih (e := .indexDrop p idx πs) he ⟨v₁, rfl, hr⟩ hc₁
          (by simpa [Contents.ofVals, Contents.copyClosedList] using hv₁) _
          (by simp only [eval, hr, EvalRes.andThen])
    | indexWrite p idx πs e₁ =>
        have he₁ : e₁.pendingSafe = true := by have h := he; simp only [Expr.pendingSafe, Bool.and_eq_true] at he; exact he.1.1
        simp only [eval]
        refine Exact.bind (ih H φ e₁ hcc he₁) (fun H₁ v₁ tr hr hc₁ hv₁ => ?_)
        rw [← Contents.ownList_ofVals_single]
        exact rest_step M hp ih (e := .indexWrite p idx πs e₁) he ⟨v₁, rfl, hr⟩ hc₁
          (by simpa [Contents.ofVals, Contents.copyClosedList] using hv₁) _
          (by simp only [eval, hr, EvalRes.andThen])
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
        have he₁ : e₁.pendingSafe = true := by have h := he; simp only [Expr.pendingSafe, Bool.and_eq_true] at he; exact he.1
        simp only [eval]
        refine Exact.bind (ih H φ e₁ hcc he₁) (fun H₁ v₁ tr hr hc₁ hv₁ => ?_)
        rw [← Contents.ownList_ofVals_single]
        exact rest_step M hp ih (e := .letIn m e₁ e₂) he ⟨v₁, rfl, hr⟩ hc₁
          (by simpa [Contents.ofVals, Contents.copyClosedList] using hv₁) _
          (by simp only [eval, hr, EvalRes.andThen])
    | assign p e₁ =>
        have he₁ : e₁.pendingSafe = true := by simpa [Expr.pendingSafe] using he
        simp only [eval]
        refine Exact.bind (ih H φ e₁ hcc he₁) (fun H₁ v₁ tr hr hc₁ hv₁ => ?_)
        rw [← Contents.ownList_ofVals_single]
        exact rest_step M hp ih (e := .assign p e₁) he ⟨v₁, rfl, hr⟩ hc₁
          (by simpa [Contents.ofVals, Contents.copyClosedList] using hv₁) _
          (by simp only [eval, hr, EvalRes.andThen])
    | seq e₁ e₂ =>
        have he₁ : e₁.pendingSafe = true := by have h := he; simp only [Expr.pendingSafe, Bool.and_eq_true] at he; exact he.1
        simp only [eval]
        refine Exact.bind (ih H φ e₁ hcc he₁) (fun H₁ v₁ tr hr hc₁ hv₁ => ?_)
        rw [← Contents.ownList_ofVals_single]
        exact rest_step M hp ih (e := .seq e₁ e₂) he ⟨v₁, rfl, hr⟩ hc₁
          (by simpa [Contents.ofVals, Contents.copyClosedList] using hv₁) _
          (by simp only [eval, hr, EvalRes.andThen])
    | ite c e₁ e₂ =>
        have he₁ : c.pendingSafe = true := by have h := he; simp only [Expr.pendingSafe, Bool.and_eq_true] at he; exact he.1.1
        simp only [eval]
        refine Exact.bind (ih H φ c hcc he₁) (fun H₁ v₁ tr hr hc₁ hv₁ => ?_)
        rw [← Contents.ownList_ofVals_single]
        exact rest_step M hp ih (e := .ite c e₁ e₂) he ⟨v₁, rfl, hr⟩ hc₁
          (by simpa [Contents.ofVals, Contents.copyClosedList] using hv₁) _
          (by simp only [eval, hr, EvalRes.andThen])
    | call f args =>
        have he' := he
        simp only [Expr.pendingSafe, Bool.and_eq_true] at he'
        simp only [eval]
        have ka := hargs args he'.1 he'.2 H hcc
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          obtain ⟨l₁, c₁, cv₁, i₁⟩ := ka
          refine Exact.prefix (Y := Contents.ownList P.decls (Contents.ofVals vs)) l₁
            (fun a ha => by have := i₁ a ha; simp only [List.count_nil] at *; omega) ?_
          exact rest_step M hp ih (e := .call f args) he hra c₁ cv₁ _ (by simp only [eval, hra])
    | ret e₁ =>
        have he₁ : e₁.pendingSafe = true := by simpa [Expr.pendingSafe] using he
        simp only [eval]
        refine Exact.bind (ih H φ e₁ hcc he₁) (fun H₁ v₁ tr hr hc₁ hv₁ => ?_)
        rw [← Contents.ownList_ofVals_single]
        exact rest_step M hp ih (e := .ret e₁) he ⟨v₁, rfl, hr⟩ hc₁
          (by simpa [Contents.ofVals, Contents.copyClosedList] using hv₁) _
          (by simp only [eval, hr, EvalRes.andThen])
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

/-! ## The frame-pop invariant: every allocation is retired

`Exact` counts where an owned value *is*; it does not say *which* cell holds
it, so on its own a value parked in a cell nobody can reach any more — a
callee's parameter cell a frame pop forgot to walk — would still count as
"in the store". `Tidy` closes that: every cell an evaluation allocates is
retired (`†`) by its end — a `let`'s at its `endscope` (§6.7), a `match` arm's
at the arm's end (§6.6), a callee's at the frame pop (§6.9), and each at the
σ-walk of an unwinding `return` — except, for an unwinding `break`, the cells
its carried scope record still owes, which the loop retires (§6.10) and
whose values `rest_exactly_once` at the loop counts as ended. Cells
outside the frame's environment are touched only to be retired, and an
unwinding `return` has retired the whole frame's record. It is a fact about
the store's shape alone, proved by its own fuel induction (`eval_tidy`), with
no typing derivation and no `pendingSafe` hypothesis. -/

/-- The frame names only cells the store already has (helper). -/
def Frame.In (φ : Frame) (H : Store) : Prop :=
  (∀ ℓ ∈ φ.env, ℓ < H.length) ∧ ∀ ℓ ∈ φ.scope, ℓ < H.length

/-- A frame inside a store is inside every store grown from it (helper). -/
theorem Frame.In.mono {φ : Frame} {H H' : Store} (h : φ.In H) (hl : H.length ≤ H'.length) :
    φ.In H' :=
  ⟨fun ℓ hm => Nat.lt_of_lt_of_le (h.1 ℓ hm) hl, fun ℓ hm => Nat.lt_of_lt_of_le (h.2 ℓ hm) hl⟩

/-- Nothing changed (helper). -/
theorem Local.refl (φ : Frame) (H : Store) : Local φ H H :=
  ⟨Nat.le_refl _, fun _ _ _ => .inl rfl⟩

/-- Local steps compose (helper). -/
theorem Local.trans {φ : Frame} {H H₁ H₂ : Store} (h₁ : Local φ H H₁) (h₂ : Local φ H₁ H₂) :
    Local φ H H₂ := by
  refine ⟨Nat.le_trans h₁.1 h₂.1, fun ℓ hl hn => ?_⟩
  rcases h₂.2 ℓ (Nat.lt_of_lt_of_le hl h₁.1) hn with h | h
  · rw [h]; exact h₁.2 ℓ hl hn
  · exact .inr h

/-- A write through the environment is local (helper). -/
theorem Local.set {φ : Frame} {H : Store} {ℓ : Nat} (x : Cell) (hℓ : ℓ ∈ φ.env) :
    Local φ H (H.set ℓ x) := by
  refine ⟨by simp, fun ℓ' _ hn => .inl ?_⟩
  have : ℓ ≠ ℓ' := fun h => hn (h ▸ hℓ)
  rw [List.getElem?_set_ne this]

/-- Growing the store is local (helper). -/
theorem Local.append {φ : Frame} {H : Store} (ext : Store) : Local φ H (H ++ ext) :=
  ⟨by simp, fun ℓ hl _ => .inl (List.getElem?_append_left hl)⟩

/-- Nothing allocated, nothing to retire (helper). -/
theorem Retired.same {H H' : Store} {keep : List Nat} (h : H'.length ≤ H.length) :
    Retired H keep H' := fun _ h₁ h₂ _ => absurd (Nat.lt_of_lt_of_le h₂ h) (Nat.not_lt.mpr h₁)

/-- A value produced where the store is (helper). -/
theorem Tidy.same {φ : Frame} {H : Store} {v : Val} {tr : List Event} :
    Tidy φ H (.ok H v tr) := ⟨Local.refl φ H, Retired.same (Nat.le_refl _)⟩

/-- A write through the environment (helper). -/
theorem Tidy.write {φ : Frame} {H : Store} {ℓ : Nat} {x : Cell} {v : Val} {tr : List Event}
    (hℓ : ℓ ∈ φ.env) : Tidy φ H (.ok (H.set ℓ x) v tr) :=
  ⟨Local.set x hℓ, Retired.same (by simp)⟩

/-- An operator's outcome (helper). -/
theorem Tidy.opRes {φ : Frame} {H : Store} {o : OpRes} : Tidy φ H (o.toRes H) := by
  cases o <;> first | exact Tidy.same | trivial

/-- Aggregate introduction reserves one retired slot (helper). -/
theorem Tidy.intro {D : Decls} {φ : Frame} {H : Store} {mk : Nat → Val} :
    Tidy φ H (introVal D H mk) := by
  unfold introVal
  split
  · refine ⟨Local.append _, fun ℓ h₁ h₂ _ => ?_⟩
    simp only [List.length_append, List.length_cons, List.length_nil] at h₂
    have : ℓ = H.length := by omega
    subst this
    simp
  · trivial

/-- **Composition**: a step that allocated and retired locally, then an
evaluation in the same frame (helper). -/
theorem Tidy.prefix {φ : Frame} {H H₁ : Store} {tr : List Event} {r : EvalRes}
    (hf : ∀ ℓ ∈ φ.env, ℓ < H.length) (hl : Local φ H H₁) (hre : Retired H [] H₁)
    (hr : Tidy φ H₁ r) : Tidy φ H (r.withTrace tr) := by
  have key : ∀ (H₂ : Store) (keep : List Nat), Local φ H₁ H₂ → Retired H₁ keep H₂ →
      Retired H keep H₂ := by
    intro H₂ keep l₂ r₂ ℓ h₁ h₂ hk
    by_cases hlt : ℓ < H₁.length
    · have hd := hre ℓ h₁ hlt (by simp)
      have hn : ℓ ∉ φ.env := fun hm => absurd (hf ℓ hm) (Nat.not_lt.mpr h₁)
      rcases l₂.2 ℓ hlt hn with h | h
      · rw [h]; exact hd
      · exact h
    · exact r₂ ℓ (Nat.not_lt.mp hlt) h₂ hk
  cases r with
  | ok H₂ v tr₂ => exact ⟨hl.trans hr.1, key H₂ [] hr.1 hr.2⟩
  | returned H₂ v tr₂ => exact ⟨hl.trans hr.1, key H₂ [] hr.1 hr.2.1, hr.2.2⟩
  | broke H₂ sc tr₂ =>
      obtain ⟨locs, hsc, hfr⟩ := hr.2.2
      exact ⟨hl.trans hr.1, key H₂ sc hr.1 hr.2.1, locs, hsc,
        fun ℓ hm => Nat.le_trans hl.1 (hfr ℓ hm)⟩
  | _ => trivial

/-- §6.2's search keeps the frame-pop invariant (helper). -/
theorem Tidy.bind {φ : Frame} {H : Store} {r : EvalRes} {k : Store → Val → EvalRes}
    (hf : ∀ ℓ ∈ φ.env, ℓ < H.length) (hr : Tidy φ H r)
    (hk : ∀ H₁ v tr, r = .ok H₁ v tr → Tidy φ H₁ (k H₁ v)) : Tidy φ H (r.andThen k) := by
  cases r with
  | ok H₁ v tr => exact Tidy.prefix hf hr.1 hr.2 (hk H₁ v tr rfl)
  | _ => exact hr

/-- `drop-retire` retires exactly its cell (helper). -/
theorem dropRetire_shape {D : Decls} {H H' : Store} {ℓ : Nat} {evs : List Event}
    (h : dropRetire D H ℓ = .ok (H', evs)) : H' = H.set ℓ .dead := by
  unfold dropRetire at h
  split at h
  · cases h
  · cases h
  · split at h
    · cases h
    · split at h
      · cases h
      · cases h; rfl

/-- The same at a retired cell: `drop-retire` refuses (helper). -/
theorem dropRetire_live {D : Decls} {H H' : Store} {ℓ : Nat} {evs : List Event}
    (h : dropRetire D H ℓ = .ok (H', evs)) : ℓ < H.length := by
  unfold dropRetire at h
  split at h
  · cases h
  · cases h
  · rename_i c hc; exact (List.getElem?_eq_some_iff.mp hc).1

/-- **`run-scope-drops` retires exactly its cells** (§6.1): the store keeps its
length, every listed cell is `†`, every other cell is untouched (helper). -/
theorem unwindLocs_shape {D : Decls} : ∀ {H H' : Store} {ls : List Nat} {evs : List Event},
    unwindLocs D H ls = .ok (H', evs) →
      H'.length = H.length ∧ (∀ ℓ ∈ ls, H'[ℓ]? = some .dead) ∧ ∀ ℓ, ℓ ∉ ls → H'[ℓ]? = H[ℓ]?
  | H, H', [], evs, h => by
      simp [unwindLocs] at h; obtain ⟨rfl, rfl⟩ := h
      exact ⟨rfl, by simp, fun _ _ => rfl⟩
  | H, H', ℓ :: ls, evs, h => by
      simp only [unwindLocs] at h
      split at h
      · cases h
      · rename_i H₁ evs₁ h₁
        split at h
        · cases h
        · rename_i H₂ evs₂ h₂
          cases h
          have e₁ := dropRetire_shape h₁
          obtain ⟨l₂, d₂, u₂⟩ := unwindLocs_shape h₂
          subst e₁
          refine ⟨by simp [l₂], fun ℓ' hm => ?_, fun ℓ' hn => ?_⟩
          · rcases List.mem_cons.mp hm with rfl | hm
            · by_cases hl : ℓ' ∈ ls
              · exact d₂ ℓ' hl
              · rw [u₂ ℓ' hl]
                exact List.getElem?_set_self (dropRetire_live h₁)
            · exact d₂ ℓ' hm
          · have hne : ℓ ≠ ℓ' := fun e => hn (e ▸ List.mem_cons_self)
            rw [u₂ ℓ' (fun hm => hn (List.mem_cons_of_mem _ hm)), List.getElem?_set_ne hne]

/-- What a scope teardown after a value does to the store: refuse, or retire
exactly the cells `ls` names (helper). -/
def KillsOnly (ls : List Nat) (H : Store) (v : Val) (R : EvalRes) : Prop :=
  (∃ w, R = .stuck w) ∨
    ∃ H' evs, R = .ok H' v evs ∧ H'.length = H.length ∧ (∀ ℓ ∈ ls, H'[ℓ]? = some .dead) ∧
      ∀ ℓ, ℓ ∉ ls → H'[ℓ]? = H[ℓ]?

/-- `run-scope-drops` as a teardown (helper). -/
theorem unwind_kills {D : Decls} {H : Store} {v : Val} {ls ls' : List Nat}
    (hm : ∀ ℓ, ℓ ∈ ls' ↔ ℓ ∈ ls) :
    KillsOnly ls H v (match unwindLocs D H ls' with
      | .error w => .stuck w
      | .ok (H', evs) => .ok H' v evs) := by
  split
  · exact .inl ⟨_, rfl⟩
  · rename_i H' evs hu
    obtain ⟨l, d, u⟩ := unwindLocs_shape hu
    exact .inr ⟨H', evs, rfl, l, fun ℓ h => d ℓ ((hm ℓ).mpr h),
      fun ℓ h => u ℓ (fun h' => h ((hm ℓ).mp h'))⟩

/-- `drop-retire` of one cell as a teardown (helper). -/
theorem dropRetire_kills {D : Decls} {H : Store} {v : Val} {ℓ : Nat} :
    KillsOnly [ℓ] H v (match dropRetire D H ℓ with
      | .error w => .stuck w
      | .ok (H', evs) => .ok H' v evs) := by
  split
  · exact .inl ⟨_, rfl⟩
  · rename_i H' evs hd
    have he := dropRetire_shape hd
    have hlive := dropRetire_live hd
    subst he
    refine .inr ⟨_, evs, rfl, by simp, fun ℓ' hm => ?_, fun ℓ' hn => ?_⟩
    · simp only [List.mem_singleton] at hm; subst hm
      exact List.getElem?_set_self hlive
    · have : ℓ ≠ ℓ' := fun e => hn (by simp [e])
      rw [List.getElem?_set_ne this]

/-- **A scope opened above the frame and closed at its end** (§6.7's `let`,
§6.6's `match` arm): an evaluation in the frame extended by fresh cells `ls`,
followed on a value by a teardown that retires exactly `ls`, keeps the
frame-pop invariant in the frame it was opened in. An unwinding `return`
finds `ls` in the extended record and has retired them; an unwinding `break`
carries them in its record (helper). -/
theorem Tidy.scoped {φ : Frame} {H Hm : Store} {ls : List Nat} {r : EvalRes}
    {k : Store → Val → EvalRes}
    (hls : ∀ ℓ, ℓ ∈ ls ↔ H.length ≤ ℓ ∧ ℓ < Hm.length) (hlen : H.length ≤ Hm.length)
    (hpre : ∀ ℓ, ℓ < H.length → Hm[ℓ]? = H[ℓ]?)
    (hr : Tidy { env := ls.reverse ++ φ.env, scope := φ.scope ++ ls } Hm r)
    (hk : ∀ H₂ v tr, r = .ok H₂ v tr → KillsOnly ls H₂ v (k H₂ v)) :
    Tidy φ H (r.andThen k) := by
  have loc : ∀ H₂, Local { env := ls.reverse ++ φ.env, scope := φ.scope ++ ls } Hm H₂ →
      Local φ H H₂ := by
    intro H₂ l
    refine ⟨Nat.le_trans hlen l.1, fun ℓ hl hn => ?_⟩
    have hnl : ℓ ∉ ls := fun h => absurd ((hls ℓ).mp h).1 (Nat.not_le.mpr hl)
    have hn' : ℓ ∉ ls.reverse ++ φ.env := by simp [hnl, hn]
    rw [← hpre ℓ hl]
    exact l.2 ℓ (Nat.lt_of_lt_of_le hl hlen) hn'
  have ret : ∀ H₂ keep, (∀ ℓ ∈ ls, ℓ ∉ keep → H₂[ℓ]? = some .dead) → Retired Hm keep H₂ →
      Retired H keep H₂ := by
    intro H₂ keep hd r₂ ℓ h₁ h₂ hk'
    by_cases hm : ℓ ∈ ls
    · exact hd ℓ hm hk'
    · have : Hm.length ≤ ℓ := by
        exact Nat.not_lt.mp (fun hc => hm ((hls ℓ).mpr ⟨h₁, hc⟩))
      exact r₂ ℓ this h₂ hk'
  cases r with
  | ok H₂ v tr =>
      obtain ⟨l₂, r₂⟩ := hr
      simp only [EvalRes.andThen]
      rcases hk H₂ v tr rfl with ⟨w, hw⟩ | ⟨H₃, evs, hw, hlen₃, hd, hu⟩
      · rw [hw]; trivial
      · rw [hw]
        have l₃ : Local φ H H₃ := by
          have := loc H₂ l₂
          refine ⟨by have := this.1; omega, fun ℓ hl hn => ?_⟩
          have hnl : ℓ ∉ ls := fun h => absurd ((hls ℓ).mp h).1 (Nat.not_le.mpr hl)
          rw [hu ℓ hnl]; exact this.2 ℓ hl hn
        refine ⟨l₃, fun ℓ h₁ h₂ _ => ?_⟩
        by_cases hm : ℓ ∈ ls
        · exact hd ℓ hm
        · rw [hu ℓ hm]
          have : Hm.length ≤ ℓ := by
            exact Nat.not_lt.mp (fun hc => hm ((hls ℓ).mpr ⟨h₁, hc⟩))
          exact r₂ ℓ this (by omega) (by simp)
  | returned H₂ v tr =>
      obtain ⟨l₂, r₂, s₂⟩ := hr
      refine ⟨loc H₂ l₂, ret H₂ [] (fun ℓ hm _ => s₂ ℓ (by simp [hm])) r₂,
        fun ℓ hm => s₂ ℓ (by simp [hm])⟩
  | broke H₂ sc tr =>
      obtain ⟨l₂, r₂, locs, hsc, hfr⟩ := hr
      have hsc : sc = (φ.scope ++ ls) ++ locs ∧ ∀ ℓ ∈ locs, Hm.length ≤ ℓ := ⟨hsc, hfr⟩
      refine ⟨loc H₂ l₂, ret H₂ sc (fun ℓ hm hk' => absurd (by rw [hsc.1]; simp [hm]) hk') r₂,
        ls ++ locs, by rw [hsc.1]; simp, fun ℓ hm => ?_⟩
      rcases List.mem_append.mp hm with h | h
      · exact ((hls ℓ).mp h).1
      · exact Nat.le_trans hlen (hsc.2 ℓ h)
  | _ => trivial

/-- Minted cells are the next indices, in order (helper). -/
theorem mintParams_locs : ∀ (H : Store) (vs : List Val),
    (mintParams H vs).2 = List.range' H.length vs.length
  | _, [] => rfl
  | H, v :: vs => by
      simp only [mintParams, mintParams_locs (H ++ [Cell.full (Contents.ofVal v)]) vs,
        List.length_append, List.length_cons, List.length_nil, List.range'_succ]

/-- Membership in the minted cells (helper). -/
theorem mintParams_mem (H : Store) (vs : List Val) (ℓ : Nat) :
    ℓ ∈ (mintParams H vs).2 ↔ H.length ≤ ℓ ∧ ℓ < (mintParams H vs).1.length := by
  rw [mintParams_locs, mintParams_length, List.mem_range'_1]

/-- Minting leaves the old cells alone (helper). -/
theorem mintParams_pre (H : Store) (vs : List Val) (ℓ : Nat) (h : ℓ < H.length) :
    (mintParams H vs).1[ℓ]? = H[ℓ]? := by
  rw [mintParams_store, List.getElem?_append_left h]

/-- A dynamic place's cell is named by the environment (helper). -/
theorem dynPlace_env {H : Store} {φ : Frame} {p : Place} {vs : List Val} {πs : List (List Nat)}
    {ℓ : Nat} {c sub : Contents} {ρ : List Nat} (h : dynPlace H φ p vs πs = .at ℓ c sub ρ) :
    ℓ ∈ φ.env := by
  unfold dynPlace at h
  split at h
  · cases h
  · split at h
    · cases h
    · rename_i ℓ' hρ
      split at h
      · cases h
      · cases h
      · split at h
        · cases h
        · split at h
          · cases h; exact List.mem_of_getElem? hρ
          · cases h
          · cases h

/-- The frame-pop invariant for an argument list (helper). -/
def ArgsTidy (φ : Frame) (H : Store) : ArgsRes → Prop
  | .ok H' _ _ => Local φ H H' ∧ Retired H [] H'
  | .abort r => Tidy φ H r

/-- An argument list keeps the frame-pop invariant (helper). -/
theorem evalArgs_tidy {φ : Frame} {ev : Store → Expr → EvalRes} :
    ∀ {es : List Expr}, (∀ H e, e ∈ es → φ.In H → Tidy φ H (ev H e)) →
      ∀ H, φ.In H → ArgsTidy φ H (evalArgs ev H es)
  | [], _, H, _ => ⟨Local.refl φ H, Retired.same (Nat.le_refl _)⟩
  | e :: es, hev, H, hf => by
      have h₁ := hev H e List.mem_cons_self hf
      simp only [evalArgs]
      cases hr : ev H e with
      | ok H₁ v tr =>
          rw [hr] at h₁
          obtain ⟨l₁, r₁⟩ := h₁
          have ih := evalArgs_tidy (es := es) (fun H e' hm => hev H e' (List.mem_cons_of_mem _ hm))
            H₁ (hf.mono l₁.1)
          dsimp only
          cases hra : evalArgs ev H₁ es with
          | ok H₂ vs tr₂ =>
              rw [hra] at ih
              have := Tidy.prefix (tr := []) hf.1 l₁ r₁ (r := .ok H₂ .unit []) ih
              exact this
          | abort r =>
              rw [hra] at ih
              exact Tidy.prefix hf.1 l₁ r₁ ih
      | returned => rw [hr] at h₁; exact h₁
      | broke => rw [hr] at h₁; exact h₁
      | panic => trivial
      | stuck => trivial
      | outOfFuel => trivial

/-- **§6.9's frame, pushed and popped**: a callee's body, run in a frame of
fresh parameter cells `ls` and absorbed at the call boundary — its value's
frame popped by `run-all-scope-drops`, its unwinding `return` having popped
it already — keeps the caller's frame-pop invariant (helper). -/
theorem Tidy.call {D : Decls} {φ : Frame} {H Hm : Store} {ls : List Nat} {r : EvalRes}
    (hls : ∀ ℓ, ℓ ∈ ls ↔ H.length ≤ ℓ ∧ ℓ < Hm.length) (hlen : H.length ≤ Hm.length)
    (hpre : ∀ ℓ, ℓ < H.length → Hm[ℓ]? = H[ℓ]?)
    (hr : Tidy { env := ls.reverse, scope := ls } Hm r) :
    Tidy φ H (r.absorb fun H₃ v => match runAllScopeDrops D H₃ { env := ls.reverse, scope := ls } with
      | .error w => .stuck w
      | .ok (H₄, evs) => .ok H₄ v evs) := by
  have loc : ∀ H₂, Local { env := ls.reverse, scope := ls } Hm H₂ → Local φ H H₂ := by
    intro H₂ l
    refine ⟨Nat.le_trans hlen l.1, fun ℓ hl _ => ?_⟩
    have hnl : ℓ ∉ ls := fun h => absurd ((hls ℓ).mp h).1 (Nat.not_le.mpr hl)
    rw [← hpre ℓ hl]
    exact l.2 ℓ (Nat.lt_of_lt_of_le hl hlen) (fun h => hnl (List.mem_reverse.mp h))
  have ret : ∀ H₂, (∀ ℓ ∈ ls, H₂[ℓ]? = some .dead) → Retired Hm [] H₂ → Retired H [] H₂ := by
    intro H₂ hd r₂ ℓ h₁ h₂ hk'
    by_cases hm : ℓ ∈ ls
    · exact hd ℓ hm
    · have : Hm.length ≤ ℓ := by
        exact Nat.not_lt.mp (fun hc => hm ((hls ℓ).mpr ⟨h₁, hc⟩))
      exact r₂ ℓ this h₂ hk'
  cases r with
  | ok H₂ v tr =>
      obtain ⟨l₂, r₂⟩ := hr
      simp only [EvalRes.absorb, runAllScopeDrops]
      split
      · trivial
      · rename_i H₃ evs hu
        obtain ⟨hl₃, d, u⟩ := unwindLocs_shape hu
        have l₃ : Local φ H H₃ := by
          have := loc H₂ l₂
          refine ⟨by have := this.1; omega, fun ℓ hl hn => ?_⟩
          have hnl : ℓ ∉ ls.reverse := fun h =>
            absurd ((hls ℓ).mp (List.mem_reverse.mp h)).1 (Nat.not_le.mpr hl)
          rw [u ℓ hnl]; exact this.2 ℓ hl hn
        refine ⟨l₃, fun ℓ h₁ h₂ _ => ?_⟩
        by_cases hm : ℓ ∈ ls
        · exact d ℓ (List.mem_reverse.mpr hm)
        · rw [u ℓ (fun h => hm (List.mem_reverse.mp h))]
          have : Hm.length ≤ ℓ := by
            exact Nat.not_lt.mp (fun hc => hm ((hls ℓ).mpr ⟨h₁, hc⟩))
          exact r₂ ℓ this (by omega) (by simp)
  | returned H₂ v tr =>
      obtain ⟨l₂, r₂, s₂⟩ := hr
      exact ⟨loc H₂ l₂, ret H₂ (fun ℓ hm => s₂ ℓ hm) r₂⟩
  | _ => trivial

/-- **Every allocation is retired** (§6.7, §6.9, §6.10): every evaluation, of
every expression, in every frame that names only existing cells, keeps
`Tidy`. By fuel induction over `eval`; no typing derivation. -/
theorem eval_tidy (M : FloatOps) (P : Program) :
    ∀ (fuel : Nat) (H : Store) (φ : Frame) (e : Expr), φ.In H → Tidy φ H (eval M fuel P H φ e) := by
  intro fuel
  induction fuel with
  | zero => intro H φ e _; simp only [eval]; trivial
  | succ n ih =>
    intro H φ e hf
    have hargs := fun (es : List Expr) (H' : Store) (hf' : φ.In H') =>
      evalArgs_tidy (ev := fun H'' e' => eval M n P H'' φ e') (es := es)
        (fun H'' e' _ hf'' => ih H'' φ e' hf'') H' hf'
    cases e with
    | intLit | floatLit | boolLit | unitLit => exact Tidy.same
    | panic => simp only [eval]; trivial
    | brk =>
        simp only [eval]
        exact ⟨Local.refl φ H, Retired.same (Nat.le_refl _), [], by simp, by simp⟩
    | use p | drop p =>
        simp only [eval]
        split
        · trivial
        · rename_i ℓ hρ
          have hℓ := List.mem_of_getElem? hρ
          (repeat' split) <;> first | trivial | exact Tidy.same | exact Tidy.write hℓ
    | binop op e₁ e₂ =>
        simp only [eval]
        refine Tidy.bind hf.1 (ih H φ e₁ hf) (fun H₁ _ _ hr => ?_)
        have hl := (ih H φ e₁ hf); rw [hr] at hl
        exact Tidy.bind (hf.mono hl.1.1).1 (ih H₁ φ e₂ (hf.mono hl.1.1)) (fun _ _ _ _ => Tidy.opRes)
    | unop op e₁ | intCast w sg e₁ | fintrin k e₁ =>
        simp only [eval]
        exact Tidy.bind hf.1 (ih H φ e₁ hf) (fun _ _ _ _ => Tidy.opRes)
    | dbg e₁ =>
        simp only [eval]
        exact Tidy.bind hf.1 (ih H φ e₁ hf) (fun _ _ _ _ => by
          split
          · exact Tidy.same
          · trivial)
    | repeatArray T e₁ m =>
        simp only [eval]
        exact Tidy.bind hf.1 (ih H φ e₁ hf) (fun _ _ _ _ => by
          split
          · exact Tidy.intro
          · trivial)
    | mkStruct s args | mkEnum e k args | mkArray T args =>
        simp only [eval]
        have ka := hargs args H hf
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          refine Tidy.prefix hf.1 ka.1 ka.2 ?_
          (repeat' split) <;> first | trivial | exact Tidy.intro
    | indexRead p idx πs =>
        simp only [eval]
        have ka := hargs idx H hf
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          refine Tidy.prefix hf.1 ka.1 ka.2 ?_
          (repeat' split) <;> first | trivial | exact Tidy.same
    | indexDrop p idx πs =>
        simp only [eval]
        exact Tidy.bind hf.1 (ih H φ _ hf) (fun _ _ _ _ => Tidy.same)
    | indexWrite p idx πs e₁ =>
        simp only [eval]
        refine Tidy.bind hf.1 (ih H φ e₁ hf) (fun H₁ _ _ hr => ?_)
        have hl := (ih H φ e₁ hf); rw [hr] at hl
        have hf₁ := hf.mono hl.1.1
        have ka := hargs idx H₁ hf₁
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₂ vs tr hra
          rw [hra] at ka
          refine Tidy.prefix hf₁.1 ka.1 ka.2 ?_
          split
          · trivial
          · trivial
          · rename_i ℓ c sub ρ hdp
            have hℓ := dynPlace_env hdp
            (repeat' split) <;> first | trivial | exact Tidy.write hℓ
    | «match» scrut arms =>
        simp only [eval]
        refine Tidy.bind hf.1 (ih H φ scrut hf) (fun H₀ v _ hr => ?_)
        have hl := (ih H φ scrut hf); rw [hr] at hl
        have hf₀ := hf.mono hl.1.1
        cases v with
        | enum e k i vs =>
          dsimp only
          split
          · trivial
          · rename_i body _
            refine Tidy.prefix (H₁ := H₀) hf₀.1 (Local.refl φ H₀) (Retired.same (Nat.le_refl _)) ?_
            have hmem := mintParams_mem H₀ vs
            refine Tidy.scoped (Hm := (mintParams H₀ vs).1) hmem
              (by rw [mintParams_length]; omega) (mintParams_pre H₀ vs)
              (ih _ _ body ⟨fun ℓ hm => ?_, fun ℓ hm => ?_⟩)
              (fun H₂ v₂ _ _ => unwind_kills (fun ℓ => by simp))
            · rcases List.mem_append.mp hm with h | h
              · exact ((hmem ℓ).mp (List.mem_reverse.mp h)).2
              · exact Nat.lt_of_lt_of_le (hf₀.1 ℓ h) (by rw [mintParams_length]; omega)
            · rcases List.mem_append.mp hm with h | h
              · exact Nat.lt_of_lt_of_le (hf₀.2 ℓ h) (by rw [mintParams_length]; omega)
              · exact ((hmem ℓ).mp h).2
        | _ => trivial
    | letIn m e₁ e₂ =>
        simp only [eval]
        refine Tidy.bind hf.1 (ih H φ e₁ hf) (fun H₁ v₁ _ hr => ?_)
        have hl := (ih H φ e₁ hf); rw [hr] at hl
        have hf₁ := hf.mono hl.1.1
        have hls : ∀ ℓ, ℓ ∈ [H₁.length] ↔
            H₁.length ≤ ℓ ∧ ℓ < (H₁ ++ [Cell.full (Contents.ofVal v₁)]).length := by
          intro ℓ
          simp only [List.mem_singleton, List.length_append, List.length_cons, List.length_nil]
          constructor
          · intro h; subst h; exact ⟨Nat.le_refl _, Nat.lt_succ_self _⟩
          · intro ⟨h₁, h₂⟩; exact Nat.le_antisymm (Nat.le_of_lt_succ h₂) h₁
        refine Tidy.scoped (Hm := H₁ ++ [.full (Contents.ofVal v₁)]) hls (by simp)
          (fun ℓ h => List.getElem?_append_left h) ?_ (fun H₂ v₂ _ _ => dropRetire_kills)
        refine ih _ _ e₂ ⟨fun ℓ hm => ?_, fun ℓ hm => ?_⟩
        · simp only [List.reverse_cons, List.reverse_nil, List.nil_append, List.cons_append,
            List.mem_cons] at hm
          rcases hm with rfl | h
          · simp
          · have := hf₁.1 ℓ h; simp; omega
        · rcases List.mem_append.mp hm with h | h
          · have := hf₁.2 ℓ h; simp; omega
          · simp at h; subst h; simp
    | assign p e₁ =>
        simp only [eval]
        refine Tidy.bind hf.1 (ih H φ e₁ hf) (fun H₁ _ _ _ => ?_)
        split
        · trivial
        · rename_i ℓ hρ
          have hℓ := List.mem_of_getElem? hρ
          (repeat' split) <;> first | trivial | exact Tidy.write hℓ
    | seq e₁ e₂ =>
        simp only [eval]
        refine Tidy.bind hf.1 (ih H φ e₁ hf) (fun H₁ _ _ hr => ?_)
        have hl := (ih H φ e₁ hf); rw [hr] at hl
        have hf₁ := hf.mono hl.1.1
        split
        · trivial
        · split
          · trivial
          · exact Tidy.prefix hf₁.1 (Local.refl φ H₁) (Retired.same (Nat.le_refl _))
              (ih H₁ φ e₂ hf₁)
        · exact ih H₁ φ e₂ hf₁
    | ite c e₁ e₂ =>
        simp only [eval]
        refine Tidy.bind hf.1 (ih H φ c hf) (fun H₀ _ _ hr => ?_)
        have hl := (ih H φ c hf); rw [hr] at hl
        have hf₀ := hf.mono hl.1.1
        split
        · split
          · exact ih H₀ φ e₁ hf₀
          · exact ih H₀ φ e₂ hf₀
        · trivial
    | call f args =>
        simp only [eval]
        have ka := hargs args H hf
        split
        · rename_i r hra; rw [hra] at ka; exact ka
        · rename_i H₁ vs tr hra
          rw [hra] at ka
          refine Tidy.prefix hf.1 ka.1 ka.2 ?_
          split
          · trivial
          · rename_i fd _
            split
            · have hmem := mintParams_mem H₁ vs
              have hlen : H₁.length ≤ (mintParams H₁ vs).1.length := by
                rw [mintParams_length]; omega
              have hb := ih (mintParams H₁ vs).1
                { env := (mintParams H₁ vs).2.reverse, scope := (mintParams H₁ vs).2 } fd.body
                ⟨fun ℓ hm => ((hmem ℓ).mp (List.mem_reverse.mp hm)).2,
                 fun ℓ hm => ((hmem ℓ).mp hm).2⟩
              exact Tidy.call hmem hlen (mintParams_pre H₁ vs) hb
            · trivial
    | ret e₁ =>
        simp only [eval]
        refine Tidy.bind hf.1 (ih H φ e₁ hf) (fun H₁ v _ hr => ?_)
        have hl := (ih H φ e₁ hf); rw [hr] at hl
        have hf₁ := hf.mono hl.1.1
        simp only [runAllScopeDrops]
        split
        · trivial
        · rename_i H₂ evs hu
          obtain ⟨l, d, u⟩ := unwindLocs_shape hu
          refine ⟨⟨by omega, fun ℓ hl' _ => ?_⟩, Retired.same (by omega),
            fun ℓ hm => d ℓ (List.mem_reverse.mpr hm)⟩
          by_cases hm : ℓ ∈ φ.scope.reverse
          · exact .inr (d ℓ hm)
          · exact .inl (u ℓ hm)
    | loop e₁ =>
        simp only [eval]
        have hb := ih H φ e₁ hf
        split
        · rename_i H₁ tr hr
          rw [hr] at hb
          exact Tidy.prefix hf.1 hb.1 hb.2 (ih H₁ φ (.loop e₁) (hf.mono hb.1.1))
        · trivial
        · rename_i H₁ sc tr hr
          rw [hr] at hb
          obtain ⟨l₁, r₁, locs, hsc, _⟩ := hb
          split
          · trivial
          · rename_i H₂ evs hu
            obtain ⟨l, d, u⟩ := unwindLocs_shape hu
            have hdrop : sc.drop φ.scope.length = locs := by rw [hsc]; simp
            rw [hdrop] at d u
            have := l₁.1
            refine ⟨⟨by omega, fun ℓ hl' hn => ?_⟩, fun ℓ h₁ h₂ _ => ?_⟩
            · by_cases hm : ℓ ∈ locs.reverse
              · exact .inr (d ℓ hm)
              · rw [u ℓ hm]; exact l₁.2 ℓ hl' hn
            · by_cases hm : ℓ ∈ locs.reverse
              · exact d ℓ hm
              · rw [u ℓ hm]
                refine r₁ ℓ h₁ (by omega) ?_
                rw [hsc]
                simp only [List.mem_append, not_or]
                exact ⟨fun h => absurd (hf.2 ℓ h) (Nat.not_lt.mpr h₁),
                  fun h => hm (List.mem_reverse.mpr h)⟩
        · exact hb

/-! ## §7: every owned value ends exactly once -/

/-- A frame agreeing with a context names only existing cells (helper). -/
theorem FrameMatches.frameIn {D : Decls} {Γ : Ctx} {φ : Frame} {H : Store}
    (h : FrameMatches D Γ φ H) : φ.In H := by
  refine ⟨h.store.mem_lt, fun ℓ hm => h.store.mem_lt ℓ ?_⟩
  rw [← h.record]; exact List.mem_reverse.mpr hm

/-- **No leak of drops: every owned value ends exactly once** (§7's
"no use-after-drop / no leak of drops" bullet, the "exactly once" half;
§6.7, §6.9, §6.10, §6.11). Take any well-typed configuration of a checked
program — an expression typed in `Γ`, run in a frame and store that agree
with `Γ` — whose program and expression are `pendingSafe`. Its evaluation is
never refused, and when it finishes normally or unwinds by `return` or
`break`:

* **every owned identity the store held at the start** is in exactly one
  place: in a cell that already existed, part of the result, or ended in the
  trace exactly as many times as it was held — by a drop, a discarded
  temporary, a residue drop, or a consumption (`Exact`);
* **every cell the evaluation allocated is retired** (`Tidy`): a `let`'s at
  its `endscope` (§6.7), a `match` arm's at the arm's end (§6.6), a callee's
  at its frame pop (§6.9) — except, for an unwinding `break`, the cells its
  scope record still owes, which the loop retires (§6.10) and whose values
  `rest_exactly_once` at the loop counts (`breakLeak_rejected`) — cells outside the
  frame's environment were touched only to be retired, and an unwinding
  `return` has retired the frame's whole record (§6.9's σ-walk). So "still in
  the store" means a cell the enclosing code can still reach, never one a
  frame pop forgot (`orphan_rejected`).

So a binding's value is dropped at its scope's end on the normal path
(`endscope`, §6.7) or by the σ-walk of an unwind (§6.9, §6.10) — never both,
since the two share one trace and one count, and never neither. "Exactly
once" presumes each starting identity is held once, which every store a run
reaches satisfies (`no_double_free`); the equation counts multiplicity in
general.

The values an evaluation *mints* are covered by its forms instead:
`rest_exactly_once` is the same ledger for the rest of a form once its
leading operands have produced their values.

The carve-outs, stated where they apply:

* **`@panic`** (§6.12): a trap carries no store and runs no drop, so `Exact`
  promises nothing about a `panic` result — the values live at the trap are
  abandoned, which is §5.7's `⊥_panic` edge;
* **RUE-2316**: a value computed for an earlier operand that a later operand
  abandons by `return` or `break` is dropped by nothing, in the calculus as in
  the compiler; `pendingSafe` excludes the shape, and
  `pendingSafe_needed` shows a checked program on which the conclusion fails
  without it. -/
theorem drop_exactly_once (M : FloatModel) {P : Program} (h : ProgramTyped P)
    (hp : P.pendingSafe = true) {fuel : Nat} {R : Ty} {Γ : Ctx} {e : Expr} {T : Ty} {Ω : Out}
    {φ : Frame} {H : Store} (ht : Typed P R Γ e T Ω) (hfm : FrameMatches P.decls Γ φ H)
    (hcc : StoreCC P.decls H) (he : e.pendingSafe = true) :
    (∀ w, eval M.toFloatOps fuel P H φ e ≠ .stuck w) ∧
      Exact P.decls H [] (eval M.toFloatOps fuel P H φ e) ∧
      Tidy φ H (eval M.toFloatOps fuel P H φ e) := by
  refine ⟨fun w hw => ?_, eval_exact M.toFloatOps hp fuel H φ e hcc he,
    eval_tidy M.toFloatOps P fuel H φ e hfm.frameIn⟩
  have := soundness M h.wf fuel ht hfm
  rw [hw] at this
  exact this

/-- Allocations since an earlier store include those since a later one
(helper). -/
theorem Retired.mono {H H₁ H' : Store} {keep : List Nat} (hle : H.length ≤ H₁.length)
    (h : Retired H keep H') : Retired H₁ keep H' :=
  fun ℓ h₁ h₂ hk => h ℓ (Nat.le_trans hle h₁) h₂ hk

/-- The whole form's frame-pop invariant, read at the rest (helper). -/
theorem Tidy.settled {φ : Frame} {H H₁ : Store} {r : EvalRes} {tr : List Event}
    (hle : H.length ≤ H₁.length) (h : Tidy φ H (r.withTrace tr)) : Settled φ H₁ r := by
  cases r with
  | ok => exact Retired.mono hle h.2
  | returned => exact ⟨Retired.mono hle h.2.1, h.2.2⟩
  | broke => exact ⟨Retired.mono hle h.2.1, h.2.2.elim fun locs h' => ⟨locs, h'.1⟩⟩
  | _ => trivial

/-- A form's leading operands ran from a copy-closed store: the store only
grew, it stays copy-closed, and the values are (helper). -/
theorem lead_cc (M : FloatOps) {P : Program} (hp : P.pendingSafe = true) {fuel : Nat}
    {H : Store} {φ : Frame} {e : Expr} {H₁ : Store} {vs : List Val} {tr : List Event}
    (hcc : StoreCC P.decls H) (he : e.pendingSafe = true) (hl : Lead M P fuel H φ H₁ vs tr e) :
    H.length ≤ H₁.length ∧ StoreCC P.decls H₁ ∧
      Contents.copyClosedList P.decls (Contents.ofVals vs) = true := by
  have single : ∀ e₁ v, e₁.pendingSafe = true → vs = [v] → eval M fuel P H φ e₁ = .ok H₁ v tr →
      H.length ≤ H₁.length ∧ StoreCC P.decls H₁ ∧
        Contents.copyClosedList P.decls (Contents.ofVals vs) = true := by
    intro e₁ v hps hv hr
    have := eval_exact M hp fuel H φ e₁ hcc hps
    rw [hr] at this
    subst hv
    exact ⟨this.1, this.2.1, by simp [Contents.ofVals, Contents.copyClosedList, this.2.2.1]⟩
  have list : ∀ args, Expr.pendingSafeList args = true → Expr.quietList args.tail = true →
      evalArgs (fun H' e => eval M fuel P H' φ e) H args = .ok H₁ vs tr →
      H.length ≤ H₁.length ∧ StoreCC P.decls H₁ ∧
        Contents.copyClosedList P.decls (Contents.ofVals vs) = true := by
    intro args hps hql hra
    have := evalArgs_exact (ev := fun H' e => eval M fuel P H' φ e) (es := args)
      (fun H' e' hm hc' => eval_exact M hp fuel H' φ e' hc' (Expr.pendingSafeList_mem hps hm))
      (fun H' e' hm => let q := Expr.quietList_mem hql hm
        ⟨(eval_quiet M P fuel H' φ e').1 q.1, (eval_quiet M P fuel H' φ e').2 q.2⟩) H hcc
    rw [hra] at this
    exact ⟨this.1, this.2.1, this.2.2.1⟩
  cases e with
  | intLit | floatLit | boolLit | unitLit | use | panic | drop | brk => exact hl.elim
  | loop e₁ =>
      obtain ⟨sc, rfl, hr⟩ := hl
      have := eval_exact M hp fuel H φ e₁ hcc (by simpa [Expr.pendingSafe] using he)
      rw [hr] at this
      exact ⟨this.1, this.2.1, rfl⟩
  | mkStruct _ args | mkEnum _ _ args | mkArray _ args | call _ args | indexRead _ args _ =>
      simp only [Expr.pendingSafe, Bool.and_eq_true] at he
      exact list args he.1 he.2 hl
  | indexDrop p idx πs =>
      obtain ⟨v, hv, hr⟩ := hl
      exact single _ v (by simpa [Expr.pendingSafe] using he) hv hr
  | unop _ e₁ | intCast _ _ e₁ | fintrin _ e₁ | dbg e₁ | repeatArray _ e₁ _ | assign _ e₁
  | ret e₁ =>
      obtain ⟨v, hv, hr⟩ := hl
      exact single e₁ v (by simpa [Expr.pendingSafe] using he) hv hr
  | binop _ e₁ _ | indexWrite _ _ _ e₁ | ite e₁ _ _ =>
      obtain ⟨v, hv, hr⟩ := hl
      simp only [Expr.pendingSafe, Bool.and_eq_true] at he
      exact single e₁ v he.1.1 hv hr
  | letIn _ e₁ _ | seq e₁ _ | «match» e₁ _ =>
      obtain ⟨v, hv, hr⟩ := hl
      simp only [Expr.pendingSafe, Bool.and_eq_true] at he
      exact single e₁ v he.1 hv hr

/-- **Values minted inside an evaluation end exactly once too**: the rest of
every form keeps the ledger (§6.7, §6.9, §6.10, §6.11, §7). Take a
well-typed configuration of a checked, `pendingSafe` program, and a form
whose leading operands — its first operand, or its argument list for a call,
a literal or a dynamic read — have produced the values `vs` in store `H₁`
after trace `tr` (`Lead`). Whatever the rest of the form yields, `r` with
`eval … e = r.withTrace tr`, is not a refusal, and every owned identity of
`vs` or of `H₁` is, at its end, in a cell that already existed, in the result,
or ended in `r`'s trace exactly as many times as it was held; every cell
allocated since `H₁` is retired (`Settled`). This is where a `let`'s
initializer is dropped at the `endscope`, a discarded `S { .. };` at the
`dropTemp`, an argument at the callee's frame pop, and a scrutinee's shell at
its `consume`, and where a `break` unwinds the bindings its loop body still
held — the loop's lead is its body breaking, so the unwind is that loop's rest
(`breakLeak_rejected`) — values `drop_exactly_once` alone never sees, because
no evaluation starts holding them (`letDropDeleted_rejected`). Every place the
machine ends an owned value — an `endscope`, a discard, a frame pop, a
`return`'s σ-walk, a `break`'s unwind, a consumption, an overwrite or `@drop`
— lies inside the window of a statement that already counts the value: the
rest of the form that bound or received it, the rest of the loop a `break`
unwinds to, or an evaluation that started holding it. So the two theorems
together say every owned value a checked run holds is ended at most once in
each window, exactly once by the end of the window that holds it, and never
left in a cell nobody can reach.

What the two do not see is an end emitted *early*, inside the evaluation
that minted the value: no window holds the value yet, so only
`no_double_free`'s "at most once" bounds such an end, and `main`'s own result
is part of `run`'s result, handed to no form. -/
theorem rest_exactly_once (M : FloatModel) {P : Program} (h : ProgramTyped P)
    (hp : P.pendingSafe = true) {fuel : Nat} {R : Ty} {Γ : Ctx} {e : Expr} {T : Ty} {Ω : Out}
    {φ : Frame} {H : Store} (ht : Typed P R Γ e T Ω) (hfm : FrameMatches P.decls Γ φ H)
    (hcc : StoreCC P.decls H) (he : e.pendingSafe = true)
    {H₁ : Store} {vs : List Val} {tr : List Event} (hl : Lead M.toFloatOps P fuel H φ H₁ vs tr e)
    {r : EvalRes} (hr : eval M.toFloatOps (fuel + 1) P H φ e = r.withTrace tr) :
    (∀ w, r ≠ .stuck w) ∧
      Exact P.decls H₁ (Contents.ownList P.decls (Contents.ofVals vs)) r ∧ Settled φ H₁ r := by
  obtain ⟨hle, hc₁, hvs⟩ := lead_cc M.toFloatOps hp hcc he hl
  refine ⟨fun w hw => ?_, rest_step M.toFloatOps hp
      (fun H' φ' e' hc hps => eval_exact M.toFloatOps hp fuel H' φ' e' hc hps) he hl hc₁ hvs r hr,
    Tidy.settled hle (hr ▸ eval_tidy M.toFloatOps P (fuel + 1) H φ e hfm.frameIn)⟩
  have := soundness M h.wf (fuel + 1) ht hfm
  rw [hr, hw] at this
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

/-- **The RUE-2316 carve-out is load-bearing** (§6.9's (D-Return), §7): at a
typed configuration of a checked program that is not `pendingSafe`, the exact
ledger fails — `x`'s
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

/-! ## What the statements reject, at typed configurations

Four results a buggy machine could produce. Each is accepted by the bare
start-of-evaluation ledger `Exact`; each is rejected by what
`drop_exactly_once` or `rest_exactly_once` concludes at a typed configuration
of a checked, `pendingSafe` program — where the real run, by the same
theorems, satisfies the conclusion.

* **An orphaned parameter** (first review, probe 5): `g(x)` with `g` ignoring
  its parameter. A frame pop that forgot the σ-walk leaves `g`'s parameter
  cell full and emits no drop. The ledger still balances — the `S0` is "still
  in the store" — and `Tidy` rejects it.
* **A deleted drop of a minted value** (first review, probe 4):
  `let x = S0 { 1 }; 0` and `S0 { 1 }; 0`. The `S0` is minted inside the
  evaluation, so no evaluation starts holding it; `rest_exactly_once`'s ledger,
  taken where the initializer has produced it, rejects the deletion.
* **A silent `break` unwind** (second review, probe B1): in
  `loop { let z = S0 { 1 }; break; }`, a loop that retires `z`'s cell without
  dropping its `S0`. `Tidy` is satisfied — the cell is retired — and the `S0`
  was minted after the loop started; `rest_exactly_once` at the loop, whose
  lead is its body breaking, rejects it. -/

/-- A checker verdict at a type is a typing derivation (helper). -/
theorem typed_of_check {P : Program} {R : Ty} {Γ : Ctx} {e : Expr} (T : Ty)
    (h : (check P R Γ e).any (fun p => p.1.fits T) = true) : ∃ Ω, Typed P R Γ e T Ω := by
  cases hc : check P R Γ e with
  | none => simp [hc] at h
  | some p => obtain ⟨c, Ω⟩ := p; simp [hc] at h; exact ⟨Ω, check_sound e hc T h⟩

/-- `g`'s entry frame agrees with its entry context (helper). -/
theorem lostFrame_matches : FrameMatches lostDecls lostCtx lostFrame lostStore :=
  ⟨.cons (by rfl) ⟨_, rfl, ContentsMatches.ofVal (v := .struct 0 0 [.int .w64 .signed 7])
    (HasTy.struct (by rfl) (.cons (.int (w := .w64) (s := .signed) (n := 7) (by decide)) .nil))⟩
    (by simp) .nil, rfl⟩

/-- `g`'s entry store is copy-closed (helper). -/
theorem lostStore_cc : StoreCC lostDecls lostStore := by
  intro ℓ c hc
  match ℓ, hc with
  | 0, hc => simp [lostStore] at hc
  | 1, hc => simp [lostStore] at hc; subst hc; rfl
  | _ + 2, hc => simp [lostStore] at hc

/-- `g` ignores its `S0` parameter (helper). -/
def orphanProgram : Program :=
  { decls := lostDecls,
    fns := [{ params := [], ret := .int .w64 .signed, body := .intLit .w64 .signed 0 },
            { params := [⟨.struct 0, false⟩], ret := .int .w64 .signed,
              body := .intLit .w64 .signed 0 }] }

/-- `x`'s `S0` (helper). -/
def s0x : Contents := .struct 0 0 [.int .w64 .signed 7]

/-- The result of a frame pop that forgot its σ-walk: `g`'s parameter cell
`ℓ2` still full, no drop, the destructor event alone (helper). -/
def orphanResult : EvalRes :=
  .ok [.dead, .full .hole, .full s0x] (.int .w64 .signed 0) [.dtor 0 s0x]

/-- **An orphaned cell balances the ledger but breaks `Tidy`** (§6.9). At the
typed configuration `g(x)` of a checked, `pendingSafe` program, the real run
drops `x`'s `S0` at `g`'s frame pop and retires the cell, and satisfies
`drop_exactly_once`; the orphaned result keeps `Exact` yet fails `Tidy`,
which `drop_exactly_once` concludes. -/
theorem orphan_rejected (M : FloatModel) :
    ProgramTyped orphanProgram ∧ orphanProgram.pendingSafe = true ∧
      (∃ Ω, Typed orphanProgram (.int .w64 .signed) lostCtx (.call 1 [.use (.var 0)])
        (.int .w64 .signed) Ω) ∧
      eval M.toFloatOps 100 orphanProgram lostStore lostFrame (.call 1 [.use (.var 0)])
        = .ok [.dead, .full .hole, .dead] (.int .w64 .signed 0) [.drop 2 s0x, .dtor 0 s0x] ∧
      Tidy lostFrame lostStore
        (eval M.toFloatOps 100 orphanProgram lostStore lostFrame (.call 1 [.use (.var 0)])) ∧
      Exact lostDecls lostStore [] orphanResult ∧ ¬ Tidy lostFrame lostStore orphanResult := by
  have hP : ProgramTyped orphanProgram := checkProgram_sound (by rfl)
  obtain ⟨Ω, ht⟩ := typed_of_check (P := orphanProgram) (R := .int .w64 .signed) (Γ := lostCtx)
    (e := .call 1 [.use (.var 0)]) (.int .w64 .signed) (by rfl)
  refine ⟨hP, by rfl, ⟨Ω, ht⟩, by rfl,
    (drop_exactly_once M hP (by rfl) ht lostFrame_matches lostStore_cc (by rfl)).2.2,
    ⟨by decide, fun ℓ c hc => ?_, rfl, fun a ha => ?_⟩, fun ⟨_, hret⟩ => ?_⟩
  · match ℓ, hc with
    | 0, hc => simp at hc
    | 1, hc => simp at hc; subst hc; rfl
    | 2, hc => simp at hc; subst hc; rfl
    | _ + 3, hc => simp at hc
  · match a, ha with
    | 0, _ => rfl
    | 1, _ => rfl
    | _ + 2, ha => exact absurd ha (by simp [lostStore])
  · have := hret 2 (by decide) (by decide) (by simp)
    simp at this

/-- The minted `S0 { 1 }` (helper). -/
def s0one : Val := .struct 0 0 [.int .w64 .signed 1]

/-- The empty frame agrees with the empty context over the empty store
(helper). -/
theorem emptyStore_cc : StoreCC lostDecls [] := fun ℓ c hc => by simp at hc

/-- **A deleted `let` drop passes the bare ledger but not the form's** (§6.7):
`let x = S0 { 1 }; 0`, a typed configuration of a checked, `pendingSafe`
program, from the empty store. The whole evaluation's ledger counts no
starting identity, so it accepts the result with the `endscope`'s drop
deleted; `rest_exactly_once`'s ledger — from the store the initializer left,
holding the minted `S0` — holds of the real rest and rejects the deletion. -/
theorem letDropDeleted_rejected (M : FloatModel) :
    ProgramTyped orphanProgram ∧
      (∃ Ω, Typed orphanProgram (.int .w64 .signed) []
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 1]) (.intLit .w64 .signed 0))
        (.int .w64 .signed) Ω) ∧
      Lead M.toFloatOps orphanProgram 100 [] { env := [], scope := [] } [.dead] [s0one] []
        (.letIn false (.mkStruct 0 [.intLit .w64 .signed 1]) (.intLit .w64 .signed 0)) ∧
      eval M.toFloatOps 101 orphanProgram [] { env := [], scope := [] }
          (.letIn false (.mkStruct 0 [.intLit .w64 .signed 1]) (.intLit .w64 .signed 0))
        = .ok [.dead, .dead] (.int .w64 .signed 0)
            [.drop 1 (Contents.ofVal s0one), .dtor 0 (Contents.ofVal s0one)] ∧
      Exact lostDecls [.dead] (Contents.ownList lostDecls (Contents.ofVals [s0one]))
          (.ok [.dead, .dead] (.int .w64 .signed 0)
            [.drop 1 (Contents.ofVal s0one), .dtor 0 (Contents.ofVal s0one)]) ∧
      Exact lostDecls [] [] (.ok [.dead, .dead] (.int .w64 .signed 0) [.dtor 0 (Contents.ofVal s0one)]) ∧
      ¬ Exact lostDecls [.dead] (Contents.ownList lostDecls (Contents.ofVals [s0one]))
          (.ok [.dead, .dead] (.int .w64 .signed 0) [.dtor 0 (Contents.ofVal s0one)]) := by
  have hP : ProgramTyped orphanProgram := checkProgram_sound (by rfl)
  obtain ⟨Ω, ht⟩ := typed_of_check (P := orphanProgram) (R := .int .w64 .signed) (Γ := [])
    (e := .letIn false (.mkStruct 0 [.intLit .w64 .signed 1]) (.intLit .w64 .signed 0))
    (.int .w64 .signed) (by rfl)
  have hl : Lead M.toFloatOps orphanProgram 100 [] { env := [], scope := [] } [.dead] [s0one] []
      (.letIn false (.mkStruct 0 [.intLit .w64 .signed 1]) (.intLit .w64 .signed 0)) :=
    ⟨s0one, rfl, by rfl⟩
  have real := (rest_exactly_once M hP (by rfl) ht frameMatches_empty emptyStore_cc (by rfl) hl
    (r := .ok [.dead, .dead] (.int .w64 .signed 0)
      [.drop 1 (Contents.ofVal s0one), .dtor 0 (Contents.ofVal s0one)]) (by rfl)).2.1
  refine ⟨hP, ⟨Ω, ht⟩, hl, by rfl, real, ⟨by decide, fun ℓ c hc => ?_, rfl, fun a ha => ?_⟩,
    fun ⟨_, _, _, h4⟩ => ?_⟩
  · match ℓ, hc with
    | 0, hc => simp at hc
    | 1, hc => simp at hc
    | _ + 2, hc => simp at hc
  · exact absurd ha (Nat.not_lt_zero a)
  · have := h4 0 (by decide)
    exact absurd this (by decide)

/-- **The same for a discarded temporary** (§6.7's (D-Seq)): `S0 { 1 }; 0`
with its `dropTemp` deleted is rejected by `rest_exactly_once`'s ledger, which
holds of the real rest. -/
theorem seqDropDeleted_rejected (M : FloatModel) :
    (∃ Ω, Typed orphanProgram (.int .w64 .signed) []
        (.seq (.mkStruct 0 [.intLit .w64 .signed 1]) (.intLit .w64 .signed 0))
        (.int .w64 .signed) Ω) ∧
      Lead M.toFloatOps orphanProgram 100 [] { env := [], scope := [] } [.dead] [s0one] []
        (.seq (.mkStruct 0 [.intLit .w64 .signed 1]) (.intLit .w64 .signed 0)) ∧
      eval M.toFloatOps 101 orphanProgram [] { env := [], scope := [] }
          (.seq (.mkStruct 0 [.intLit .w64 .signed 1]) (.intLit .w64 .signed 0))
        = .ok [.dead] (.int .w64 .signed 0) [.dropTemp s0one, .dtor 0 (Contents.ofVal s0one)] ∧
      Exact lostDecls [.dead] (Contents.ownList lostDecls (Contents.ofVals [s0one]))
          (.ok [.dead] (.int .w64 .signed 0) [.dropTemp s0one, .dtor 0 (Contents.ofVal s0one)]) ∧
      ¬ Exact lostDecls [.dead] (Contents.ownList lostDecls (Contents.ofVals [s0one]))
          (.ok [.dead] (.int .w64 .signed 0) [.dtor 0 (Contents.ofVal s0one)]) := by
  have hP : ProgramTyped orphanProgram := checkProgram_sound (by rfl)
  obtain ⟨Ω, ht⟩ := typed_of_check (P := orphanProgram) (R := .int .w64 .signed) (Γ := [])
    (e := .seq (.mkStruct 0 [.intLit .w64 .signed 1]) (.intLit .w64 .signed 0))
    (.int .w64 .signed) (by rfl)
  have hl : Lead M.toFloatOps orphanProgram 100 [] { env := [], scope := [] } [.dead] [s0one] []
      (.seq (.mkStruct 0 [.intLit .w64 .signed 1]) (.intLit .w64 .signed 0)) :=
    ⟨s0one, rfl, by rfl⟩
  have real := (rest_exactly_once M hP (by rfl) ht frameMatches_empty emptyStore_cc (by rfl) hl
    (r := .ok [.dead] (.int .w64 .signed 0) [.dropTemp s0one, .dtor 0 (Contents.ofVal s0one)])
    (by rfl)).2.1
  refine ⟨⟨Ω, ht⟩, hl, by rfl, real, fun ⟨_, _, _, h4⟩ => ?_⟩
  have := h4 0 (by decide)
  exact absurd this (by decide)

/-- `g(x: S0) { loop { let z = S0 { 1 }; break; }; @drop(x); 0 }`, called with
an `S0` (helper). -/
def breakProgram : Program :=
  { decls := lostDecls,
    fns := [{ params := [], ret := .int .w64 .signed,
              body := .call 1 [.mkStruct 0 [.intLit .w64 .signed 7]] },
            { params := [⟨.struct 0, false⟩], ret := .int .w64 .signed,
              body := .seq (.loop (.letIn false (.mkStruct 0 [.intLit .w64 .signed 1]) .brk))
                (.seq (.drop (.var 0)) (.intLit .w64 .signed 0)) }] }

/-- `g`'s loop (helper). -/
def breakLoop : Expr := .loop (.letIn false (.mkStruct 0 [.intLit .w64 .signed 1]) .brk)

/-- `z`'s `S0` (helper). -/
def s0z : Contents := .struct 0 2 [.int .w64 .signed 1]

/-- The store the loop body broke in: `z` still bound in `ℓ3` (helper). -/
def breakStore : Store := [.dead, .full s0x, .dead, .full s0z]

/-- A loop whose `break` unwind retires `z`'s cell with no drop and no
destructor (helper). -/
def breakLeak : EvalRes := .ok [.dead, .full s0x, .dead, .dead] .unit []

/-- **A silent `break` unwind of a body-minted value** (§6.10's (D-Break)):
`g`'s loop is a typed configuration of a checked, `pendingSafe` program. The
real loop drop-retires `z`'s cell. A loop that retires it silently satisfies
the bare ledger and `Tidy` — `z`'s `S0` was minted after the loop started, and
its cell is retired — and is rejected by `rest_exactly_once` at the loop, whose
lead is the body breaking in `breakStore`; the real unwind satisfies it. -/
theorem breakLeak_rejected (M : FloatModel) :
    ProgramTyped breakProgram ∧ breakProgram.pendingSafe = true ∧
      (∃ Ω, Typed breakProgram (.int .w64 .signed) lostCtx breakLoop .unit Ω) ∧
      Lead M.toFloatOps breakProgram 100 lostStore lostFrame breakStore [] [] breakLoop ∧
      eval M.toFloatOps 101 breakProgram lostStore lostFrame breakLoop
        = .ok [.dead, .full s0x, .dead, .dead] .unit [.drop 3 s0z, .dtor 0 s0z] ∧
      Exact lostDecls breakStore []
        (.ok [.dead, .full s0x, .dead, .dead] .unit [.drop 3 s0z, .dtor 0 s0z]) ∧
      Exact lostDecls lostStore [] breakLeak ∧ Tidy lostFrame lostStore breakLeak ∧
      ¬ Exact lostDecls breakStore [] breakLeak := by
  have hP : ProgramTyped breakProgram := checkProgram_sound (by rfl)
  obtain ⟨Ω, ht⟩ := typed_of_check (P := breakProgram) (R := .int .w64 .signed) (Γ := lostCtx)
    (e := breakLoop) .unit (by rfl)
  have hl : Lead M.toFloatOps breakProgram 100 lostStore lostFrame breakStore [] [] breakLoop :=
    ⟨[1, 3], rfl, by rfl⟩
  have real := (rest_exactly_once M hP (by rfl) ht lostFrame_matches lostStore_cc (by rfl) hl
    (r := .ok [.dead, .full s0x, .dead, .dead] .unit [.drop 3 s0z, .dtor 0 s0z]) (by rfl)).2.1
  refine ⟨hP, by rfl, ⟨Ω, ht⟩, hl, by rfl, real,
    ⟨by decide, fun ℓ c hc => ?_, rfl, fun a ha => ?_⟩,
    ⟨⟨by decide, fun ℓ hl' _ => ?_⟩, fun ℓ h₁ h₂ _ => ?_⟩, fun ⟨_, _, _, h4⟩ => ?_⟩
  · match ℓ, hc with
    | 0, hc => simp at hc
    | 1, hc => simp at hc; subst hc; rfl
    | 2, hc => simp at hc
    | 3, hc => simp at hc
    | _ + 4, hc => simp at hc
  · match a, ha with
    | 0, _ => rfl
    | 1, _ => rfl
    | _ + 2, ha => exact absurd ha (by simp [lostStore])
  · match ℓ, hl' with
    | 0, _ => exact .inl rfl
    | 1, _ => exact .inl rfl
    | _ + 2, hl' => exact absurd hl' (by simp [lostStore])
  · simp [lostStore] at h₁; simp at h₂
    match ℓ, h₁, h₂ with
    | 2, _, _ => rfl
    | 3, _, _ => rfl
  · have := h4 2 (by decide)
    exact absurd this (by decide)

end RueCore
