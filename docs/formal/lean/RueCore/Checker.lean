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

`checkProgram` lifts it to a whole program: the declarations are well-formed
(`checkDecls`), every function body checks at its declared return type under
(Fn) §5.8's entry context, its normal exit edge discharges §5.6's obligation,
and the entry point takes no parameters. Its soundness lemma produces the
`ProgramTyped` hypothesis `Soundness.lean`'s program theorems ask for.

`checkDecls` is what makes `Ty.mult`'s lookup honest, in three parts. A
declaration *records* `class(S)`, and `checkStructs` is the equation §3 writes
for it, together with `3.8:18`/`3.9:31`'s `@copy` restriction and `3.9:44`'s
destructor restriction; `checkEnums` is the enum layer's one equation, the
payload join over every variant (`6.3:19`). What makes either equation a
*definition* rather than a fixpoint condition is `3.0:5` (E0483): no
declaration contains itself by value, directly or through a cycle. That rule is
joint over the two layers — a field may name an enum and a payload may name a
struct — so `checkNoCycle` decides it once, for the whole environment, and
`class_unique` is the unconditional uniqueness statement it buys.

## `match`, algorithmically

(Match) §5.5 decides four things, and `check` decides each where the rule states
it. **Arm count and order**: the arm list must be as long as the variant list,
and arm `j` is variant `j`'s — that is exhaustiveness (`4.7:9`, `4.7:10`) with no
coverage search. **Arity**: each arm's payload locals are the variant's declared
components, which `armCtx` supplies, so a wrong arity is not expressible rather
than rejected. **The per-arm leak check**: `NoResidualLinear` over the entries
the arm pops, which is `letIn`'s check read over a whole payload. **The folded
join**: `Ctx.joinAll` over the arms' outgoing contexts in declaration order.

