import RueCore.Statics

/-!
# RueCore.Checker.Defs — the checker, as an algorithm (layer L1)

`check`, `checkFn`, `checkDecls` and `checkProgram`: §5's judgment and §3's
class assignment as a decision procedure, the verdict the bridge corpus
records. The module docstring of `Checker.lean` explains the algorithm and
what completeness costs; `Checker.lean` proves it sound (`check_sound`,
`checkProgram_sound`).

The definitions are moved here verbatim from `Checker.lean` (RUE-2456), so the
algorithm the soundness theorem is about sits in the definitions layer and
depends on nothing but §5's rules (`Statics`); the audit
`lake exe ruecore-layers` keeps it that way (README, "Layers").
-/

namespace RueCore

/-- The type `check` concludes at: a type, or `never` — §5.7's type of the
diverging forms, which the fragment's rules fold (Sub-Never) into by
concluding at every type. `check` returns `never` exactly where the rule it
mirrors concludes at an arbitrary type, and `check_sound` says so: a `never`
result has a derivation at **every** type (§5.7's (Sub-Never)). -/
inductive CTy where
  /-- §5.7's `never`: the expression has a derivation at every type. -/
  | never
  /-- An ordinary type. -/
  | ty (T : Ty)
deriving DecidableEq, Repr

/-- Whether a checked type admits `T` — (Sub-Never) §5.7 for `never`, identity
otherwise (helper). -/
def CTy.fits : CTy → Ty → Bool
  | .never, _ => true
  | .ty T', T => decide (T' = T)

/-- Whether one checked type admits every type another admits — the arm
comparison `match` makes against the type `firstArmTy` fixed (helper). -/
def CTy.fitsC : CTy → CTy → Bool
  | .never, _ => true
  | .ty T', .ty T => decide (T' = T)
  | .ty _, .never => false

/-- The common type of two branch arms, §5.5's single `T` with (Sub-Never)
§5.7 applied to a diverging arm: `never` meets anything, two types meet only
when equal (helper). -/
def CTy.meet : CTy → CTy → Option CTy
  | .never, c => some c
  | c, .never => some c
  | .ty T₁, .ty T₂ => if T₁ = T₂ then some (.ty T₁) else none

/-- A type a checked type admits, defaulting when it admits them all (helper). -/
def CTy.pick : CTy → Ty → Ty
  | .ty T, _ => T
  | .never, d => d

mutual
/-- The number of nodes of an expression, the bound on §5.7's loop-head
iteration (`headIter`) (helper). -/
def Expr.nodes : Expr → Nat
  | .intLit _ _ _ | .floatLit _ _ | .boolLit _ | .unitLit | .use _ | .panic _ | .drop _
  | .brk => 1
  | .binop _ e₁ e₂ | .letIn _ e₁ e₂ | .seq e₁ e₂ => e₁.nodes + e₂.nodes + 1
  | .unop _ e | .intCast _ _ e | .fintrin _ e | .dbg e | .repeatArray _ e _ | .assign _ e
  | .ret e | .loop e => e.nodes + 1
  | .mkStruct _ args | .mkEnum _ _ args | .mkArray _ args | .call _ args
  | .indexRead _ args _ | .indexDrop _ args _ => Expr.nodesList args + 1
  | .indexWrite _ idx _ e => e.nodes + Expr.nodesList idx + 1
  | .ite c e₁ e₂ => c.nodes + e₁.nodes + e₂.nodes + 1
  | .«match» scrut arms => scrut.nodes + Expr.nodesList arms + 1

/-- The same over a list (helper). -/
def Expr.nodesList : List Expr → Nat
  | [] => 0
  | e :: es => e.nodes + Expr.nodesList es
end

/-! ### The loop head, algorithmically

§5.7 types a loop body at the loop-head state `Σ_h`, a fixpoint of
`Σ_h = join(Σ, B_h)` where `B_h` is read off the body typed at `Σ_h`, and says
how to compute the least one: "start from `Σ`, type the body, join the entry
with the back-edge states it reaches, and repeat until the state stops
changing". `headIter` is that iteration, over a function `body` that types
the body at a candidate head and reports its normal outgoing state (`none`
when the body is refused there). It stops at the first candidate the step
leaves unchanged, and refuses when a step is refused, when a join is
undefined (§5.5's `3.8:50`: a linear-carrying path `Owned` at entry and
`MovedOut` at the back edge), or when the bound runs out.

