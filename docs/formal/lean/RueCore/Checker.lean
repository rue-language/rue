import RueCore.Soundness

/-!
# RueCore.Checker — a decidable, verified checker for the §5 rules

The `Typed` judgment is syntax-directed, so it has a computable image:
`check P R Γ e` either produces `(T, Γ')` or rejects. `check_sound` proves
every acceptance is backed by a real derivation — so the §7 safety theorems
apply to anything `check` accepts. This is the seed of the "second,
independent implementation" purpose of the formal core
(`docs/formal/README.md`): a verified reference for what the compiler's
semantic phase must accept.

`checkProgram` lifts it to a whole program: §3's class assignment holds of
every struct declaration (`checkStructs`), every function body checks at its
declared return type under (Fn) §5.8's entry context, its normal exit edge
discharges §5.6's obligation, and the entry point takes no parameters. Its
soundness lemma produces the `ProgramTyped` hypothesis `Soundness.lean`'s
program theorems ask for.

`checkStructs` is what makes `Ty.mult`'s lookup honest: a declaration
*records* `class(S)`, and this pass is the equation §3 writes for it, together
with `3.8:18`/`3.9:31`'s `@copy` restriction, `3.9:44`'s destructor
restriction, and the acyclicity that makes the equation solvable in one pass
(`struct_class_unique`).

## `return` and `@panic`, algorithmically

§5.7 types `return e` at `never` and (Sub-Never) coerces it to whatever the
context needs, with a divergent outgoing state `⊥` that a join reads nothing
from; (Panic) §5.8 gives `@panic(s)` the same treatment. `Typed.ret` and
`Typed.panic` fold both in by concluding at *any* type and *any*
same-skeleton outgoing context, so an algorithm has to pick. `check` picks the
enclosing function's return type `R` and the state in force at the form — for
`return`, after the operand; for `@panic`, the incoming state, since the
message is a literal the form carries. That is the choice that makes the
shapes the fragment writes go through: a body that ends in `return` or
`@panic`, and an `if` whose arms are one of those and a value of the
function's return type.

The paragraphs below spell the cost out for `return`, and `@panic` carries
the same two: `1 + @panic("x")` inside a `bool`-returning function has a
derivation and `check` rejects it, and a `@panic` arm of an `if` contributes
its incoming state to §5.5's join where §5.7 excludes it. `@panic` carries a
**third** that `return` does not, and it is the shape `Typed.panic`'s missing
residual-linear premise exists for; the state paragraph below names it. The
generator emits neither form (`Gen.lean`).

That choice is a *restriction* of the rule, so `check_sound` still holds, and
it is where completeness is lost. Both halves of the choice cost something,
and the second costs more than the first.

The **type** choice: `1 + return true` inside a `bool`-returning function has
a derivation and `check` rejects it, because the algorithm never re-types a
`return` at the type its context wants. Contrived, and no program a reader
would write.

The **state** choice: §5.7 gives a diverging arm the outgoing state `⊥`, which
§5.5's join reads *nothing* from. `check` hands the join the state in force
after the operand instead, so a `return` arm does contribute — conservatively
— and a binding that arm moved out is `MovedOut` after the `if`, hence
unusable. `main() -> int { let x = mk 5; (if c { @drop(x); return 0 } else { 5 }); consume(x) }`
is the shape: `Typed` derives it (the `ret` rule may take the other arm's
outgoing context), the machine runs it, the Rue compiler accepts it, and
`check` rejects it. That is a program a reader would write, so the rejection
is not merely incomplete — it is *wrong* about the program, and anything that
reads a `reject` verdict as "the compiler must reject this too"
(`Corpus.lean`'s verdict contract) must not be handed that shape.

The state choice costs `@panic` one shape more, and it is the only one where
(Panic) and (Return-Value) differ at all: a `@panic` past a **live linear**
binding. `return` there would fail §5.6's frame-wide obligation, which
`Typed.ret` carries as a premise; `@panic` carries `⊥_panic`, which §5.7
exempts, so `Typed.panic` has no such premise and the judgment derives the
program. `check` hands the enclosing `let` the state in force at the form
instead of `⊥`, sees the binding still `Owned` at a `Linear` type, and
refuses. The Rue compiler accepts it, runs it, and does not run the
destructor either — `Examples.panicPastLinear` is the program, with the
derivation, the rejection and the run all pinned. The same shape in one arm
of an `if`, and as a sibling of a linear call argument, behave the same way.
At an **affine** binding there is nothing to see: `Typed.letIn`'s premise is
already vacuous at a non-linear type, so `panic` and `ret` agree.

So completeness — `Typed` implies `check` succeeds — is not open here: it is
**false**, and the counterexamples above are why. What is deferred is a
`check` that carries §5.7's ⊥ provenance (a `div` flag on the result, excluded
from the join) and closes both; until then the fragment's corpus and generator
stay off the shapes it gets wrong (`Corpus.lean`, `Gen.lean`).
-/

