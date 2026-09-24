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
the arm pops, which is `letIn`'s check read over a whole payload, for an arm
that continues. **The folded join**: `Ctx.joinOpts`, the fold `Ctx.joinAll`
over the continuing arms' outgoing contexts in declaration order.

The one thing `check` must *choose* is the arms' shared type, since §5.5 states
it as one `T` and lets (Sub-Never) coerce a diverging arm to it. `firstArmTy`
takes the first arm's that has one — skipping an arm whose type is `never` —
and every other arm is compared against that (`CTy.fitsC`), the same choice
`ite` makes for its two arms (`CTy.meet`). The arms are therefore checked
twice, which costs time and nothing else.

## `⊥`, algorithmically

`check` computes §5.3's outgoing result `Ω` (`Out`) and a type that may be
`never` (`CTy`). A form the rules type at `⊥` — `return`, `@panic`, and every
`-Bottom` rule — produces `⟨none, Δ⟩`, and the forms that consume a result
stop exactly where the rules do: a diverging operand ends a strict context
((Strict-Bottom) §5.3), a diverging prefix ends a sequence or a `let`
((Seq-Bottom), (Let-Bottom)), and a branch joins only the arms that continue
(`Ctx.joinOpt`, `Ctx.joinOpts`, §5.5). A `never` result is one whose rule
concludes at every type, and `check_sound` says so: `check P R Γ e = some (c,
Ω)` gives a derivation at every type `c` admits (`CTy.fits`).

Before RUE-2368 `check` had no `⊥`: it concluded a `return` or `@panic` at the
enclosing return type `R` and at the state in force, and handed that state to
the join, where §5.7 contributes nothing. That refused five shapes the
judgment derives and the compiler accepts, and they are seeded now
(`Corpus.lean`): a `return` arm after the arm dropped a binding the other arm
keeps (`if_return_arm_affine`), the same at a `match` over a linear binding
(`match_return_arm_linear`), a `match` whose first arm is a `return` and whose
type is its second arm's (`match_never_first_arm`), a `@panic` arm beside an
arm that consumes a linear binding (`if_panic_arm_linear`), and a `@panic`
past a live linear binding (`panic_past_linear`, `Examples.panicPastLinear`).

## What completeness still costs

`check_sound` holds, and completeness — `Typed` implies `check` succeeds — is
still **false**. Every shape where `check` refuses a derivable program is a
`never` operand in a position `check` reads a type off, or a syntactic premise
it keeps where a `-Bottom` rule drops it:

* **A `never` operator operand**, read left to right: the left operand of a
  binary operator, and the operand of `-`, `!` and `~`. (Strict-Bottom) §5.3
  concludes at the construct's own type `T_E`; for `+` or `-` that type is read
  off the operand's, so `(return 1) + 2` is derivable at every integer type
  and `check` has no one type to name. For `!` (always `bool`) and a
  comparison (`(return 1) < 2` is `bool` at every width) `T_E` is fixed, and
  the refusal is a simplification of the algorithm, not a limit of the rules.
* **A `never` operand to a form that fixes `T_E` itself**: `@intCast`,
  `@int_to_float`, the float intrinsics, `@dbg` and the repeat form. A
  derivation exists; `check` refuses for simplicity.
* **A `never` index expression**, in a read, a write or a `@drop` below a
  dynamic index (`a[return 8]`): `checkIdx` requires each index at an integer
  type, and `TypedArgs.consBot` needs none.
* **An assignment whose right-hand side diverges into a root `check` refuses**:
  `assignBot`, `indexWriteBotRhs` and `indexWriteBotIdx` omit (Assign)'s
  `μ = mut` root (their docstrings say why), and `check` still demands it.

A `never` *right* operand is fine — the left one has already fixed the type —
as is a `never` call argument, struct field, array element, condition or
scrutinee, whose position names its own type. Nothing a reader would write
turns on these, and `Gen.lean` emits no `return` or `@panic`.

## Dead code: accepted here, rejected by the compiler

The converse gap is new with `⊥`, and it is the calculus's, not the
algorithm's. (Seq-Bottom), (Let-Bottom) and (Strict-Bottom) type **nothing**
past a diverging subexpression, so code after a `return` or `@panic` — the
tail of a sequence or a `let`, a later argument or operand, the arms of a
branch whose condition or scrutinee diverges — is not checked at all. §5.3
says so: "the surface checker may still analyze `e2` to issue
unreachable-code diagnostics and report ordinary errors in unreachable source,
but that analysis does not create a reachable ownership path". The compiler
does analyze it, and rejects ill-formed dead code: a type error (E0206), a
use after move (E0205), a linear leak or discard (E0406, E0478), an
assignment to an unmarked binding (E0203) and a missing `match` arm (E0600)
after a `return` or `@panic` are all compile errors there, while `check`
accepts every one (the RUE-2368 review's probes q02–q07, q15, q17, q18, q23,
q25, q28). Such programs are well-typed by the calculus and run safely; the
compiler is within §5.3's licence to reject them. So an `accept` verdict says
nothing about the compiler's verdict on a program with syntax after a
diverging form, and `Corpus.lean`'s verdict contract excludes that shape.
Whether the core should say more about errors in unreachable code is a
question for the calculus, not settled here. No seed and no generated case
has the shape today; a generator that emits `break` (RUE-2369) must not put
syntax after a diverging form, or the bridge must skip such cases.
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
fixpoint). None of this is needed for soundness: `check` re-checks the body
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

