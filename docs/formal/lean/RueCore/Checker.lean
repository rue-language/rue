module

public import RueCore.Soundness
public import RueCore.Checker.Defs

@[expose] public section

/-!
# RueCore.Checker — a decidable checker for the §5 rules, proved sound

The `Typed` judgment is syntax-directed, so it has a computable image:
`check P R Γ e` either produces `(T, Γ')` or rejects. `check_sound` proves
every acceptance is backed by a real derivation — so the §7 safety theorems
apply to anything `check` accepts. This is the seed of the "second,
independent implementation" purpose of the formal core
(`docs/formal/README.md`): a reference, proved sound but not complete, for what the compiler's
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

* **A loop whose head iteration outlasts its bound** (`headIter`): refused.
  The argument in "The loop head, algorithmically" below says the bound is
  never reached, so this is a completeness limit only in principle.

The never-typed forms are `return`, `@panic`, `break` and a `break`-less
`loop` alike, so each shape above is one for all four.

A `never` *right* operand is fine — the left one has already fixed the type —
as is a `never` call argument, struct field, array element, condition or
scrutinee, whose position names its own type. Nothing a reader would write
turns on these, and `Gen.lean` puts a `break`, a `return` or a `@panic` only
where a whole arm or a block's last form stands, never in an operand
(RUE-2383).

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
has the shape: `Gen.lean` draws `break` only as the last form of an arm or of
a once-through loop body (RUE-2330), and `return` or `@panic` only as the last
form of an arm or of the function body (RUE-2383), with at most one diverging
arm per branch.

The algorithm itself (`check` … `checkProgram`) is in `Checker/Defs.lean`,
the definitions layer; this module proves it sound (README, "Layers").
-/

namespace RueCore

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