namespace RueCore

mutual
/-- The §5 judgment as an algorithm: one case per `Typed` rule, in the same
order, producing the type and outgoing context or rejecting. `P` is the
top-level function environment (Call) §5.8 looks a callee up in and `R` the
enclosing function's declared return type (Return-Value) §5.7 checks a
`return` operand against. -/
def check (P : Program) (R : Ty) (Γ : Ctx) : Expr → Option (Ty × Ctx)
  | .intLit w s n => if InBounds w s n then some (.int w s, Γ) else none
  | .boolLit _ => some (.bool, Γ)
  | .unitLit => some (.unit, Γ)
  | .use p =>
      match Γ[p.root]? with
      | none => none
      | some en =>
        match en.st.get p.path, en.ty.atPath P.structs p.path with
        | some u, some T =>
            if noLinearPrefix P.structs en.ty p.path then
              if T.mult P.structs = .copy then
                (if u.fullyOwned then some (T, Γ) else none)
              else
                (if u.fullyOwned ∧ noDtorPrefix P.structs en.ty p.path then
                   some (T, Γ.set p.root (en.setSt (en.st.setAt p.path .movedOut)))
                 else none)
            else none
        | _, _ => none
  | .binop op e₁ e₂ =>
      match check P R Γ e₁ with
      | some (.int w s, Γ₁) =>
        (match check P R Γ₁ e₂ with
        | some (.int w' s', Γ₂) =>
            if w' = w ∧ s' = s ∧ op.intAdmits = true then some (op.resultTy (.int w s), Γ₂)
            else none
        | _ => none)
      | some (.float w, Γ₁) =>
        (match check P R Γ₁ e₂ with
        | some (.float w', Γ₂) =>
            if w' = w ∧ op.floatAdmits = true then some (op.resultTy (.float w), Γ₂) else none
        | _ => none)
      | _ => none
  | .floatLit w l => if l.RoundsFinite w then some (.float w, Γ) else none
  | .fintrin (.intToFloat w) e =>
      match check P R Γ e with
      | some (.int _ _, Γ') => some (.float w, Γ')
      | _ => none
  | .fintrin k e =>
      match check P R Γ e with
      | some (.float w, Γ') => if k.floatSrc w then some (k.resTy w, Γ') else none
      | _ => none
  | .unop .neg e =>
      match check P R Γ e with
      | some (.int w .signed, Γ') => some (.int w .signed, Γ')
      | some (.float w, Γ') => some (.float w, Γ')
      | _ => none
  | .unop .not e =>
      match check P R Γ e with
      | some (.bool, Γ') => some (.bool, Γ')
      | _ => none
  | .unop .bitnot e =>
      match check P R Γ e with
      | some (.int w s, Γ') => some (.int w s, Γ')
      | _ => none
  | .intCast w s e =>
      match check P R Γ e with
      | some (.int _ _, Γ') => some (.int w s, Γ')
      | _ => none
  | .panic _ => some (R, Γ)
  | .dbg e =>
      match check P R Γ e with
      | some (T, Γ') => if T.observable then some (.unit, Γ') else none
      | none => none
  | .mkStruct s args =>
      match P.structs[s]? with
      | none => none
      | some sd =>
        match checkArgs P R Γ args sd.fields with
        | some Γ' => some (.struct s, Γ')
        | none => none
  | .drop p =>
      match Γ[p.root]? with
      | none => none
      | some en =>
        match en.st.get p.path, en.ty.atPath P.structs p.path with
        | some u, some T =>
            if noLinearPrefix P.structs en.ty p.path then
              if T.mult P.structs = .copy then
                (if u.fullyOwned then some (.unit, Γ) else none)
              else
                (if u.isOwned ∧ noDtorPrefix P.structs en.ty p.path ∧
                    (u.fullyOwned = true ∨ residualLinearBelow P.structs u T = false) then
                   some (.unit, Γ.set p.root (en.setSt (en.st.setAt p.path .movedOut)))
                 else none)
            else none
        | _, _ => none
  | .letIn m e₁ e₂ =>
      match check P R Γ e₁ with
      | none => none
      | some (T₁, Γ₁) =>
        match check P R ({ ty := T₁, mu := m, st := .owned } :: Γ₁) e₂ with
        | some (T₂, en' :: Γ₂) =>
            if residualLinear P.structs en'.st en'.ty then none else some (T₂, Γ₂)
        | _ => none
  | .assign p e =>
      match Γ[p.root]? with
      | none => none
      | some en₀ =>
        if en₀.mu = true then
          match en₀.st.get p.path, en₀.ty.atPath P.structs p.path with
          | some _, some T =>
            (match check P R Γ e with
             | some (T', Γ₁) =>
               if T' = T then
                 (match Γ₁[p.root]? with
                  | some en₁ =>
                    (match en₁.st.get p.path with
                     | some u₁ =>
                         if residualLinear P.structs u₁ T then none
                         else some (.unit,
                           Γ₁.set p.root (en₁.setSt (en₁.st.setAt p.path .owned)))
                     | none => none)
                  | none => none)
               else none
             | none => none)
          | _, _ => none
        else none
  | .seq e₁ e₂ =>
      match check P R Γ e₁ with
      | some (T₁, Γ₁) =>
          if T₁.mult P.structs = .linear then none
          else check P R Γ₁ e₂
      | none => none
  | .ite c e₁ e₂ =>
      match check P R Γ c with
      | some (.bool, Γ₀) =>
        (match check P R Γ₀ e₁, check P R Γ₀ e₂ with
        | some (T₁, Γ₁), some (T₂, Γ₂) =>
            if T₁ = T₂ then
              match Ctx.join P.structs Γ₁ Γ₂ with
              | some Γ' => some (T₁, Γ')
              | none => none
            else none
        | _, _ => none)
      | _ => none
  | .call f args =>
      match P.fns[f]? with
      | none => none
      | some fd =>
        match checkArgs P R Γ args (fd.params.map Param.ty) with
        | some Γ' => some (fd.ret, Γ')
        | none => none
  | .ret e =>
      match check P R Γ e with
      | none => none
      | some (T, Γ₁) =>
          if T = R ∧ NoResidualLinear P.structs Γ₁ then some (R, Γ₁) else none