/-- (helper) `check`'s result type `ty X` admits exactly `X`. -/
theorem CTy.eq_of_fits {T' T : Ty} (h : (CTy.ty T').fits T = true) : T' = T := by
  simpa [CTy.fits] using h

/-- (helper) A type admits itself. -/
theorem CTy.fits_self (T : Ty) : (CTy.ty T).fits T = true := by simp [CTy.fits]

/-- (helper) `never` admits every type — (Sub-Never) §5.7. -/
theorem CTy.fits_never (T : Ty) : CTy.never.fits T = true := rfl

/-- (helper) `pick` chooses a type the checked type admits. -/
theorem CTy.fits_pick (c : CTy) (d : Ty) : c.fits (c.pick d) = true := by
  cases c <;> simp [CTy.fits, CTy.pick]

/-- (helper) An arm whose type fits the one `firstArmTy` fixed admits every type
that one admits. -/
theorem CTy.fitsC_fits {c' c : CTy} {T : Ty} (h : c'.fitsC c = true) (hT : c.fits T = true) :
    c'.fits T = true := by
  cases c' <;> cases c <;> simp_all [CTy.fitsC, CTy.fits]

/-- (helper) Two arms whose types meet both admit whatever their meet admits. -/
theorem CTy.meet_fits {c₁ c₂ c' : CTy} {T : Ty} (h : CTy.meet c₁ c₂ = some c')
    (hT : c'.fits T = true) : c₁.fits T = true ∧ c₂.fits T = true := by
  cases c₁ with
  | never =>
      simp only [CTy.meet, Option.some.injEq] at h
      subst h; exact ⟨rfl, hT⟩
  | ty T₁ =>
    cases c₂ with
    | never =>
        simp only [CTy.meet, Option.some.injEq] at h
        subst h; exact ⟨hT, rfl⟩
    | ty T₂ =>
        simp only [CTy.meet] at h
        split at h
        · rename_i heq
          simp only [Option.some.injEq] at h
          subst h; subst heq; exact ⟨hT, hT⟩
        · cases h

/-- (helper) Close a `check_sound` case whose result type is an ordinary type:
the only type it admits is that one. -/
local macro "fin_ty" : tactic => `(tactic| (intro T hT; cases CTy.eq_of_fits hT))

mutual
/-- Every `check` acceptance is a real derivation of the §5 judgment, at every
type the result admits (a `never` result at every type, (Sub-Never) §5.7), so
the §7 theorems apply to whatever `check` accepts. -/
theorem check_sound {P : Program} {R : Ty} : ∀ (e : Expr) {Γ : Ctx} {c : CTy} {Ω : Out},
    check P R Γ e = some (c, Ω) → ∀ T, c.fits T = true → Typed P R Γ e T Ω
  | .intLit w s n, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · cases h; fin_ty; exact .intLit ‹_›
      · cases h
  | .boolLit b, Γ, c, Ω, h => by
      simp only [check] at h; cases h; fin_ty; exact .boolLit
  | .unitLit, Γ, c, Ω, h => by
      simp only [check] at h; cases h; fin_ty; exact .unitLit
  | .use pl, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · cases h
      · rename_i en hen
        cases hplan : declaredPrefix P.decls en.ty pl.path with
        | some r =>
            obtain ⟨πd, πs⟩ := r
            simp only [hplan] at h
            split at h
            · rename_i u Td T₀ hgd htd hty
              split at h
              · rename_i hprem
                cases h; fin_ty
                exact .useDeclared hen hplan hgd hprem.1 htd hprem.2.1 hty hprem.2.2
              · cases h
            · cases h
        | none =>
            simp only [hplan] at h
            split at h
            · rename_i u T₀ hg hty
              split at h
              · rename_i hcopy
                split at h
                · rename_i hfo
                  cases h; fin_ty
                  exact .useCopy hen hg hfo hty hcopy hplan
                · cases h
              · rename_i hncopy
                split at h
                · rename_i hprem
                  cases h; fin_ty
                  exact .useMove hen hg hprem.1 hty hncopy hprem.2.1 hplan hprem.2.2
                · cases h
            · cases h
  | .binop op e₁ e₂, Γ, c, Ω, h => by
      simp only [check] at h
      cases h₁ : check P R Γ e₁ with
      | none => simp [h₁] at h
      | some r₁ =>
        obtain ⟨c₁, o₁, Δ₁⟩ := r₁
        cases c₁ with
        | never => simp [h₁] at h
        | ty T₁ =>
          cases T₁ with
          | int w s =>
            cases o₁ with
            | none =>
                simp only [h₁] at h
                split at h
                · cases h; fin_ty
                  exact .binopBot (check_sound e₁ h₁ _ (CTy.fits_self _)) ‹_›
                · cases h
            | some Γ₁ =>
                simp only [h₁] at h
                cases h₂ : check P R Γ₁ e₂ with
                | none => simp [h₂] at h
                | some r₂ =>
                  obtain ⟨c₂, Ω₂⟩ := r₂
                  cases c₂ with
                  | never =>
                      simp only [h₂] at h
                      split at h
                      · cases h; fin_ty
                        exact .binop (check_sound e₁ h₁ _ (CTy.fits_self _))
                          (check_sound e₂ h₂ _ rfl) ‹_›
                      · cases h
                  | ty T₂ =>
                    cases T₂ with
                    | int w' s' =>
                        simp only [h₂] at h
                        split at h
                        · rename_i hws
                          obtain ⟨rfl, rfl, hadm⟩ := hws
                          cases h; fin_ty
                          exact .binop (check_sound e₁ h₁ _ (CTy.fits_self _))
                            (check_sound e₂ h₂ _ (CTy.fits_self _)) hadm
                        · cases h
                    | float _ | bool | unit | struct _ | enum _ | array _ _ => simp [h₂] at h
          | float w =>
            cases o₁ with
            | none =>
                simp only [h₁] at h
                split at h
                · cases h; fin_ty
                  exact .floatBinopBot (check_sound e₁ h₁ _ (CTy.fits_self _)) ‹_›
                · cases h
            | some Γ₁ =>
                simp only [h₁] at h
                cases h₂ : check P R Γ₁ e₂ with
                | none => simp [h₂] at h
                | some r₂ =>
                  obtain ⟨c₂, Ω₂⟩ := r₂
                  cases c₂ with
                  | never =>
                      simp only [h₂] at h
                      split at h
                      · cases h; fin_ty
                        exact .floatBinop (check_sound e₁ h₁ _ (CTy.fits_self _))
                          (check_sound e₂ h₂ _ rfl) ‹_›
                      · cases h
                  | ty T₂ =>
                    cases T₂ with
                    | float w' =>
                        simp only [h₂] at h
                        split at h
                        · rename_i hws
                          obtain ⟨rfl, hadm⟩ := hws
                          cases h; fin_ty
                          exact .floatBinop (check_sound e₁ h₁ _ (CTy.fits_self _))
                            (check_sound e₂ h₂ _ (CTy.fits_self _)) hadm
                        · cases h
                    | int _ _ | bool | unit | struct _ | enum _ | array _ _ => simp [h₂] at h
          | bool | unit | struct _ | enum _ | array _ _ => simp [h₁] at h
  | .floatLit w l, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · cases h; fin_ty; exact .floatLit ‹_›
      · cases h
  | .fintrin (.intToFloat w) e, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · cases h; fin_ty; exact .intToFloat (check_sound e ‹_› _ (CTy.fits_self _))
      · cases h
  | .fintrin (.floatToInt w s) e, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · split at h
        · cases h; fin_ty
          exact .floatIntrin (check_sound e ‹_› _ (CTy.fits_self _)) (by simpa using ‹_›)
        · cases h
      · cases h
  | .fintrin (.floatCast w) e, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · split at h
        · cases h; fin_ty
          exact .floatIntrin (check_sound e ‹_› _ (CTy.fits_self _)) (by simpa using ‹_›)
        · cases h
      · cases h
  | .fintrin (.roundOp k) e, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · split at h
        · cases h; fin_ty
          exact .floatIntrin (check_sound e ‹_› _ (CTy.fits_self _)) (by simpa using ‹_›)
        · cases h
      · cases h
  | .unop .neg e, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · cases h; fin_ty; exact .neg (check_sound e ‹_› _ (CTy.fits_self _))
      · cases h; fin_ty; exact .floatNeg (check_sound e ‹_› _ (CTy.fits_self _))
      · cases h
  | .unop .not e, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · cases h; fin_ty; exact .notOp (check_sound e ‹_› _ (CTy.fits_self _))
      · cases h
  | .unop .bitnot e, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · cases h; fin_ty; exact .bitnot (check_sound e ‹_› _ (CTy.fits_self _))
      · cases h
  | .intCast w s e, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · cases h; fin_ty; exact .intCast (check_sound e ‹_› _ (CTy.fits_self _))
      · cases h
  | .panic msg, Γ, c, Ω, h => by
      simp only [check] at h
      cases h
      intro T _
      exact .panic
  | .dbg e, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · rename_i T₁ Ω₁ h₁
        split at h
        · cases h; fin_ty; exact .dbg (check_sound e h₁ _ (CTy.fits_self _)) ‹_›
        · cases h
      · cases h
  | .mkStruct s args, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · cases h
      · rename_i sd hsd
        split at h
        · rename_i Ω₁ hargs
          cases h; fin_ty
          exact .mkStruct hsd (checkArgs_sound args hargs)
        · cases h
  | .mkEnum e k args, Γ, c, Ω, h => by
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
          | some Ω₁ =>
              simp only [hargs] at h
              cases h; fin_ty
              exact .mkEnum hed hv (checkArgs_sound args hargs)
  | .«match» scrut arms, Γ, c, Ω, h => by
      simp only [check] at h
      cases hscrut : check P R Γ scrut with
      | none => simp [hscrut] at h
      | some r =>
        obtain ⟨csc, o₀, Δ₀⟩ := r
        cases csc with
        | never =>
          cases o₀ with
          | some Γ₀ => simp [hscrut] at h
          | none =>
              simp only [hscrut, Option.some.injEq, Prod.mk.injEq] at h
              obtain ⟨rfl, rfl⟩ := h
              intro T _
              exact .matchBot (e := 0) (check_sound scrut hscrut _ rfl)
        | ty Tsc =>
          cases Tsc with
          | int _ _ | float _ | bool | unit | struct _ | array _ _ => simp [hscrut] at h
          | enum e =>
            cases o₀ with
            | none =>
                simp only [hscrut, Option.some.injEq, Prod.mk.injEq] at h
                obtain ⟨rfl, rfl⟩ := h
                intro T _
                exact .matchBot (check_sound scrut hscrut _ (CTy.fits_self _))
            | some Γ₀ =>
              simp only [hscrut] at h
              cases hed : P.decls.enums[e]? with
              | none => simp only [hed] at h; cases h
              | some ed =>
                simp only [hed] at h
                by_cases hlen : arms.length = ed.variants.length
                · simp only [if_pos hlen] at h
                  cases harms : checkArms P R Γ₀ (firstArmTy P R Γ₀ arms ed.variants) arms
                      ed.variants with
                  | none => simp only [harms] at h; cases h
                  | some r =>
                    obtain ⟨os, Δs⟩ := r
                    simp only [harms] at h
                    cases hjoin : Ctx.joinOpts P.decls os with
                    | none => simp only [hjoin] at h; cases h
                    | some o =>
                        simp only [hjoin, Option.some.injEq, Prod.mk.injEq] at h
                        obtain ⟨rfl, rfl⟩ := h
                        intro T hT
                        exact .«match» (check_sound scrut hscrut _ (CTy.fits_self _)) hed hlen
                          (checkArms_sound arms harms T hT) hjoin
                · simp only [if_neg hlen] at h; cases h
  | .mkArray Te args, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · rename_i Ω₁ hargs
        cases h; fin_ty
        exact .mkArray (checkArgs_sound args hargs)
      · cases h
  | .repeatArray Te e n, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · rename_i T' Ω₁ hchk
        split at h
        · rename_i hprem
          obtain ⟨hT, hcopy⟩ := hprem
          subst hT
          cases h; fin_ty
          exact .repeatArray (check_sound e hchk _ (CTy.fits_self _)) hcopy
        · cases h
      · cases h
  | .indexRead pl idx πs, Γ, c, Ω, h => by
      simp only [check] at h
      cases hidx : checkIdx P R Γ idx with
      | none => simp [hidx] at h
      | some r =>
        obtain ⟨Ts, o₁, Δ⟩ := r
        obtain ⟨hta, hint⟩ := checkIdx_sound idx hidx
        cases o₁ with
        | some Γ₁ =>
          simp only [hidx] at h
          split at h
          · cases h
          · rename_i en hen
            split at h
            · rename_i u Ta hg hty
              split at h
              · rename_i T₀ hdyn
                split at h
                · rename_i hprem
                  obtain ⟨hlen, hne, hfo, hcopy, hplan, hnd⟩ := hprem
                  cases h; fin_ty
                  exact .indexRead hta hint hlen hne hen hg hfo hty hdyn hcopy hplan hnd
                · cases h
              · cases h
            · cases h
        | none =>
          simp only [hidx] at h
          split at h
          · cases h
          · rename_i en hen
            split at h
            · rename_i Ta hty
              split at h
              · rename_i T₀ hdyn
                split at h
                · rename_i hprem
                  cases h; fin_ty
                  exact .indexReadBot hta hint hprem.1 hprem.2 hen hty hdyn
                · cases h
              · cases h
            · cases h
  | .indexWrite pl idx πs e, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · cases h
      · rename_i en₀ hget₀
        split at h
        · rename_i hmu
          split at h
          · rename_i u₀ Ta hg₀ hty₀
            split at h
            · rename_i T₀ hdyn
              cases hchk : check P R Γ e with
              | none => simp [hchk] at h
              | some r =>
                obtain ⟨ce, o₁, Δ₁⟩ := r
                cases o₁ with
                | none =>
                    simp only [hchk, Option.some.injEq, Prod.mk.injEq] at h
                    obtain ⟨rfl, rfl⟩ := h
                    fin_ty
                    exact .indexWriteBotRhs (check_sound e hchk _ (CTy.fits_pick ce T₀))
                | some Γ₁ =>
                  simp only [hchk] at h
                  split at h
                  · rename_i hT
                    cases hidx : checkIdx P R Γ₁ idx with
                    | none => simp [hidx] at h
                    | some r₂ =>
                      obtain ⟨Ts, o₂, Δ₂⟩ := r₂
                      obtain ⟨hta, hint⟩ := checkIdx_sound idx hidx
                      cases o₂ with
                      | none =>
                          simp only [hidx, Option.some.injEq, Prod.mk.injEq] at h
                          obtain ⟨rfl, rfl⟩ := h
                          fin_ty
                          exact .indexWriteBotIdx hget₀ hty₀ hdyn (check_sound e hchk _ hT) hta hint
                      | some Γ₂ =>
                        simp only [hidx] at h
                        split at h
                        · rename_i en₁ hget₁
                          split at h
                          · rename_i u₁ hg₁
                            split at h
                            · rename_i hpost
                              obtain ⟨hlen, hne, hfo, harr, hnl⟩ := hpost
                              cases h; fin_ty
                              exact .indexWrite hget₀ hmu hg₀ hty₀ hdyn hlen hne
                                (check_sound e hchk _ hT) hta hint hget₁ hg₁ hfo harr hnl
                            · cases h
                          · cases h
                        · cases h
                  · cases h
            · cases h
          · cases h
        · cases h
  | .indexDrop pl idx πs, Γ, c, Ω, h => by
      simp only [check] at h
      cases hidx : checkIdx P R Γ idx with
      | none => simp [hidx] at h
      | some r =>
        obtain ⟨Ts, o₁, Δ⟩ := r
        obtain ⟨hta, hint⟩ := checkIdx_sound idx hidx
        cases o₁ with
        | some Γ₁ =>
          simp only [hidx] at h
          split at h
          · cases h
          · rename_i en hen
            split at h
            · rename_i u Ta hg hty
              split at h
              · rename_i T₀ hdyn
                split at h
                · rename_i hprem
                  obtain ⟨hlen, hne, hfo, hcopy, hplan, hnd⟩ := hprem
                  cases h; fin_ty
                  exact .indexDrop
                    (.indexRead hta hint hlen hne hen hg hfo hty hdyn hcopy hplan hnd)
                · cases h
              · cases h
            · cases h
        | none =>
          simp only [hidx] at h
          split at h
          · cases h
          · rename_i en hen
            split at h
            · rename_i Ta hty
              split at h
              · rename_i T₀ hdyn
                split at h
                · rename_i hprem
                  cases h; fin_ty
                  exact .indexDrop (.indexReadBot hta hint hprem.1 hprem.2 hen hty hdyn)
                · cases h
              · cases h
            · cases h
  | .drop pl, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · cases h
      · rename_i en hen
        cases hplan : declaredPrefix P.decls en.ty pl.path with
        | some r =>
            obtain ⟨πd, πs⟩ := r
            simp only [hplan] at h
            split at h
            · rename_i u Td T₀ hgd htd hty
              split at h
              · rename_i hprem
                cases h; fin_ty
                exact .dropDeclared hen hplan hgd hprem.1 htd hprem.2.1 hty hprem.2.2
              · cases h
            · cases h
        | none =>
            simp only [hplan] at h
            split at h
            · rename_i u T₀ hg hty
              split at h
              · rename_i hcopy
                split at h
                · rename_i hfo
                  cases h; fin_ty
                  exact .dropCopy hen hg hfo hty hcopy hplan
                · cases h
              · rename_i hncopy
                split at h
                · rename_i hprem
                  cases h; fin_ty
                  exact .dropRes hen hg hprem.1 hty hncopy hprem.2.1 hplan hprem.2.2.1
                    hprem.2.2.2
                · cases h
            · cases h
  | .letIn m e₁ e₂, Γ, c, Ω, h => by
      simp only [check] at h
      cases h₁ : check P R Γ e₁ with
      | none => simp [h₁] at h
      | some r₁ =>
        obtain ⟨c₁, o₁, Δ₁⟩ := r₁
        cases o₁ with
        | none =>
            simp only [h₁, Option.some.injEq, Prod.mk.injEq] at h
            obtain ⟨rfl, rfl⟩ := h
            intro T _
            exact .letBot (check_sound e₁ h₁ _ (CTy.fits_pick c₁ .unit))
        | some Γ₁ =>
          cases c₁ with
          | never => simp [h₁] at h
          | ty T₁ =>
            simp only [h₁] at h
            cases h₂ : check P R ({ ty := T₁, mu := m, st := .owned } :: Γ₁) e₂ with
            | none => simp [h₂] at h
            | some r₂ =>
              obtain ⟨c₂, o₂, Δ₂⟩ := r₂
              cases o₂ with
              | none =>
                  simp only [h₂, Option.some.injEq, Prod.mk.injEq] at h
                  obtain ⟨rfl, rfl⟩ := h
                  intro T hT
                  exact .letInDiv (check_sound e₁ h₁ _ (CTy.fits_self _)) (check_sound e₂ h₂ T hT)
              | some Γb =>
                cases Γb with
                | nil => simp [h₂] at h
                | cons en' Γ₂ =>
                  simp only [h₂] at h
                  split at h
                  · cases h
                  · rename_i hres
                    simp only [Option.some.injEq, Prod.mk.injEq] at h
                    obtain ⟨rfl, rfl⟩ := h
                    intro T hT
                    exact .letIn (check_sound e₁ h₁ _ (CTy.fits_self _)) (check_sound e₂ h₂ T hT)
                      ((Bool.not_eq_true _).mp hres)
  | .assign pl e, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · cases h
      · rename_i en₀ hget₀
        split at h
        · rename_i hmu
          split at h
          · rename_i u₀ T₀ hg₀ hty₀
            cases hchk : check P R Γ e with
            | none => simp [hchk] at h
            | some r =>
              obtain ⟨ce, o₁, Δ⟩ := r
              cases o₁ with
              | none =>
                  simp only [hchk, Option.some.injEq, Prod.mk.injEq] at h
                  obtain ⟨rfl, rfl⟩ := h
                  fin_ty
                  exact .assignBot (check_sound e hchk _ (CTy.fits_pick ce T₀))
              | some Γ₁ =>
                simp only [hchk] at h
                split at h
                · rename_i hT
                  split at h
                  · rename_i en₁ hget₁
                    split at h
                    · rename_i u₁ hg₁
                      split at h
                      · rename_i hover
                        cases h; fin_ty
                        exact .assign hget₀ hmu hg₀ hty₀ (check_sound e hchk _ hT) hget₁ hg₁
                          hover.1 (overwriteOk_iff.mp hover.2)
                      · cases h
                    · cases h
                  · cases h
                · cases h
          · cases h
        · cases h
  | .seq e₁ e₂, Γ, c, Ω, h => by
      simp only [check] at h
      cases h₁ : check P R Γ e₁ with
      | none => simp [h₁] at h
      | some r₁ =>
        obtain ⟨c₁, o₁, Δ₁⟩ := r₁
        cases o₁ with
        | none =>
            simp only [h₁, Option.some.injEq, Prod.mk.injEq] at h
            obtain ⟨rfl, rfl⟩ := h
            intro T _
            exact .seqBot (check_sound e₁ h₁ _ (CTy.fits_pick c₁ .unit))
        | some Γ₁ =>
          cases c₁ with
          | never => simp [h₁] at h
          | ty T₁ =>
            simp only [h₁] at h
            split at h
            · cases h
            · rename_i hnl
              cases h₂ : check P R Γ₁ e₂ with
              | none => simp [h₂] at h
              | some r₂ =>
                  obtain ⟨c₂, Ω₂⟩ := r₂
                  simp only [h₂, Option.some.injEq, Prod.mk.injEq] at h
                  obtain ⟨rfl, rfl⟩ := h
                  intro T hT
                  exact .seq (check_sound e₁ h₁ _ (CTy.fits_self _)) hnl (check_sound e₂ h₂ T hT)
  | .ite cnd e₁ e₂, Γ, c, Ω, h => by
      simp only [check] at h
      cases hc : check P R Γ cnd with
      | none => simp [hc] at h
      | some r₀ =>
        obtain ⟨cc, o₀, Δ₀⟩ := r₀
        cases o₀ with
        | none =>
            simp only [hc] at h
            split at h
            · rename_i hb
              simp only [Option.some.injEq, Prod.mk.injEq] at h
              obtain ⟨rfl, rfl⟩ := h
              intro T _
              exact .iteBot (check_sound cnd hc _ hb)
            · cases h
        | some Γ₀ =>
          cases cc with
          | never => simp [hc] at h
          | ty Tc =>
            cases Tc with
            | int _ _ | float _ | unit | struct _ | enum _ | array _ _ => simp [hc] at h
            | bool =>
              simp only [hc] at h
              cases h₁ : check P R Γ₀ e₁ with
              | none => simp [h₁] at h
              | some r₁ =>
                cases h₂ : check P R Γ₀ e₂ with
                | none => simp [h₁, h₂] at h
                | some r₂ =>
                  obtain ⟨c₁, Ω₁⟩ := r₁
                  obtain ⟨c₂, Ω₂⟩ := r₂
                  simp only [h₁, h₂] at h
                  cases hm : CTy.meet c₁ c₂ with
                  | none => simp [hm] at h
                  | some c' =>
                    simp only [hm] at h
                    cases hj : Ctx.joinOpt P.decls Ω₁.norm Ω₂.norm with
                    | none => simp [hj] at h
                    | some o =>
                        simp only [hj, Option.some.injEq, Prod.mk.injEq] at h
                        obtain ⟨rfl, rfl⟩ := h
                        intro T hT
                        obtain ⟨hT₁, hT₂⟩ := CTy.meet_fits hm hT
                        exact .ite (check_sound cnd hc _ (CTy.fits_self _))
                          (check_sound e₁ h₁ T hT₁) (check_sound e₂ h₂ T hT₂) hj
  | .call f args, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · cases h
      · rename_i fd hfd
        split at h
        · rename_i Ω₁ hargs
          cases h; fin_ty
          exact .call hfd (checkArgs_sound args hargs)
        · cases h
  | .ret e, Γ, c, Ω, h => by
      simp only [check] at h
      cases hchk : check P R Γ e with
      | none => simp [hchk] at h
      | some r =>
        obtain ⟨ce, o₁, Δ⟩ := r
        cases o₁ with
        | none =>
            simp only [hchk] at h
            split at h
            · rename_i hR
              simp only [Option.some.injEq, Prod.mk.injEq] at h
              obtain ⟨rfl, rfl⟩ := h
              intro T _
              exact .retBot (check_sound e hchk R hR)
            · cases h
        | some Γ₁ =>
            simp only [hchk] at h
            split at h
            · rename_i hcond
              simp only [Option.some.injEq, Prod.mk.injEq] at h
              obtain ⟨rfl, rfl⟩ := h
              intro T _
              exact .ret (check_sound e hchk R hcond.1) hcond.2
            · cases h
  | .brk, Γ, c, Ω, h => by
      simp only [check, Option.some.injEq, Prod.mk.injEq] at h
      obtain ⟨rfl, rfl⟩ := h
      intro T _
      exact .brk
  | .loop e, Γ, c, Ω, h => by
      simp only [check] at h
      split at h
      · cases h
      · rename_i Γh _
        cases hchk : check P R Γh e with
        | none => simp [hchk] at h
        | some r =>
          obtain ⟨ce, Ωe⟩ := r
          simp only [hchk] at h
          split at h
          · rename_i hcond
            obtain ⟨hfit, hjoin, hwfh⟩ := hcond
            have hbody := check_sound e hchk .unit hfit
            have hhead : LoopHead P.decls Γ Ωe.norm Γh :=
              ⟨hjoin, fun Γe hn => hwfh.resolve_left (by rw [hn]; simp)⟩
            split at h
            · rename_i hb
              split at h
              · rename_i hnil
                split at h
                · rename_i hdiv
                  simp only [Option.some.injEq, Prod.mk.injEq] at h
                  obtain ⟨rfl, rfl⟩ := h
                  fin_ty
                  exact .loopBreakDiv hbody hhead hb hnil
                    (fun Γe hn => hdiv.resolve_left (by rw [hn]; simp))
                · cases h
              · rename_i Γb₀ Γbs hcons
                split at h
                · rename_i hall
                  split at h
                  · rename_i Γx hjx
                    simp only [Option.some.injEq, Prod.mk.injEq] at h
                    obtain ⟨rfl, rfl⟩ := h
                    fin_ty
                    rw [← hcons] at hall hjx
                    exact .loopBreak hbody hhead hb
                      (fun Γb hΓb => of_decide_eq_true (List.all_eq_true.mp hall Γb hΓb)) hjx
                  · cases h
                · cases h
            · rename_i hb
              split at h
              · rename_i hdiv
                simp only [Option.some.injEq, Prod.mk.injEq] at h
                obtain ⟨rfl, rfl⟩ := h
                intro T _
                exact .loopDiv hbody hhead (by simpa using hb)
                  (fun Γe hn => hdiv.resolve_left (by rw [hn]; simp))
              · cases h
          · cases h

/-- Every `checkIdx` acceptance is a real index-list derivation at integer
types (`4.11:4`) (helper). -/
theorem checkIdx_sound {P : Program} {R : Ty} : ∀ (es : List Expr) {Γ : Ctx} {Ts Ω},
    checkIdx P R Γ es = some (Ts, Ω) → TypedArgs P R Γ es Ts Ω ∧ Ts.all Ty.isInt = true
  | [], Γ, Ts, Ω, h => by
      simp only [checkIdx, Option.some.injEq, Prod.mk.injEq] at h
      obtain ⟨rfl, rfl⟩ := h
      exact ⟨.nil, rfl⟩
  | e :: es, Γ, Ts, Ω, h => by
      simp only [checkIdx] at h
      cases hchk : check P R Γ e with
      | none => simp [hchk] at h
      | some r =>
        obtain ⟨ce, o₁, Δ₁⟩ := r
        cases ce with
        | never => simp [hchk] at h
        | ty T₁ =>
          cases T₁ with
          | float _ | bool | unit | struct _ | enum _ | array _ _ => simp [hchk] at h
          | int w sg =>
            cases o₁ with
            | none =>
                simp only [hchk, Option.some.injEq, Prod.mk.injEq] at h
                obtain ⟨rfl, rfl⟩ := h
                refine ⟨.consBot (check_sound e hchk _ (CTy.fits_self _)) (by simp), ?_⟩
                simp [Ty.isInt]
            | some Γ₁ =>
                simp only [hchk] at h
                cases hrest : checkIdx P R Γ₁ es with
                | none => simp [hrest] at h
                | some r' =>
                  obtain ⟨Ts', Ω'⟩ := r'
                  simp only [hrest, Option.some.injEq, Prod.mk.injEq] at h
                  obtain ⟨rfl, rfl⟩ := h
                  obtain ⟨hta, hint⟩ := checkIdx_sound es hrest
                  exact ⟨.cons (check_sound e hchk _ (CTy.fits_self _)) hta,
                    by simp [Ty.isInt, hint]⟩

/-- Every `checkArms` acceptance is a real (Match) §5.5 arm-list derivation, at
every type the arms' fixed type admits. -/
theorem checkArms_sound {P : Program} {R : Ty} {Γ₀ : Ctx} {c : CTy} :
    ∀ (es : List Expr) {Tss : List (List Ty)} {os : List (Option Ctx)} {Δs : List Ctx},
    checkArms P R Γ₀ c es Tss = some (os, Δs) → ∀ T, c.fits T = true →
      TypedArms P R Γ₀ es Tss T os Δs
  | [], Tss, os, Δs, h => by
      cases Tss with
      | nil =>
          simp only [checkArms, Option.some.injEq, Prod.mk.injEq] at h
          obtain ⟨rfl, rfl⟩ := h
          intro T _; exact .noArms
      | cons _ _ => simp [checkArms] at h
  | e :: es, Tss, os, Δs, h => by
      cases Tss with
      | nil => simp [checkArms] at h
      | cons Ts Tss' =>
          simp only [checkArms] at h
          cases hchk : check P R (armCtx Ts Γ₀) e with
          | none => simp [hchk] at h
          | some r =>
            obtain ⟨c', o, Δb⟩ := r
            cases o with
            | some Γb =>
                simp only [hchk] at h
                split at h
                · rename_i hcond
                  cases hrest : checkArms P R Γ₀ c es Tss' with
                  | none => simp [hrest] at h
                  | some r' =>
                    obtain ⟨os', Δs'⟩ := r'
                    simp only [hrest, Option.some.injEq, Prod.mk.injEq] at h
                    obtain ⟨rfl, rfl⟩ := h
                    intro T hT
                    exact .arm (check_sound e hchk T (CTy.fitsC_fits hcond.1 hT)) hcond.2
                      (checkArms_sound es hrest T hT)
                · cases h
            | none =>
                simp only [hchk] at h
                split at h
                · rename_i hcond
                  cases hrest : checkArms P R Γ₀ c es Tss' with
                  | none => simp [hrest] at h
                  | some r' =>
                    obtain ⟨os', Δs'⟩ := r'
                    simp only [hrest, Option.some.injEq, Prod.mk.injEq] at h
                    obtain ⟨rfl, rfl⟩ := h
                    intro T hT
                    exact .armDiv (check_sound e hchk T (CTy.fitsC_fits hcond hT))
                      (checkArms_sound es hrest T hT)
                · cases h

/-- Every `checkArgs` acceptance is a real (Call) §5.8 argument-list
derivation. -/
theorem checkArgs_sound {P : Program} {R : Ty} : ∀ (es : List Expr) {Γ : Ctx} {Ts Ω},
    checkArgs P R Γ es Ts = some Ω → TypedArgs P R Γ es Ts Ω
  | [], Γ, Ts, Ω, h => by
      cases Ts with
      | nil => simp only [checkArgs, Option.some.injEq] at h; subst h; exact .nil
      | cons _ _ => simp [checkArgs] at h
  | e :: es, Γ, Ts, Ω, h => by
      cases Ts with
      | nil => simp [checkArgs] at h
      | cons T Ts' =>
          simp only [checkArgs] at h
          cases hchk : check P R Γ e with
          | none => simp [hchk] at h
          | some r =>
            obtain ⟨ce, o, Δ₁⟩ := r
            cases o with
            | none =>
                simp only [hchk] at h
                split at h
                · rename_i hcond
                  simp only [Option.some.injEq] at h
                  subst h
                  exact .consBot (check_sound e hchk T hcond.1) hcond.2
                · cases h
            | some Γ₁ =>
                simp only [hchk] at h
                split at h
                · rename_i hT
                  cases hrest : checkArgs P R Γ₁ es Ts' with
                  | none => simp [hrest] at h
                  | some Ω' =>
                      simp only [hrest, Option.some.injEq] at h
                      subst h
                      exact .cons (check_sound e hchk T hT) (checkArgs_sound es hrest)
                · cases h
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

/-- A grounded type's every named declaration is grounded: `Ty.declIds` peels
exactly the array wrappers `Ty.grounded` walks through (helper). -/
theorem Ty.grounded_declIds {st : List Bool × List Bool} :
    ∀ {T : Ty} {d : DeclId}, Ty.grounded st T = true → d ∈ T.declIds →
      Ty.grounded st d.ty = true
  | .struct _, _, h, hm => by
      simp only [Ty.declIds, List.mem_singleton] at hm; subst hm; exact h
  | .enum _, _, h, hm => by
      simp only [Ty.declIds, List.mem_singleton] at hm; subst hm; exact h
  | .array T _, _, h, hm =>
      Ty.grounded_declIds (T := T) h (by simpa only [Ty.declIds] using hm)
  | .int _ _, _, _, hm => by simp [Ty.declIds] at hm
  | .float _, _, _, hm => by simp [Ty.declIds] at hm
  | .bool, _, _, hm => by simp [Ty.declIds] at hm
  | .unit, _, _, hm => by simp [Ty.declIds] at hm

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
  · rename_i c Ω hchk
    simp only [Bool.and_eq_true, List.isEmpty_iff] at h
    obtain ⟨⟨hT, hnl⟩, hbrk⟩ := h
    refine ⟨Ω, check_sound fd.body hchk _ hT, fun Γf hn => ?_, hbrk⟩
    rw [hn] at hnl
    simpa using hnl
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
          obtain ⟨T, hT, hd'⟩ := hn
          exact Ty.grounded_declIds (h T hT) hd'
  | enum e =>
      simp only [DeclId.ty, Ty.grounded, Decls.peel, Decls.peelStep, List.getElem?_map] at h
      cases hd : D.enums[e]? with
      | none => rw [hd] at h; exact absurd h (by simp)
      | some ed =>
          rw [hd] at h
          simp only [Option.map_some, Option.getD_some, List.all_eq_true] at h
          simp only [Decls.Names, Decls.byValue, hd] at hn
          obtain ⟨T, hTmem, hd'⟩ := hn
          obtain ⟨Ts, hTs, hT⟩ := List.mem_flatten.mp hTmem
          exact Ty.grounded_declIds (h Ts hTs T hT) hd'

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