**Termination** is structural: the iteration is bounded, so `check` is total.
**The bound suffices**, by the argument §5.7 gives: the sequence only moves
toward `MovedOut`. Every head is `join(Σ, Σ_e)` for a back-edge state `Σ_e`,
so it has every move `Σ` has; the body's outgoing state at a path is either
written by the body (a move, a `@drop`, an assignment) — the same at every
head — or carried through from the head, so a head with more paths `MovedOut`
gives a back-edge state with at least as many, and the next head is no
smaller. A head that is not yet the fixpoint therefore adds a `MovedOut` at a
path the body writes, and the body writes at most one per node of its
syntax, which is the bound (`Expr.nodes`, plus the step that confirms the
fixpoint). The bound is generous: in practice the second step already
confirms the fixpoint, because the back-edge moves at the first head are the
body's writes plus the head's carried moves, all of which that head already
has. Its cost is multiplicative in loop nesting depth — each level re-checks
its body about three times — which the corpus does not feel. None of this is
needed for soundness: `check` re-checks the body
at the head it found and verifies the equation `LoopHead` states, so
`check_sound` reads only that final check. A bound too small would cost
completeness, never soundness. -/

/-- One step of the head iteration: join the entry state with the back-edge
state the body reached from the current candidate, or `none` when the body
was refused there or the join is undefined (helper). -/
def headNext (D : Decls) (Γ : Ctx) : Option (Option Ctx) → Option Ctx
  | some o =>
      match Ctx.joinOpt D (some Γ) o with
      | some (some Γ') => some Γ'
      | _ => none
  | none => none

/-- §5.7's head iteration from the candidate `Γc`, for at most `n` steps: the
first candidate a step leaves unchanged (section docstring) (helper). -/
def headIter (D : Decls) (body : Ctx → Option (Option Ctx)) (Γ : Ctx) :
    Nat → Ctx → Option Ctx
  | 0, _ => none
  | n + 1, Γc =>
      match headNext D Γ (body Γc) with
      | none => none
      | some Γ' => if Γ' = Γc then some Γc else headIter D body Γ n Γ'

mutual
/-- The §5 judgment as an algorithm: one case per `Typed` rule, in the same
order, producing the type (`CTy`) and §5.3's outgoing `Ω` or rejecting. `P`
is the top-level function environment (Call) §5.8 looks a callee up in and
`R` the enclosing function's declared return type (Return-Value) §5.7 checks
a `return` operand against. Where an operand's `Ω` is `⊥` the algorithm stops
exactly where the `-Bottom` rules stop, and a branch joins only the arms that
continue (`Ctx.joinOpt`, `Ctx.joinOpts`). -/
def check (P : Program) (R : Ty) (Γ : Ctx) : Expr → Option (CTy × Out)
  | .intLit w s n => if InBounds w s n then some (.ty (.int w s), ⟨some Γ, []⟩) else none
  | .boolLit _ => some (.ty .bool, ⟨some Γ, []⟩)
  | .unitLit => some (.ty .unit, ⟨some Γ, []⟩)
  | .use p =>
      match Γ[p.root]? with
      | none => none
      | some en =>
        match declaredPrefix P.decls en.ty p.path with
        | some (πd, πs) =>
            (match en.st.get πd, en.ty.atPath P.decls πd, en.ty.atPath P.decls p.path with
             | some u, some Td, some T =>
                 if u.fullyOwned ∧ linearResidue P.decls Td πs = false ∧
                     noDtorPrefix P.decls en.ty p.path then
                   some (.ty T, ⟨some (Γ.set p.root (en.setSt (en.st.setAt πd .movedOut))), []⟩)
                 else none
             | _, _, _ => none)
        | none =>
            (match en.st.get p.path, en.ty.atPath P.decls p.path with
             | some u, some T =>
                 if T.mult P.decls = .copy then
                   (if u.fullyOwned then some (.ty T, ⟨some Γ, []⟩) else none)
                 else
                   (if u.fullyOwned ∧ noDtorPrefix P.decls en.ty p.path ∧
                       rootIdxOnly P.decls en.ty p.path then
                      some (.ty T, ⟨some (Γ.set p.root (en.setSt (en.st.setAt p.path .movedOut))), []⟩)
                    else none)
             | _, _ => none)
  | .binop op e₁ e₂ =>
      match check P R Γ e₁ with
      | some (.ty (.int w s), ⟨some Γ₁, Δ₁⟩) =>
        (match check P R Γ₁ e₂ with
        | some (.ty (.int w' s'), Ω₂) =>
            if w' = w ∧ s' = s ∧ op.intAdmits = true then
              some (.ty (op.resultTy (.int w s)), Ω₂.add Δ₁)
            else none
        | some (.never, Ω₂) =>
            if op.intAdmits = true then some (.ty (op.resultTy (.int w s)), Ω₂.add Δ₁)
            else none
        | _ => none)
      | some (.ty (.int w s), ⟨none, Δ₁⟩) =>
          if op.intAdmits = true then some (.ty (op.resultTy (.int w s)), ⟨none, Δ₁⟩)
          else none
      | some (.ty (.float w), ⟨some Γ₁, Δ₁⟩) =>
        (match check P R Γ₁ e₂ with
        | some (.ty (.float w'), Ω₂) =>
            if w' = w ∧ op.floatAdmits = true then
              some (.ty (op.resultTy (.float w)), Ω₂.add Δ₁)
            else none
        | some (.never, Ω₂) =>
            if op.floatAdmits = true then some (.ty (op.resultTy (.float w)), Ω₂.add Δ₁)
            else none
        | _ => none)
      | some (.ty (.float w), ⟨none, Δ₁⟩) =>
          if op.floatAdmits = true then some (.ty (op.resultTy (.float w)), ⟨none, Δ₁⟩)
          else none
      | _ => none
  | .floatLit w l => if l.RoundsFinite w then some (.ty (.float w), ⟨some Γ, []⟩) else none
  | .fintrin (.intToFloat w) e =>
      match check P R Γ e with
      | some (.ty (.int _ _), Ω) => some (.ty (.float w), Ω)
      | _ => none
  | .fintrin k e =>
      match check P R Γ e with
      | some (.ty (.float w), Ω) => if k.floatSrc w then some (.ty (k.resTy w), Ω) else none
      | _ => none
  | .unop .neg e =>
      match check P R Γ e with
      | some (.ty (.int w .signed), Ω) => some (.ty (.int w .signed), Ω)
      | some (.ty (.float w), Ω) => some (.ty (.float w), Ω)
      | _ => none
  | .unop .not e =>
      match check P R Γ e with
      | some (.ty .bool, Ω) => some (.ty .bool, Ω)
      | _ => none
  | .unop .bitnot e =>
      match check P R Γ e with
      | some (.ty (.int w s), Ω) => some (.ty (.int w s), Ω)
      | _ => none
  | .intCast w s e =>
      match check P R Γ e with
      | some (.ty (.int _ _), Ω) => some (.ty (.int w s), Ω)
      | _ => none
  | .panic _ => some (.never, ⟨none, []⟩)
  | .dbg e =>
      match check P R Γ e with
      | some (.ty T, Ω) => if T.observable then some (.ty .unit, Ω) else none
      | _ => none
  | .mkStruct s args =>
      match P.decls.structs[s]? with
      | none => none
      | some sd =>
        match checkArgs P R Γ args sd.fields with
        | some Ω => some (.ty (.struct s), Ω)
        | none => none
  | .mkEnum e k args =>
      match P.decls.enums[e]? with
      | none => none
      | some ed =>
        match ed.variants[k]? with
        | none => none
        | some Ts =>
          match checkArgs P R Γ args Ts with
          | some Ω => some (.ty (.enum e), Ω)
          | none => none
  | .«match» scrut arms =>
      match check P R Γ scrut with
      | some (.ty (.enum e), ⟨some Γ₀, Δ₀⟩) =>
        (match P.decls.enums[e]? with
         | none => none
         | some ed =>
           if arms.length = ed.variants.length then
             (match checkArms P R Γ₀ (firstArmTy P R Γ₀ arms ed.variants) arms ed.variants with
              | none => none
              | some (os, Δs) =>
                (match Ctx.joinOpts P.decls os with
                 | some o => some (firstArmTy P R Γ₀ arms ed.variants, ⟨o, Δs ++ Δ₀⟩)
                 | none => none))
           else none)
      | some (.ty (.enum _), ⟨none, Δ₀⟩) => some (.never, ⟨none, Δ₀⟩)
      | some (.never, ⟨none, Δ₀⟩) => some (.never, ⟨none, Δ₀⟩)
      | _ => none
  | .mkArray T args =>
      match checkArgs P R Γ args (List.replicate args.length T) with
      | some Ω => some (.ty (.array T args.length), Ω)
      | none => none
  | .repeatArray T e n =>
      match check P R Γ e with
      | some (.ty T', Ω) =>
          if T' = T ∧ T.mult P.decls = .copy then some (.ty (.array T n), Ω) else none
      | _ => none
  | .indexRead p idx πs =>
      match checkIdx P R Γ idx with
      | some (_, ⟨some Γ₁, Δ⟩) =>
        (match Γ₁[p.root]? with
         | none => none
         | some en =>
           match en.st.get p.path, en.ty.atPath P.decls p.path with
           | some u, some Ta =>
             (match Ta.atDyn P.decls πs with
              | some T =>
                  if idx.length = πs.length ∧ πs ≠ [] ∧ u.fullyOwned ∧
                      T.mult P.decls = .copy ∧
                      declaredPrefix P.decls en.ty p.path = none ∧
                      Ta.dynNoDeclared P.decls πs then some (.ty T, ⟨some Γ₁, Δ⟩)
                  else none
              | none => none)
           | _, _ => none)
      | some (_, ⟨none, Δ⟩) =>
        (match Γ[p.root]? with
         | none => none
         | some en =>
           match en.ty.atPath P.decls p.path with
           | some Ta =>
             (match Ta.atDyn P.decls πs with
              | some T =>
                  if idx.length = πs.length ∧ πs ≠ [] then some (.ty T, ⟨none, Δ⟩) else none
              | none => none)
           | none => none)
      | none => none
  | .indexWrite p idx πs e =>
      match Γ[p.root]? with
      | none => none
      | some en₀ =>
        if en₀.mu = true then
          match en₀.st.get p.path, en₀.ty.atPath P.decls p.path with
          | some _, some Ta =>
            (match Ta.atDyn P.decls πs with
             | some T =>
               (match check P R Γ e with
                | some (c, ⟨some Γ₁, Δ₁⟩) =>
                  if c.fits T then
                    (match checkIdx P R Γ₁ idx with
                     | some (_, ⟨some Γ₂, Δ₂⟩) =>
                       (match Γ₂[p.root]? with
                        | some en₁ =>
                          (match en₁.st.get p.path with
                           | some u₁ =>
                               if idx.length = πs.length ∧ πs ≠ [] ∧ u₁.fullyOwned ∧
                                   assignArrayOk P.decls en₁.st en₁.ty p.path ∧
                                   T.mult P.decls ≠ .linear then
                                 some (.ty .unit,
                                   ⟨some (Γ₂.set p.root (en₁.setSt (en₁.st.setAt p.path .owned))),
                                     Δ₂ ++ Δ₁⟩)
                               else none
                           | none => none)
                        | none => none)
                     | some (_, ⟨none, Δ₂⟩) => some (.ty .unit, ⟨none, Δ₂ ++ Δ₁⟩)
                     | none => none)
                  else none
                | some (_, ⟨none, Δ₁⟩) => some (.ty .unit, ⟨none, Δ₁⟩)
                | none => none)
             | none => none)
          | _, _ => none
        else none
  | .indexDrop p idx πs =>
      -- (@Drop-Copy) §5.3 below a dynamic index: exactly the read's check,
      -- at type `unit` (`Typed.indexDrop`).
      match checkIdx P R Γ idx with
      | some (_, ⟨some Γ₁, Δ⟩) =>
        (match Γ₁[p.root]? with
         | none => none
         | some en =>
           match en.st.get p.path, en.ty.atPath P.decls p.path with
           | some u, some Ta =>
             (match Ta.atDyn P.decls πs with
              | some T =>
                  if idx.length = πs.length ∧ πs ≠ [] ∧ u.fullyOwned ∧
                      T.mult P.decls = .copy ∧
                      declaredPrefix P.decls en.ty p.path = none ∧
                      Ta.dynNoDeclared P.decls πs then some (.ty .unit, ⟨some Γ₁, Δ⟩)
                  else none
              | none => none)
           | _, _ => none)
      | some (_, ⟨none, Δ⟩) =>
        (match Γ[p.root]? with
         | none => none
         | some en =>
           match en.ty.atPath P.decls p.path with
           | some Ta =>
             (match Ta.atDyn P.decls πs with
              | some _ =>
                  if idx.length = πs.length ∧ πs ≠ [] then some (.ty .unit, ⟨none, Δ⟩) else none
              | none => none)
           | none => none)
      | none => none
  | .drop p =>
      match Γ[p.root]? with
      | none => none
      | some en =>
        match declaredPrefix P.decls en.ty p.path with
        | some (πd, πs) =>
            (match en.st.get πd, en.ty.atPath P.decls πd, en.ty.atPath P.decls p.path with
             | some u, some Td, some _T =>
                 if u.fullyOwned ∧ linearResidue P.decls Td πs = false ∧
                     noDtorPrefix P.decls en.ty p.path then
                   some (.ty .unit, ⟨some (Γ.set p.root (en.setSt (en.st.setAt πd .movedOut))), []⟩)
                 else none
             | _, _, _ => none)
        | none =>
            (match en.st.get p.path, en.ty.atPath P.decls p.path with
             | some u, some T =>
                 if T.mult P.decls = .copy then
                   (if u.fullyOwned then some (.ty .unit, ⟨some Γ, []⟩) else none)
                 else
                   (if u.isOwned ∧ noDtorPrefix P.decls en.ty p.path ∧
                       (u.fullyOwned = true ∨ residualLinearBelow P.decls u T = false) ∧
                       rootIdxOnly P.decls en.ty p.path then
                      some (.ty .unit, ⟨some (Γ.set p.root (en.setSt (en.st.setAt p.path .movedOut))), []⟩)
                    else none)
             | _, _ => none)
  | .letIn m e₁ e₂ =>
      match check P R Γ e₁ with
      | some (.ty T₁, ⟨some Γ₁, Δ₁⟩) =>
        (match check P R ({ ty := T₁, mu := m, st := .owned } :: Γ₁) e₂ with
         | some (c₂, ⟨some (en' :: Γ₂), Δ₂⟩) =>
             if residualLinear P.decls en'.st en'.ty then none
             else some (c₂, ⟨some Γ₂, Δ₂ ++ Δ₁⟩)
         | some (c₂, ⟨none, Δ₂⟩) => some (c₂, ⟨none, Δ₂ ++ Δ₁⟩)
         | _ => none)
      | some (_, ⟨none, Δ₁⟩) => some (.never, ⟨none, Δ₁⟩)
      | _ => none
  | .assign p e =>
      match Γ[p.root]? with
      | none => none
      | some en₀ =>
        if en₀.mu = true then
          match en₀.st.get p.path, en₀.ty.atPath P.decls p.path with
          | some _, some T =>
            (match check P R Γ e with
             | some (c, ⟨some Γ₁, Δ⟩) =>
               if c.fits T then
                 (match Γ₁[p.root]? with
                  | some en₁ =>
                    (match en₁.st.get p.path with
                     | some u₁ =>
                         if assignArrayOk P.decls en₁.st en₁.ty p.path ∧
                             overwriteOk P.decls u₁ T then
                           some (.ty .unit,
                             ⟨some (Γ₁.set p.root (en₁.setSt (en₁.st.setAt p.path .owned))), Δ⟩)
                         else none
                     | none => none)
                  | none => none)
               else none
             | some (_, ⟨none, Δ⟩) => some (.ty .unit, ⟨none, Δ⟩)
             | none => none)
          | _, _ => none
        else none
  | .seq e₁ e₂ =>
      match check P R Γ e₁ with
      | some (.ty T₁, ⟨some Γ₁, Δ₁⟩) =>
          if T₁.mult P.decls = .linear then none
          else
            (match check P R Γ₁ e₂ with
             | some (c₂, Ω₂) => some (c₂, Ω₂.add Δ₁)
             | none => none)
      | some (_, ⟨none, Δ₁⟩) => some (.never, ⟨none, Δ₁⟩)
      | _ => none
  | .ite c e₁ e₂ =>
      match check P R Γ c with
      | some (.ty .bool, ⟨some Γ₀, Δ₀⟩) =>
        (match check P R Γ₀ e₁, check P R Γ₀ e₂ with
        | some (c₁, Ω₁), some (c₂, Ω₂) =>
            (match CTy.meet c₁ c₂ with
             | some c' =>
               (match Ctx.joinOpt P.decls Ω₁.norm Ω₂.norm with
                | some o => some (c', ⟨o, Ω₁.brk ++ Ω₂.brk ++ Δ₀⟩)
                | none => none)
             | none => none)
        | _, _ => none)
      | some (cc, ⟨none, Δ₀⟩) => if cc.fits .bool then some (.never, ⟨none, Δ₀⟩) else none
      | _ => none
  | .call f args =>
      match P.fns[f]? with
      | none => none
      | some fd =>
        match checkArgs P R Γ args (fd.params.map Param.ty) with
        | some Ω => some (.ty fd.ret, Ω)
        | none => none
  | .ret e =>
      match check P R Γ e with
      | some (c, ⟨some Γ₁, Δ⟩) =>
          if c.fits R ∧ NoResidualLinear P.decls Γ₁ then some (.never, ⟨none, Δ⟩) else none
      | some (c, ⟨none, Δ⟩) => if c.fits R then some (.never, ⟨none, Δ⟩) else none
      | none => none
  | .brk => some (.never, ⟨none, [Γ]⟩)
  | .loop e =>
      -- §5.7: find the loop-head state by iteration, type the body there,
      -- and check that the head solves the equation the rules state; then
      -- the syntactic classification (`4.8:21`) picks the rule.
      match headIter P.decls (fun Γ' => (check P R Γ' e).map (fun r => r.2.norm)) Γ
          (e.nodes + 2) Γ with
      | none => none
      | some Γh =>
        match check P R Γh e with
        | some (c, Ωe) =>
          if c.fits .unit ∧ Ctx.joinOpt P.decls (some Γ) Ωe.norm = some (some Γh) ∧
              (Ωe.norm = none ∨ Ctx.Wf P.decls Γh) then
            if e.breaks then
              match Ωe.brk with
              | [] =>
                  if Ωe.norm = none ∨ NoResidualLinear P.decls Γh then
                    some (.ty .unit, ⟨none, []⟩)
                  else none
              | Γb₀ :: Γbs =>
                  if (Γb₀ :: Γbs).all
                      (fun Γb => decide (NoResidualLinear P.decls (Ctx.loopLocals Γh Γb))) then
                    match Ctx.joinAll P.decls ((Γb₀ :: Γbs).map (Ctx.outsideLoop Γh)) with
                    | some Γx => some (.ty .unit, ⟨some Γx, []⟩)
                    | none => none
                  else none
            else if Ωe.norm = none ∨ NoResidualLinear P.decls Γh then
              some (.never, ⟨none, []⟩)
            else none
          else none
        | none => none

/-- (Call) §5.8's argument list as an algorithm: each argument is checked
against its parameter's type with Σ threaded left to right, and the count must
match (`4.10:3`, `4.10:4`). An argument that diverges stops the list there
(§5.3's (Strict-Bottom)); the ones after it are not checked. -/
def checkArgs (P : Program) (R : Ty) : Ctx → List Expr → List Ty → Option Out
  | Γ, [], [] => some ⟨some Γ, []⟩
  | Γ, e :: es, T :: Ts =>
      match check P R Γ e with
      | some (c, ⟨some Γ₁, Δ₁⟩) =>
          if c.fits T then
            (match checkArgs P R Γ₁ es Ts with
             | some Ω => some (Ω.add Δ₁)
             | none => none)
          else none
      | some (c, ⟨none, Δ₁⟩) =>
          if c.fits T ∧ es.length = Ts.length then some ⟨none, Δ₁⟩ else none
      | none => none
  | _, _, _ => none

/-- The index expressions of a place below a dynamic index, as an algorithm:
each is checked at whatever integer type it has (`4.11:4`), left to right with
Σ threaded, and their types are returned for `Typed.indexRead`/`indexWrite`'s
`TypedArgs` premise. An index that diverges stops the list (§5.3's
(Strict-Bottom)); the unchecked indices after it are given its type, which is
any integer type `Typed`'s `TypedArgs.consBot` accepts for an untyped
member. -/
def checkIdx (P : Program) (R : Ty) : Ctx → List Expr → Option (List Ty × Out)
  | Γ, [] => some ([], ⟨some Γ, []⟩)
  | Γ, e :: es =>
      match check P R Γ e with
      | some (.ty (.int w s), ⟨some Γ₁, Δ₁⟩) =>
        (match checkIdx P R Γ₁ es with
         | some (Ts, Ω) => some (.int w s :: Ts, Ω.add Δ₁)
         | none => none)
      | some (.ty (.int w s), ⟨none, Δ₁⟩) =>
          some (.int w s :: es.map (fun _ => .int w s), ⟨none, Δ₁⟩)
      | _ => none

/-- The type (Match) §5.5's arms must share, as the algorithm picks it: the
**first** arm's that has a type, read under that arm's own payload locals, or
`never` when every arm diverges at `never`. §5.5 states the premise as one
type `T` for every arm and lets (Sub-Never) supply it for a diverging one, so
`check` fixes `T` here and compares the others against it — exactly what it
does for `ite`'s two arms (`CTy.meet`). -/
def firstArmTy (P : Program) (R : Ty) (Γ₀ : Ctx) :
    List Expr → List (List Ty) → CTy
  | e :: es, Ts :: Tss =>
      match check P R (armCtx Ts Γ₀) e with
      | some (.ty T, _) => .ty T
      | _ => firstArmTy P R Γ₀ es Tss
  | _, _ => .never

/-- (Match) §5.5's arm premises as an algorithm: every arm from the same
post-scrutinee state `Γ₀`, each under its variant's payload locals (`armCtx`),
each at the type `c` the first typed arm fixed, and each that continues
discharging §5.6 for the locals it pops. The result is one optional outgoing
context per arm — `none` for an arm that diverges — in declaration order, and
the arms' deliveries, which is what `Ctx.joinOpts` then folds. A count
mismatch between the arms and the variants is the last clause's `none` —
`check` has already required the counts to agree, so no program reaches it. -/
def checkArms (P : Program) (R : Ty) (Γ₀ : Ctx) (c : CTy) :
    List Expr → List (List Ty) → Option (List (Option Ctx) × List Ctx)
  | [], [] => some ([], [])
  | e :: es, Ts :: Tss =>
      match check P R (armCtx Ts Γ₀) e with
      | some (c', ⟨some Γb, Δb⟩) =>
          if c'.fitsC c ∧ NoResidualLinear P.decls (Γb.take Ts.length) then
            (match checkArms P R Γ₀ c es Tss with
             | some (os, Δs) => some (some (Γb.drop Ts.length) :: os, Δb ++ Δs)
             | none => none)
          else none
      | some (c', ⟨none, Δb⟩) =>
          if c'.fitsC c then
            (match checkArms P R Γ₀ c es Tss with
             | some (os, Δs) => some (none :: os, Δb ++ Δs)
             | none => none)
          else none
      | none => none
  | _, _ => none
end

/-- (Fn) §5.8 as an algorithm: the body checks at the declared return type
from the entry context `Γ0;Σ0` (`fnCtx`), and its normal exit edge discharges
§5.6's residual-linear obligation for the by-value parameters and every
still-open body-local binding (`3.8:62`). -/
def checkFn (P : Program) (fd : FnDef) : Bool :=
  match check P fd.ret (fnCtx fd) fd.body with
  | some (c, Ω) =>
      c.fits fd.ret &&
        (match Ω.norm with
         | some Γf => decide (NoResidualLinear P.decls Γf)
         | none => true) &&
        Ω.brk.isEmpty
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

/-- Whether a type's declaration is already grounded. An array is grounded
exactly when its element type is — `3.0:5` names array elements beside struct
fields and enum payloads, so `struct S { x0: [S; 1] }` must peel no further
than `struct S { x0: S }` does (E0483). A scalar names no declaration, so it
always is (helper). -/
def Ty.grounded (st : List Bool × List Bool) : Ty → Bool
  | .struct s => (st.1[s]?).getD false
  | .enum e => (st.2[e]?).getD false
  | .array T _ => Ty.grounded st T
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

end RueCore