/-- (Call) §5.8's argument list as an algorithm: each argument is checked
against its parameter's type with Σ threaded left to right, and the count must
match (`4.10:3`, `4.10:4`). -/
def checkArgs (P : Program) (R : Ty) : Ctx → List Expr → List Ty → Option Ctx
  | Γ, [], [] => some Γ
  | Γ, e :: es, T :: Ts =>
      match check P R Γ e with
      | some (T', Γ₁) => if T' = T then checkArgs P R Γ₁ es Ts else none
      | none => none
  | _, _, _ => none
end

mutual
/-- Every `check` acceptance is a real derivation of the §5 judgment, so the
§7 theorems apply to whatever `check` accepts. -/
theorem check_sound {P : Program} {R : Ty} : ∀ (e : Expr) {Γ : Ctx} {T Γ'},
    check P R Γ e = some (T, Γ') → Typed P R Γ e T Γ'
  | .intLit w s n, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · cases h; exact .intLit ‹_›
      · cases h
  | .boolLit b, Γ, T, Γ', h => by
      simp only [check] at h; cases h; exact .boolLit
  | .unitLit, Γ, T, Γ', h => by
      simp only [check] at h; cases h; exact .unitLit
  | .use pl, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · cases h
      · rename_i en hen
        split at h
        · rename_i u T₀ hg hty
          split at h
          · rename_i hlin
            split at h
            · rename_i hcopy
              split at h
              · rename_i hfo
                cases h
                exact .useCopy hen hg hfo hty hcopy hlin
              · cases h
            · rename_i hncopy
              split at h
              · rename_i hprem
                cases h
                exact .useMove hen hg hprem.1 hty hncopy hprem.2 hlin
              · cases h
          · cases h
        · cases h
  | .binop op e₁ e₂, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · rename_i w s Γ₁ h₁
        split at h
        · rename_i w' s' Γ₂ h₂
          split at h
          · rename_i hws
            obtain ⟨hw, hs, hadm⟩ := hws
            subst hw; subst hs
            cases h
            exact .binop (check_sound e₁ h₁) (check_sound e₂ h₂) hadm
          · cases h
        · cases h
      · rename_i w Γ₁ h₁
        split at h
        · rename_i w' Γ₂ h₂
          split at h
          · rename_i hws
            obtain ⟨hw, hadm⟩ := hws
            subst hw
            cases h
            exact .floatBinop (check_sound e₁ h₁) (check_sound e₂ h₂) hadm
          · cases h
        · cases h
      · cases h
  | .floatLit w l, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · cases h; exact .floatLit ‹_›
      · cases h
  | .fintrin (.intToFloat w) e, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · cases h; exact .intToFloat (check_sound e ‹_›)
      · cases h
  | .fintrin (.floatToInt w s) e, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · split at h
        · cases h; exact .floatIntrin (check_sound e ‹_›) (by simpa using ‹_›)
        · cases h
      · cases h
  | .fintrin (.floatCast w) e, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · split at h
        · cases h; exact .floatIntrin (check_sound e ‹_›) (by simpa using ‹_›)
        · cases h
      · cases h
  | .fintrin (.roundOp k) e, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · split at h
        · cases h; exact .floatIntrin (check_sound e ‹_›) (by simpa using ‹_›)
        · cases h
      · cases h
  | .unop .neg e, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · cases h; exact .neg (check_sound e ‹_›)
      · cases h; exact .floatNeg (check_sound e ‹_›)
      · cases h
  | .unop .not e, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · cases h; exact .notOp (check_sound e ‹_›)
      · cases h
  | .unop .bitnot e, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · cases h; exact .bitnot (check_sound e ‹_›)
      · cases h
  | .intCast w s e, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · cases h; exact .intCast (check_sound e ‹_›)
      · cases h
  | .panic msg, Γ, T, Γ', h => by
      simp only [check] at h
      cases h
      exact .panic rfl
  | .dbg e, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · rename_i T₁ Γ₁ h₁
        split at h
        · cases h; exact .dbg (check_sound e h₁) ‹_›
        · cases h
      · cases h
  | .mkStruct s args, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · cases h
      · rename_i sd hsd
        split at h
        · rename_i Γ₁ hargs
          cases h
          exact .mkStruct hsd (checkArgs_sound args hargs)
        · cases h
  | .drop pl, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · cases h
      · rename_i en hen
        split at h
        · rename_i u T₀ hg hty
          split at h
          · rename_i hlin
            split at h
            · rename_i hcopy
              split at h
              · rename_i hfo
                cases h
                exact .dropCopy hen hg hfo hty hcopy hlin
              · cases h
            · rename_i hncopy
              split at h
              · rename_i hprem
                cases h
                exact .dropRes hen hg hprem.1 hty hncopy hprem.2.1 hlin hprem.2.2
              · cases h
          · cases h
        · cases h
  | .letIn m e₁ e₂, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · cases h
      · split at h
        · split at h
          · cases h
          · cases h
            exact .letIn (check_sound e₁ ‹_›) (check_sound e₂ ‹_›)
              ((Bool.not_eq_true _).mp ‹_›)
        · cases h
  | .assign pl e, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · cases h
      · rename_i en₀ hget₀
        split at h
        · rename_i hmu
          split at h
          · rename_i u₀ T₀ hg₀ hty₀
            split at h
            · rename_i T' Γ₁ hchk
              split at h
              · rename_i hT
                subst hT
                split at h
                · rename_i en₁ hget₁
                  split at h
                  · rename_i u₁ hg₁
                    split at h
                    · cases h
                    · rename_i hover
                      cases h
                      exact .assign hget₀ hmu hg₀ hty₀ (check_sound e hchk) hget₁ hg₁
                        ((Bool.not_eq_true _).mp hover)
                  · cases h
                · cases h
              · cases h
            · cases h
          · cases h
        · cases h
  | .seq e₁ e₂, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · split at h
        · cases h
        · exact .seq (check_sound e₁ ‹_›) ‹_› (check_sound e₂ h)
      · cases h
  | .ite c e₁ e₂, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · rename_i Γ₀ hcond
        split at h
        · rename_i T₁ Γ₁ T₂ Γ₂ h₁ h₂
          split at h
          · rename_i hT
            split at h
            · rename_i Γj hjoin
              cases h
              subst hT
              exact .ite (check_sound c hcond) (check_sound e₁ h₁) (check_sound e₂ h₂) hjoin
            · cases h
          · cases h
        · cases h
      · cases h
  | .call f args, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · cases h
      · rename_i fd hfd
        split at h
        · rename_i Γ₁ hargs
          cases h
          exact .call hfd (checkArgs_sound args hargs)
        · cases h
  | .ret e, Γ, T, Γ', h => by
      simp only [check] at h
      split at h
      · cases h
      · rename_i T₁ Γ₁ hchk
        split at h
        · rename_i hcond
          cases h
          obtain ⟨hT, hnl⟩ := hcond
          subst hT
          exact .ret (check_sound e hchk) hnl rfl
        · cases h

/-- Every `checkArgs` acceptance is a real (Call) §5.8 argument-list
derivation. -/
theorem checkArgs_sound {P : Program} {R : Ty} : ∀ (es : List Expr) {Γ : Ctx} {Ts Γ'},
    checkArgs P R Γ es Ts = some Γ' → TypedArgs P R Γ es Ts Γ'
  | [], Γ, Ts, Γ', h => by
      cases Ts with
      | nil => simp only [checkArgs] at h; cases h; exact .nil
      | cons _ _ => simp only [checkArgs] at h; simp at h
  | e :: es, Γ, Ts, Γ', h => by
      cases Ts with
      | nil => simp only [checkArgs] at h; simp at h
      | cons T Ts' =>
          simp only [checkArgs] at h
          split at h
          · rename_i T'' Γ₁ hchk
            split at h
            · rename_i hT
              subst hT
              exact .cons (check_sound e hchk) (checkArgs_sound es h)
            · cases h
          · cases h
end

/-- (Fn) §5.8 as an algorithm: the body checks at the declared return type
from the entry context `Γ0;Σ0` (`fnCtx`), and its normal exit edge discharges
§5.6's residual-linear obligation for the by-value parameters and every
still-open body-local binding (`3.8:62`). -/
def checkFn (P : Program) (fd : FnDef) : Bool :=
  match check P fd.ret (fnCtx fd) fd.body with
  | some (T, Γf) => decide (T = fd.ret) && decide (NoResidualLinear P.structs Γf)
  | none => false

/-- §3's class assignment for one declaration, as an algorithm: the recorded
class is the attribute's lifting of the field join, a `@copy` declaration's
join is already `Copy` and it has no destructor (`3.8:18`, `3.9:31`), a
destructor-bearing declaration carries no linear field (`3.9:44`), and every
field names an earlier declaration. -/
def checkStructDecl (D : StructEnv) (s : Nat) (sd : StructDecl) : Bool :=
  sd.fields.all (fun T => match T with | .struct s' => decide (s' < s) | _ => true) &&
    decide (sd.cls = sd.attr.lift (sd.baseOf D)) &&
    (match sd.attr with
     | .copy => decide (sd.baseOf D = .copy) && !sd.dtor
     | _ => true) &&
    (!sd.dtor || !decide (sd.baseOf D = .linear))

/-- The declarations from index `k` on (helper). -/
def checkStructsFrom (D : StructEnv) : Nat → List StructDecl → Bool
  | _, [] => true
  | s, sd :: rest => checkStructDecl D s sd && checkStructsFrom D (s + 1) rest

/-- §3's class assignment for a whole struct environment, as an algorithm.
`WfStructs` is what it decides, and that is the premise `Ty.mult`'s lookup
needs to be §3's join. -/
def checkStructs (D : StructEnv) : Bool := checkStructsFrom D 0 D

/-- A whole program as an algorithm: (Fn) §5.8 for every function, plus the
entry point's empty parameter list (§6.12's top-level result is `main()`). -/
def checkProgram (P : Program) : Bool :=
  checkStructs P.structs && P.fns.all (checkFn P) &&
    (match P.fns[0]? with
     | some fd => fd.params.isEmpty
     | none => false)

/-- Every `checkFn` acceptance is a real (Fn) §5.8 derivation. -/
theorem checkFn_sound {P : Program} {fd : FnDef} (h : checkFn P fd = true) : WfFn P fd := by
  unfold checkFn at h
  split at h
  · rename_i T Γf hchk
    simp only [Bool.and_eq_true, decide_eq_true_eq] at h
    obtain ⟨hT, hnl⟩ := h
    subst hT
    exact ⟨Γf, check_sound fd.body hchk, hnl⟩
  · exact absurd h (by simp)

/-- Every `checkStructDecl` acceptance is §3's class assignment for that
declaration. -/
theorem checkStructDecl_sound {D : StructEnv} {s : Nat} {sd : StructDecl}
    (h : checkStructDecl D s sd = true) : sd.Wf D s := by
  unfold checkStructDecl at h
  simp only [Bool.and_eq_true, List.all_eq_true, decide_eq_true_eq] at h
  obtain ⟨⟨⟨hfields, hcls⟩, hcopy⟩, hdtor⟩ := h
  refine ⟨?_, hcls, ?_, ?_⟩
  · intro s' hmem
    have := hfields _ hmem
    simpa using this
  · intro hattr
    rw [hattr] at hcopy
    simp only [Bool.and_eq_true, decide_eq_true_eq, Bool.not_eq_eq_eq_not,
      Bool.not_true] at hcopy
    exact ⟨hcopy.1, hcopy.2⟩
  · intro hd
    simp only [hd, Bool.not_true, Bool.false_or, Bool.not_eq_eq_eq_not, Bool.not_true,
      decide_eq_false_iff_not] at hdtor
    exact hdtor

/-- `checkStructsFrom` checks the declaration at every offset (helper). -/
theorem checkStructsFrom_sound : ∀ (D : StructEnv) (k : Nat) (L : List StructDecl),
    checkStructsFrom D k L = true → ∀ (i : Nat) (sd : StructDecl), L[i]? = some sd →
      checkStructDecl D (k + i) sd = true
  | _, _, [], _, i, sd, hget => by simp at hget
  | D, k, sd₀ :: rest, h, i, sd, hget => by
      simp only [checkStructsFrom, Bool.and_eq_true] at h
      cases i with
      | zero =>
          simp only [List.getElem?_cons_zero, Option.some_inj] at hget
          subst hget
          simpa using h.1
      | succ j =>
          simp only [List.getElem?_cons_succ] at hget
          have hk := checkStructsFrom_sound D (k + 1) rest h.2 j sd hget
          have heq : k + 1 + j = k + (j + 1) := by omega
          rwa [heq] at hk

/-- Every `checkStructs` acceptance is §3's class assignment for the whole
environment (`WfStructs`). -/
theorem checkStructs_sound {D : StructEnv} (h : checkStructs D = true) : WfStructs D := by
  intro s sd hget
  have := checkStructsFrom_sound D 0 D h s sd hget
  simpa using checkStructDecl_sound this

/-- Every `checkProgram` acceptance is the `ProgramTyped` hypothesis the §7
program theorems (`Soundness.lean`) take, so running the checker is enough to
know the safety theorems apply to a program. -/
theorem checkProgram_sound {P : Program} (h : checkProgram P = true) : ProgramTyped P := by
  unfold checkProgram at h
  simp only [Bool.and_eq_true, List.all_eq_true] at h
  obtain ⟨⟨hstructs, hall⟩, hentry⟩ := h
  refine ⟨⟨checkStructs_sound hstructs,
    fun fd hmem => checkFn_sound (hall fd (by simpa using hmem))⟩, ?_⟩
  split at hentry
  · rename_i fd hfd
    exact ⟨fd, hfd, List.isEmpty_iff.mp hentry⟩
  · exact absurd hentry (by simp)

end RueCore