The one thing `check` must *choose* is the arms' shared type, since §5.5 states
it as one `T` and lets (Sub-Never) coerce a diverging arm to it. `firstArmTy`
takes the first arm's, and every other arm is compared against that — the same
choice `ite` makes for its two arms, with the same cost: a `match` whose first
arm is a `return` takes `R` for `T` and a sibling arm at another type is refused,
where §5.5 would take the sibling's. The first arm is therefore checked twice,
which costs time and nothing else.

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
unusable. `main() -> int { let x = mk 5; (if c { @drop(x); return 0 } else { 5 }); @drop(x) }`
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
        match en.st.get p.path, en.ty.atPath P.decls p.path with
        | some u, some T =>
            if noLinearPrefix P.decls en.ty p.path then
              if T.mult P.decls = .copy then
                (if u.fullyOwned then some (T, Γ) else none)
              else
                (if u.fullyOwned ∧ noDtorPrefix P.decls en.ty p.path then
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
      match P.decls.structs[s]? with
      | none => none
      | some sd =>
        match checkArgs P R Γ args sd.fields with
        | some Γ' => some (.struct s, Γ')
        | none => none
  | .mkEnum e k args =>
      match P.decls.enums[e]? with
      | none => none
      | some ed =>
        match ed.variants[k]? with
        | none => none
        | some Ts =>
          match checkArgs P R Γ args Ts with
          | some Γ' => some (.enum e, Γ')
          | none => none
  | .«match» scrut arms =>
      match check P R Γ scrut with
      | some (.enum e, Γ₀) =>
        (match P.decls.enums[e]? with
         | none => none
         | some ed =>
           if arms.length = ed.variants.length then
             (match firstArmTy P R Γ₀ arms ed.variants with
              | none => none
              | some T =>
                (match checkArms P R Γ₀ T arms ed.variants with
                 | none => none
                 | some Γs =>
                   (match Ctx.joinAll P.decls Γs with
                    | some Γ' => some (T, Γ')
                    | none => none)))
           else none)
      | _ => none
  | .drop p =>
      match Γ[p.root]? with
      | none => none
      | some en =>
        match en.st.get p.path, en.ty.atPath P.decls p.path with
        | some u, some T =>
            if noLinearPrefix P.decls en.ty p.path then
              if T.mult P.decls = .copy then
                (if u.fullyOwned then some (.unit, Γ) else none)
              else
                (if u.isOwned ∧ noDtorPrefix P.decls en.ty p.path ∧
                    (u.fullyOwned = true ∨ residualLinearBelow P.decls u T = false) then
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
            if residualLinear P.decls en'.st en'.ty then none else some (T₂, Γ₂)
        | _ => none
  | .assign p e =>
      match Γ[p.root]? with
      | none => none
      | some en₀ =>
        if en₀.mu = true then
          match en₀.st.get p.path, en₀.ty.atPath P.decls p.path with
          | some _, some T =>
            (match check P R Γ e with
             | some (T', Γ₁) =>
               if T' = T then
                 (match Γ₁[p.root]? with
                  | some en₁ =>
                    (match en₁.st.get p.path with
                     | some u₁ =>
                         if overwriteOk P.decls u₁ T then
                           some (.unit,
                             Γ₁.set p.root (en₁.setSt (en₁.st.setAt p.path .owned)))
                         else none
                     | none => none)
                  | none => none)
               else none
             | none => none)
          | _, _ => none
        else none
  | .seq e₁ e₂ =>
      match check P R Γ e₁ with
      | some (T₁, Γ₁) =>
          if T₁.mult P.decls = .linear then none
          else check P R Γ₁ e₂
      | none => none
  | .ite c e₁ e₂ =>
      match check P R Γ c with
      | some (.bool, Γ₀) =>
        (match check P R Γ₀ e₁, check P R Γ₀ e₂ with
        | some (T₁, Γ₁), some (T₂, Γ₂) =>
            if T₁ = T₂ then
              match Ctx.join P.decls Γ₁ Γ₂ with
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
          if T = R ∧ NoResidualLinear P.decls Γ₁ then some (R, Γ₁) else none

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

/-- The type (Match) §5.5's arms must share, as the algorithm picks it: the
**first** arm's, read under that arm's own payload locals. §5.5 states the
premise as one type `T` for every arm and lets (Sub-Never) supply it for a
diverging one, which an algorithm cannot do, so `check` fixes `T` here and
compares the others against it — exactly what it does for `ite`'s two arms, with
the same cost in completeness (module docstring): a `match` whose first arm is a
`return` takes `R` for `T`, and a sibling arm at another type is refused. -/
def firstArmTy (P : Program) (R : Ty) (Γ₀ : Ctx) :
    List Expr → List (List Ty) → Option Ty
  | e :: _, Ts :: _ => (check P R (armCtx Ts Γ₀) e).map Prod.fst
  | _, _ => none

/-- (Match) §5.5's arm premises as an algorithm: every arm from the same
post-scrutinee state `Γ₀`, each under its variant's payload locals (`armCtx`),
each at the type `T` the first arm fixed, and each discharging §5.6 for the
locals it pops. The result is one outgoing context per arm, in declaration
order, which is what `Ctx.joinAll` then folds. A count mismatch between the arms
and the variants is the last clause's `none` — `check` has already required the
counts to agree, so no program reaches it. -/
def checkArms (P : Program) (R : Ty) (Γ₀ : Ctx) (T : Ty) :
    List Expr → List (List Ty) → Option (List Ctx)
  | [], [] => some []
  | e :: es, Ts :: Tss =>
      match check P R (armCtx Ts Γ₀) e with
      | some (T', Γb) =>
          if T' = T ∧ NoResidualLinear P.decls (Γb.take Ts.length) then
            (match checkArms P R Γ₀ T es Tss with
             | some Γs => some (Γb.drop Ts.length :: Γs)
             | none => none)
          else none
      | none => none
  | _, _ => none
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
  | .mkEnum e k args, Γ, T, Γ', h => by
      simp only [check] at h
      cases hed : P.decls.enums[e]? with
      | none => simp only [hed] at h; cases h
      | some ed =>
        simp only [hed] at h
        cases hv : ed.variants[k]? with
        | none => simp only [hv] at h; cases h
        | some Ts =>
          simp only [hv] at h
          cases hargs : checkArgs P R Γ args Ts with
          | none => simp only [hargs] at h; cases h
          | some Γ₁ =>
              simp only [hargs] at h
              cases h
              exact .mkEnum hed hv (checkArgs_sound args hargs)
  | .«match» scrut arms, Γ, T, Γ', h => by
      simp only [check] at h
      cases hscrut : check P R Γ scrut with
      | none => simp only [hscrut] at h; cases h
      | some p =>
        obtain ⟨Tsc, Γ₀⟩ := p
        simp only [hscrut] at h
        cases Tsc with
        | int w sg => simp at h
        | float w => simp at h
        | bool => simp at h
        | unit => simp at h
        | struct s' => simp at h
        | enum e =>
          simp only [] at h
          cases hed : P.decls.enums[e]? with
          | none => simp only [hed] at h; cases h
          | some ed =>
            simp only [hed] at h
            by_cases hlen : arms.length = ed.variants.length
            · simp only [if_pos hlen] at h
              cases hfirst : firstArmTy P R Γ₀ arms ed.variants with
              | none => simp only [hfirst] at h; cases h
              | some T₁ =>
                simp only [hfirst] at h
                cases harms : checkArms P R Γ₀ T₁ arms ed.variants with
                | none => simp only [harms] at h; cases h
                | some Γs =>
                  simp only [harms] at h
                  cases hjoin : Ctx.joinAll P.decls Γs with
                  | none => simp only [hjoin] at h; cases h
                  | some Γj =>
                      simp only [hjoin] at h
                      cases h
                      exact .«match» (check_sound scrut hscrut) hed hlen
                        (checkArms_sound arms harms) hjoin
            · simp only [if_neg hlen] at h; cases h
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
                    · rename_i hover
                      cases h
                      exact .assign hget₀ hmu hg₀ hty₀ (check_sound e hchk) hget₁ hg₁
                        (overwriteOk_iff.mp hover)
                    · cases h
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

/-- Every `checkArms` acceptance is a real (Match) §5.5 arm-list derivation. -/
theorem checkArms_sound {P : Program} {R : Ty} {Γ₀ : Ctx} {T : Ty} :
    ∀ (es : List Expr) {Tss : List (List Ty)} {Γs : List Ctx},
    checkArms P R Γ₀ T es Tss = some Γs → TypedArms P R Γ₀ es Tss T Γs
  | [], Tss, Γs, h => by
      cases Tss with
      | nil => simp only [checkArms] at h; cases h; exact .noArms
      | cons _ _ => simp only [checkArms] at h; simp at h
  | e :: es, Tss, Γs, h => by
      cases Tss with
      | nil => simp only [checkArms] at h; simp at h
      | cons Ts Tss' =>
          simp only [checkArms] at h
          split at h
          · rename_i T' Γb hchk
            split at h
            · rename_i hcond
              obtain ⟨hT, hres⟩ := hcond
              subst hT
              split at h
              · rename_i Γs' harms
                cases h
                exact .arm (check_sound e hchk) hres (checkArms_sound es harms)
              · cases h
            · cases h
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
  | some (T, Γf) => decide (T = fd.ret) && decide (NoResidualLinear P.decls Γf)
  | none => false

/-- §3's class assignment for one struct declaration, as an algorithm: the
recorded class is the attribute's lifting of the field join, a `@copy`
declaration's join is already `Copy` and it has no destructor (`3.8:18`,
`3.9:31`), and a destructor-bearing declaration carries no linear field
(`3.9:44`).

Acyclicity is deliberately **not** here. `3.0:5` is one rule over both layers,
so `checkNoCycle` decides it for the whole environment at once and no
per-declaration clause can stand in for it. -/
def checkStructDecl (D : Decls) (sd : StructDecl) : Bool :=
  decide (sd.cls = sd.attr.lift (sd.baseOf D)) &&
    (match sd.attr with
     | .copy => decide (sd.baseOf D = .copy) && !sd.dtor
     | _ => true) &&
    (!sd.dtor || !decide (sd.baseOf D = .linear))

/-- §3's class assignment for a whole struct environment, as an algorithm.
`WfStructs` is what it decides, and that is the premise `Ty.mult`'s lookup
needs to be §3's join. -/
def checkStructs (D : Decls) : Bool := D.structs.all (checkStructDecl D)

/-- §3's class assignment for one enum declaration, as an algorithm (`6.3:19`):
the recorded class is the payload join over every variant. There is no attribute
clause, no destructor clause and no acyclicity clause — §3 gives an enum neither
of the first two, and the third is `checkNoCycle`'s. -/
def checkEnumDecl (D : Decls) (ed : EnumDecl) : Bool :=
  decide (ed.cls = ed.payloadJoin D)

/-- §3's class assignment for a whole enum environment, as an algorithm.
`WfEnums` is what it decides, and that is the premise `Ty.mult`'s lookup needs to
be `6.3:19`'s join at an enum type. -/
def checkEnums (D : Decls) : Bool := D.enums.all (checkEnumDecl D)

/-! ### `3.0:5`, decided by peeling

`WfNames` (`Statics.lean`) says the by-value "contains" relation over the
declarations is well-founded. On a finite environment that is decidable by
**peeling**: a declaration is *grounded* at round `n+1` when every declaration
it contains by value is grounded at round `n`, and nothing is grounded at round
`0`. Grounding is monotone in the round, and while any declaration is
ungrounded but has all its dependencies grounded, the next round grounds it —
so `|structs| + |enums|` rounds settle the question, and an environment all of
whose declarations are grounded by then has no cycle.

The order is **computed and thrown away**. Nothing is stored in `Decls`, so a
declaration environment is the same data it always was — which is what keeps
the corpus JSON shape and the seed cases unchanged — and the one rule covers
both layers, as `3.0:5` writes it (E0483).
-/

/-- Whether a type's declaration is already grounded. A scalar names no
declaration, so it always is (helper). -/
def Ty.grounded (st : List Bool × List Bool) : Ty → Bool
  | .struct s => (st.1[s]?).getD false
  | .enum e => (st.2[e]?).getD false
  | .int _ _ | .float _ | .bool | .unit => true

/-- One peel round: a declaration is grounded when every type it contains by
value is — a struct's fields, an enum's payload components over every variant
(helper). -/
def Decls.peelStep (D : Decls) (st : List Bool × List Bool) : List Bool × List Bool :=
  (D.structs.map (fun sd => sd.fields.all (Ty.grounded st)),
   D.enums.map (fun ed => ed.variants.all (fun Ts => Ts.all (Ty.grounded st))))

/-- The grounded flags after `n` peel rounds, one per declaration of each
layer; nothing is grounded at round `0` (helper). -/
def Decls.peel (D : Decls) : Nat → List Bool × List Bool
  | 0 => (D.structs.map (fun _ => false), D.enums.map (fun _ => false))
  | n + 1 => D.peelStep (D.peel n)

/-- **`3.0:5` (E0483) as an algorithm**: every declaration is grounded after
`|structs| + |enums|` peel rounds, which is "no struct or enum contains itself
by value, either directly or through a cycle of struct fields and enum
payloads". `checkNoCycle_sound` turns an acceptance into `WfNames`, the premise
that makes §3's two class equations a definition. -/
def checkNoCycle (D : Decls) : Bool :=
  let st := D.peel (D.structs.length + D.enums.length)
  st.1.all id && st.2.all id

/-- A whole declaration environment as an algorithm: §3's equation for every
struct (`checkStructs`), `6.3:19`'s for every enum (`checkEnums`), and
`3.0:5`'s acyclicity once, jointly (`checkNoCycle`). `WfDecls` is what it
decides. -/
def checkDecls (D : Decls) : Bool := checkStructs D && checkEnums D && checkNoCycle D

/-- A whole program as an algorithm: §3 and `3.0:5` for the declarations, (Fn)
§5.8 for every function, plus the entry point's empty parameter list (§6.12's
top-level result is `main()`). -/
def checkProgram (P : Program) : Bool :=
  checkDecls P.decls && P.fns.all (checkFn P) &&
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
theorem checkStructDecl_sound {D : Decls} {sd : StructDecl}
    (h : checkStructDecl D sd = true) : sd.Wf D := by
  unfold checkStructDecl at h
  simp only [Bool.and_eq_true, decide_eq_true_eq] at h
  obtain ⟨⟨hcls, hcopy⟩, hdtor⟩ := h
  refine ⟨hcls, ?_, ?_⟩
  · intro hattr
    rw [hattr] at hcopy
    simp only [Bool.and_eq_true, decide_eq_true_eq, Bool.not_eq_eq_eq_not,
      Bool.not_true] at hcopy
    exact ⟨hcopy.1, hcopy.2⟩
  · intro hd
    simp only [hd, Bool.not_true, Bool.false_or, Bool.not_eq_eq_eq_not, Bool.not_true,
      decide_eq_false_iff_not] at hdtor
    exact hdtor

/-- Every `checkStructs` acceptance is §3's class assignment for the whole
environment (`WfStructs`). -/
theorem checkStructs_sound {D : Decls} (h : checkStructs D = true) : WfStructs D := by
  intro s sd hget
  simp only [checkStructs, List.all_eq_true] at h
  obtain ⟨hlt, hs⟩ := List.getElem?_eq_some_iff.mp hget
  exact checkStructDecl_sound (h sd (hs ▸ List.getElem_mem hlt))

/-- Every `checkEnumDecl` acceptance is §3's class assignment for that
declaration (`6.3:19`). -/
theorem checkEnumDecl_sound {D : Decls} {ed : EnumDecl}
    (h : checkEnumDecl D ed = true) : ed.Wf D := by
  unfold checkEnumDecl at h
  simp only [decide_eq_true_eq] at h
  exact ⟨h⟩

/-- Every `checkEnums` acceptance is §3's class assignment for the whole enum
environment (`WfEnums`). -/
theorem checkEnums_sound {D : Decls} (h : checkEnums D = true) : WfEnums D := by
  intro e ed hget
  simp only [checkEnums, List.all_eq_true] at h
  obtain ⟨hlt, he⟩ := List.getElem?_eq_some_iff.mp hget
  exact checkEnumDecl_sound (h ed (he ▸ List.getElem_mem hlt))

/-- The grounded flags have one entry per declaration at every round
(helper). -/
theorem Decls.peel_length (D : Decls) : ∀ n : Nat,
    (D.peel n).1.length = D.structs.length ∧ (D.peel n).2.length = D.enums.length
  | 0 => ⟨by simp [Decls.peel], by simp [Decls.peel]⟩
  | _ + 1 => ⟨by simp [Decls.peel, Decls.peelStep], by simp [Decls.peel, Decls.peelStep]⟩

/-- Nothing is grounded at round `0` (helper). -/
theorem Decls.grounded_peel_zero (D : Decls) (d : DeclId) :
    Ty.grounded (D.peel 0) d.ty = false := by
  cases d with
  | struct s =>
      simp only [DeclId.ty, Ty.grounded, Decls.peel, List.getElem?_map]
      cases D.structs[s]? <;> rfl
  | enum e =>
      simp only [DeclId.ty, Ty.grounded, Decls.peel, List.getElem?_map]
      cases D.enums[e]? <;> rfl

/-- A declaration grounded at round `n+1` contains only declarations grounded
at round `n`. This is the peel read backwards, and it is what turns an
acceptance into well-foundedness (helper). -/
theorem Decls.grounded_pred {D : Decls} {n : Nat} {d d' : DeclId}
    (h : Ty.grounded (D.peel (n + 1)) d.ty = true) (hn : D.Names d d') :
    Ty.grounded (D.peel n) d'.ty = true := by
  cases d with
  | struct s =>
      simp only [DeclId.ty, Ty.grounded, Decls.peel, Decls.peelStep, List.getElem?_map] at h
      cases hd : D.structs[s]? with
      | none => rw [hd] at h; exact absurd h (by simp)
      | some sd =>
          rw [hd] at h
          simp only [Option.map_some, Option.getD_some, List.all_eq_true] at h
          simp only [Decls.Names, Decls.byValue, hd] at hn
          exact h d'.ty hn
  | enum e =>
      simp only [DeclId.ty, Ty.grounded, Decls.peel, Decls.peelStep, List.getElem?_map] at h
      cases hd : D.enums[e]? with
      | none => rw [hd] at h; exact absurd h (by simp)
      | some ed =>
          rw [hd] at h
          simp only [Option.map_some, Option.getD_some, List.all_eq_true] at h
          simp only [Decls.Names, Decls.byValue, hd] at hn
          obtain ⟨Ts, hTs, hT⟩ := List.mem_flatten.mp hn
          exact h Ts hTs d'.ty hT

/-- A declaration grounded at some round is accessible in the by-value
relation (helper). -/
theorem Decls.acc_of_grounded (D : Decls) : ∀ (n : Nat) (d : DeclId),
    Ty.grounded (D.peel n) d.ty = true → Acc (fun a b => D.Names b a) d
  | 0, d, h => absurd h (by rw [D.grounded_peel_zero d]; simp)
  | n + 1, d, h =>
      Acc.intro d (fun d' hd' => D.acc_of_grounded n d' (Decls.grounded_pred h hd'))

/-- A declaration index the environment does not have contains nothing, so it
is accessible outright (helper). -/
theorem Decls.acc_of_empty {D : Decls} {d : DeclId} (h : D.byValue d = []) :
    Acc (fun a b => D.Names b a) d :=
  Acc.intro d (fun _ hd' => absurd hd' (by simp [Decls.Names, h]))

/-- **Every `checkNoCycle` acceptance is `3.0:5`** (`WfNames`): the by-value
"contains" relation over the declarations is well-founded, so no struct or enum
contains itself by value through any cycle of fields and payloads. This is the
premise `class_unique` turns into "§3's class assignment has one solution". -/
theorem checkNoCycle_sound {D : Decls} (h : checkNoCycle D = true) : WfNames D := by
  have key : ∀ (l : List Bool) (i : Nat) (b : Bool), l.all id = true → l[i]? = some b →
      b = true := by
    intro l i b hall hb
    simp only [List.all_eq_true, id] at hall
    obtain ⟨hlt, hget⟩ := List.getElem?_eq_some_iff.mp hb
    exact hall b (hget ▸ List.getElem_mem hlt)
  simp only [checkNoCycle, Bool.and_eq_true] at h
  obtain ⟨hs, he⟩ := h
  refine WellFounded.intro (fun d => ?_)
  cases d with
  | struct s =>
      cases hd : D.structs[s]? with
      | none => exact Decls.acc_of_empty (by simp [Decls.byValue, hd])
      | some sd =>
          refine D.acc_of_grounded (D.structs.length + D.enums.length) (.struct s) ?_
          have hlt : s < (D.peel (D.structs.length + D.enums.length)).1.length := by
            rw [(D.peel_length _).1]
            exact (List.getElem?_eq_some_iff.mp hd).1
          have hb := List.getElem?_eq_getElem hlt
          simp only [DeclId.ty, Ty.grounded, hb, Option.getD_some]
          exact key _ s _ hs hb
  | enum e =>
      cases hd : D.enums[e]? with
      | none => exact Decls.acc_of_empty (by simp [Decls.byValue, hd])
      | some ed =>
          refine D.acc_of_grounded (D.structs.length + D.enums.length) (.enum e) ?_
          have hlt : e < (D.peel (D.structs.length + D.enums.length)).2.length := by
            rw [(D.peel_length _).2]
            exact (List.getElem?_eq_some_iff.mp hd).1
          have hb := List.getElem?_eq_getElem hlt
          simp only [DeclId.ty, Ty.grounded, hb, Option.getD_some]
          exact key _ e _ he hb

/-- Every `checkDecls` acceptance is a well-formed declaration environment:
§3's class assignment in both layers and `3.0:5`'s acyclicity. -/
theorem checkDecls_sound {D : Decls} (h : checkDecls D = true) : WfDecls D := by
  simp only [checkDecls, Bool.and_eq_true] at h
  exact ⟨checkNoCycle_sound h.2, checkStructs_sound h.1.1, checkEnums_sound h.1.2⟩

/-- Every `checkProgram` acceptance is the `ProgramTyped` hypothesis the §7
program theorems (`Soundness.lean`) take, so running the checker is enough to
know the safety theorems apply to a program. -/
theorem checkProgram_sound {P : Program} (h : checkProgram P = true) : ProgramTyped P := by
  unfold checkProgram at h
  simp only [Bool.and_eq_true, List.all_eq_true] at h
  obtain ⟨⟨hdecls, hall⟩, hentry⟩ := h
  refine ⟨⟨checkDecls_sound hdecls,
    fun fd hmem => checkFn_sound (hall fd (by simpa using hmem))⟩, ?_⟩
  split at hentry
  · rename_i fd hfd
    exact ⟨fd, hfd, List.isEmpty_iff.mp hentry⟩
  · exact absurd hentry (by simp)

end RueCore
