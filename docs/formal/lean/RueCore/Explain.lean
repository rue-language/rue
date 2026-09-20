import RueCore.Corpus

/-!
# RueCore.Explain — derivations and runs, rendered for a reader (RUE-2246)

`check` (§5, `Checker.lean`) answers *whether* a program is well-formed and
`eval` (§6, `Dynamics.lean`) answers *what it does*; neither says **why**.
This module adds the two instrumented mirrors an explainability view needs:

* `explain`, which mirrors `check` and returns a `Deriv` — the derivation
  tree the calculus would draw, one node per rule, each carrying the rule's
  name as §5 writes it, the incoming `Γ;Σ`, the expression, the resulting
  type and outgoing `Γ;Σ`, and the sub-derivations of its premises. A
  rejection keeps the partial derivation above the failure and names the
  premise that failed, in the calculus's own words, with its citation.
* `traceEval`, which mirrors `eval` and returns a `Trace` — a step table,
  one row per evaluated node in execution order, each carrying the store
  before, the store after, the drop events the node emitted, and the node's
  result.

Neither mirror is trusted on its own: `explain_result` and `traceEval_res`
prove that projecting a `Deriv` to its verdict reproduces `check` exactly,
and projecting a `Trace` to its final result reproduces `eval` exactly. The
proved definitions are untouched — an explanation that disagreed with the
checker or the machine would be a proof obligation failure, not a rendering
bug.

Renderers live in `RueCore/Explain/Text.lean` (terminal, `#eval`) and
`RueCore/Explain/Html.lean` (a self-contained page); `lake exe
ruecore-explain` drives both over the bridge corpus.

Expression text reuses `Print`'s surface syntax and its `v<depth>` binder
naming, so a subexpression is spelled the same way here, in `corpus.json`,
and in the printed Rue program.
-/

namespace RueCore
namespace Explain

/-! ## Rendering vocabulary

The pieces every renderer shares: how a type, a value, a cell, a store, a
context entry, and a drop event are spelled. -/

/-- (helper) A one-line rendering of a core expression, in the Rue surface
syntax of `Print.expr` and with its `v<depth>` binder names, but with the
block forms (`let`, assignment, sequencing, `if`) folded onto one line so a
derivation node or a trace row stays one row. -/
def exprLine : List Ty → Expr → String
  | _, .intLit n => if n < 0 then "(" ++ toString n ++ ")" else toString n
  | _, .boolLit b => if b then "true" else "false"
  | _, .unitLit => "()"
  | Γ, .use i => Print.useName Γ i
  | Γ, .add e₁ e₂ => "(" ++ exprLine Γ e₁ ++ " + " ++ exprLine Γ e₂ ++ ")"
  | Γ, .div e₁ e₂ => "(" ++ exprLine Γ e₁ ++ " / " ++ exprLine Γ e₂ ++ ")"
  | Γ, .lt e₁ e₂ => "(" ++ exprLine Γ e₁ ++ " < " ++ exprLine Γ e₂ ++ ")"
  | Γ, .mkres κ e => Print.resLit κ (exprLine Γ e)
  | Γ, .consume e =>
      let κ := match Print.tyOf Γ e with
        | some (.res κ) => κ
        | _ => .affine
      Print.consumeName κ ++ "(" ++ exprLine Γ e ++ ")"
  | Γ, .drop i =>
      match Γ[i]? with
      | some (.res .linear) => "@dbg(consume_linear(" ++ Print.useName Γ i ++ "))"
      | _ => "@drop(" ++ Print.useName Γ i ++ ")"
  | Γ, .letIn m e₁ e₂ =>
      let T₁ := (Print.tyOf Γ e₁).getD .int
      "{ let " ++ (if m then "mut " else "") ++ Print.binderName Γ.length ++ ": " ++
        Print.tyName T₁ ++ " = " ++ exprLine Γ e₁ ++ "; " ++ exprLine (T₁ :: Γ) e₂ ++ " }"
  | Γ, .assign i e => "{ " ++ Print.useName Γ i ++ " = " ++ exprLine Γ e ++ "; }"
  | Γ, .seq e₁ e₂ => "{ " ++ exprLine Γ e₁ ++ "; " ++ exprLine Γ e₂ ++ " }"
  | Γ, .ite c e₁ e₂ =>
      "if " ++ exprLine Γ c ++ " { " ++ exprLine Γ e₁ ++ " } else { " ++ exprLine Γ e₂ ++ " }"

/-- (helper) Clip a rendering so a tree keeps its shape in an 80-column
terminal. Nothing is lost: every rendering prints the whole program, in
`Print.expr`'s multi-line form, in its header. -/
def clip (n : Nat) (s : String) : String :=
  if s.length ≤ n then s else String.ofList (s.toList.take (n - 1)) ++ "…"

/-- (helper) The binder types of a fused context, innermost first — the
shape `Print`'s naming and type recovery take. -/
def binderTys (Γ : Ctx) : List Ty := Γ.map Entry.ty

/-- (helper) The static type of a machine value. In the fragment every
value determines its type (§7's `HasTy`, read as a function), which is what
lets a trace name its binders the way the source does. -/
def valTy : Val → Ty
  | .int _ => .int
  | .bool _ => .bool
  | .unit => .unit
  | .res κ _ => .res κ

/-- (helper) A value, as the observation channel spells it (`Print.lean`):
a scalar as itself, a resource as its type and payload. -/
def valLine : Val → String
  | .int n => toString n
  | .bool b => if b then "true" else "false"
  | .unit => "()"
  | .res κ n => Print.tyName (.res κ) ++ " { " ++ toString n ++ " }"

/-- (helper) A store cell (§6.1's `c ::= v | ⊘`, plus the retired `†`). -/
def cellLine : Cell → String
  | .full v => valLine v
  | .moved => "⊘ (moved out)"
  | .dead => "† (retired)"

/-- (helper) A store location. Locations are indices and are never reused
(§6.1). -/
def locName (ℓ : Nat) : String := "ℓ" ++ toString ℓ

/-- (helper) The store as location/contents rows, oldest allocation first. -/
def storeRows : Nat → Store → List (String × String)
  | _, [] => []
  | i, c :: cs => (locName i, cellLine c) :: storeRows (i + 1) cs

/-- (helper) The store on one line. -/
def storeLine (H : Store) : String :=
  if H.isEmpty then "(empty)"
  else "[" ++ String.intercalate ", "
    ((storeRows 0 H).map (fun r => r.1 ++ " = " ++ r.2)) ++ "]"

/-- (helper) A `Σ` state, spelled as §5 spells it. -/
def ownStateName : OwnState → String
  | .owned => "Owned"
  | .movedOut => "MovedOut"

/-- (helper) One fused `Γ;Σ` entry: the binder's name, its type and `μ`
mark (the fixed skeleton) and its flowing ownership state. -/
def entryLine (name : String) (en : Entry) : String :=
  name ++ ": " ++ Print.tyName en.ty ++ (if en.mu then " mut" else "") ++
    " = " ++ ownStateName en.st

/-- (helper) The context's entries, innermost binder first, named the way
`Print` names them (`v<depth>`). -/
def ctxEntryLines : Ctx → List String
  | [] => []
  | en :: rest => entryLine (Print.binderName rest.length) en :: ctxEntryLines rest

/-- (helper) The fused `Γ;Σ` context on one line. -/
def ctxLine (Γ : Ctx) : String :=
  if Γ.isEmpty then "(empty)" else "[" ++ String.intercalate ", " (ctxEntryLines Γ) ++ "]"

/-- (helper) One observable drop event (§6.7/§6.8/§6.11). -/
def eventLine : Event → String
  | .drop ℓ v => "drop " ++ locName ℓ ++ " = " ++ valLine v
  | .dropTemp v => "drop temporary " ++ valLine v

/-- (helper) A node's events on one line; most nodes emit none. -/
def eventsLine (evs : List Event) : String :=
  if evs.isEmpty then "—" else String.intercalate "; " (evs.map eventLine)

/-! ## The premises a rejection names

Each string states the premise the calculus requires, in the calculus's own
words, and cites the §-rule and the prose paragraph it comes from; where the
compiler has a diagnostic for the same rule, its code is named too, so a
reader can move between the mechanization and a real error message. -/

namespace Premise

/-- (Use-Copy)/(Use-Move) premise `Σ(p) = Owned` (§5.1); prose `3.8:5`. -/
def useMovedOut : String :=
  "a use of a place whose Σ state is MovedOut — the value was already moved out " ++
  "((Use-Copy)/(Use-Move) premise `Σ(p) = Owned`, §5.1; 3.8:5; the compiler reports E0205)"

/-- (@Drop-Copy)/(@Drop) premise `Σ(p) = Owned` (§5.3); prose `3.8:5`. -/
def dropMovedOut : String :=
  "@drop of a place whose Σ state is MovedOut — the obligation was already discharged " ++
  "((@Drop-Copy)/(@Drop) premise `Σ(p) = Owned`, §5.3; 3.8:5)"

/-- The de Bruijn index names no binder. Elaboration resolves every name
before the core (§2), so no elaborated program reaches this. -/
def unboundIndex : String :=
  "the de Bruijn index names no binder in Γ (name resolution is elaboration's job, §2); " ++
  "no elaborated program reaches this premise"

/-- (Lit) premise: the literal denotes a value of its own type (§5.8);
`int(64, signed)` bounds are `Syntax.lean`'s `intMin`/`intMax` (§6.4). -/
def litOutOfRange : String :=
  "the integer literal is outside int(64, signed) ((Lit) §5.8; the trap bounds of §6.4)"

/-- (helper) A premise whose own derivation failed: the reason is the
rejected sub-derivation nested under this rule, not this rule itself. -/
def subDerivation : String :=
  "a premise's own derivation failed — the reason is the rejected premise nested " ++
  "under this rule"

/-- (Arith)/(Ord) premise: both operands are `int(w,s)` (§5.8; `4.2:1`). -/
def operandNotInt (T : Ty) : String :=
  "an operand has type " ++ Print.tyName T ++ ", but (Arith)/(Ord) require both operands " ++
  "to be int(64, signed) (§5.8; 4.2:1)"

/-- (Struct-Intro) premise: the field's expression has the field's type
(§5.8); here `res κ`'s single payload field is `int`. -/
def payloadNotInt (T : Ty) : String :=
  "the resource payload has type " ++ Print.tyName T ++ ", but `res κ` carries an int " ++
  "((Struct-Intro) §5.8)"

/-- (Call) premise: the argument has the parameter's type (§5.8); the
consuming elimination takes a `res κ` by value (§4.2 use). -/
def consumeNotRes (T : Ty) : String :=
  "the consumed operand has type " ++ Print.tyName T ++ ", but the consuming elimination " ++
  "takes a resource `res κ` by value ((Call) §5.8; §4.2)"

/-- §5.6 scope exit, the residual-linear leak check; prose `3.8:32`. -/
def letLeak (T : Ty) : String :=
  "the residual state of the `let` binder is Owned and its type " ++ Print.tyName T ++
  " is Linear — a linear value reached end of scope unconsumed " ++
  "(§5.6 leak check; 3.8:32; the compiler reports E0406)"

/-- (helper, unreachable) `Typed.skel_preserved` (§5) forbids a rule from
changing the context skeleton, so a body cannot lose its own binder. -/
def letBinderLost : String :=
  "the body's outgoing context lost the `let` binder; skeleton preservation " ++
  "(`Statics.lean`, `Typed.skel_preserved`) forbids it, so no program reaches this premise"

/-- (Assign) mutability side condition (§5.2); prose `5.1:3`. -/
def notMutable : String :=
  "the assignment target is not a `mut` binding ((Assign) mutability side condition, §5.2; 5.1:3)"

/-- (Assign) premise `Γ ⊢ p : T` with `e ⇒ T` (§5.2): one type for the
place and the right-hand side. -/
def assignTypeMismatch (T target : Ty) : String :=
  "the right-hand side has type " ++ Print.tyName T ++ " but the target is declared " ++
  Print.tyName target ++ " ((Assign) premise `Γ ⊢ p : T`, §5.2)"

/-- (helper, unreachable) The target survives the right-hand side by
skeleton preservation (`Typed.skel_preserved`, §5). -/
def assignTargetLost : String :=
  "the target left the context while the right-hand side was checked; skeleton " ++
  "preservation (`Typed.skel_preserved`) forbids it"

/-- (Assign) premise `Σ1(p) = MovedOut ∨ ¬carries_linear(T)` (§5.2);
prose `3.8:77` (the RUE-387 premise). -/
def linearOverwrite (T : Ty) : String :=
  "overwrite of a live linear value: the target is still Owned after the right-hand side " ++
  "and its type " ++ Print.tyName T ++ " is Linear ((Assign) premise " ++
  "`Σ1(p) = MovedOut ∨ ¬carries_linear(T)`, §5.2; 3.8:77; the compiler reports E0493)"

/-- (Seq) premise `carries_linear(T1) = false` (§5.3); prose `3.8:64`. -/
def discardsLinear (T : Ty) : String :=
  "the discarded value has type " ++ Print.tyName T ++ ", which carries a linear value " ++
  "((Seq) premise `carries_linear(T1) = false`, §5.3; 3.8:64; the compiler reports E0478)"

/-- (If) premise `Γ;Σ;Λ ⊢ e0 ⇒ bool ⊣ Σ0` (§5.5). -/
def condNotBool (T : Ty) : String :=
  "the condition has type " ++ Print.tyName T ++ ", but (If) requires bool (§5.5)"

/-- (If) premise: both arms have one type `T` (§5.5). -/
def armTypeMismatch (T₁ T₂ : Ty) : String :=
  "the arms have types " ++ Print.tyName T₁ ++ " and " ++ Print.tyName T₂ ++
  "; (If) requires one type T for both (§5.5)"

/-- (If) premise `Σ' = join(Σ1, Σ2)` (§5.5); prose `3.8:50`. `who` names
the entry the two arms disagree on, when one can be named. -/
def joinConflict (who : Option String) : String :=
  "the two arms disagree on a linear-carrying entry" ++
  (match who with | some w => " — " ++ w | none => "") ++
  ", so a linear value is consumed on only some paths ((If) premise " ++
  "`Σ' = join(Σ1, Σ2)`, §5.5; 3.8:50; the compiler reports E0443)"

end Premise

/-- (helper) The first entry on which the §5.5 join fails, named as the
source names it, so a join rejection can point at a binding. -/
def joinConflictEntry : Ctx → Ctx → Option String
  | a :: as, b :: bs =>
      if (a.join b).isNone then
        some (Print.binderName as.length ++ ": " ++ Print.tyName a.ty ++ " is " ++
          ownStateName a.st ++ " in the then-arm and " ++ ownStateName b.st ++ " in the else-arm")
      else joinConflictEntry as bs
  | _, _ => none

/-! ## Derivations -/

/-- What a rule concluded at one node: the §5 judgment's right-hand side
`⇒ T ⊣ Σ'`, or the premise that failed. -/
inductive Verdict where
  | accept (ty : Ty) (ctxOut : Ctx)
  | reject (premise : String)

/-- A derivation tree for the §5 judgment `Γ;Σ ⊢ e ⇒ T ⊣ Σ'`: one node per
rule, carrying the rule's name as the calculus writes it, the incoming fused
`Γ;Σ`, the expression the rule concluded about, its verdict, and the
sub-derivations of its premises, in premise order. -/
inductive Deriv where
  | node (rule : String) (ctxIn : Ctx) (expr : Expr) (verdict : Verdict) (kids : List Deriv)

/-- The derivation's conclusion, in `check`'s shape: the type and outgoing
`Σ` of an accepted node, nothing for a rejected one. `explain_result` is the
proof that this projection is exactly `check` (§5 as an algorithm). -/
def Deriv.result : Deriv → Option (Ty × Ctx)
  | .node _ _ _ (.accept T Γ') _ => some (T, Γ')
  | .node _ _ _ (.reject _) _ => none

/-- (helper) The deepest rejected premise of a derivation: the one a reader
should read first, since every rule above it only reports that a premise
failed. The rule's name, the subexpression, its binder types, and the
premise. -/
partial def deepestFailure : Deriv → Option (String × Expr × List Ty × String)
  | .node r Γ e v kids =>
      match (kids.map deepestFailure).reduceOption.head? with
      | some f => some f
      | none =>
        match v with
        | .reject why => some (r, e, binderTys Γ, why)
        | .accept _ _ => none

/-- (helper) An accepting node. -/
def accepted (rule : String) (Γ : Ctx) (e : Expr) (T : Ty) (Γ' : Ctx)
    (kids : List Deriv) : Deriv :=
  .node rule Γ e (.accept T Γ') kids

/-- (helper) A rejecting node: the partial derivation above the failure
plus the premise that failed. -/
def rejected (rule : String) (Γ : Ctx) (e : Expr) (why : String)
    (kids : List Deriv) : Deriv :=
  .node rule Γ e (.reject why) kids

/-- The instrumented mirror of `check` (§5): the same algorithm, recording
the rule it applied at every node and, where it rejects, the premise that
failed. `explain_result` proves the two agree. -/
def explain (Γ : Ctx) : Expr → Deriv
  | .intLit n =>
      if InBounds n then accepted "(Lit) §5.8" Γ (.intLit n) .int Γ []
      else rejected "(Lit) §5.8" Γ (.intLit n) Premise.litOutOfRange []
  | .boolLit b => accepted "(Lit) §5.8" Γ (.boolLit b) .bool Γ []
  | .unitLit => accepted "(Lit) §5.8" Γ .unitLit .unit Γ []
  | .use i =>
      match Γ[i]? with
      | none => rejected "(Use-Copy)/(Use-Move) §5.1" Γ (.use i) Premise.unboundIndex []
      | some en =>
        match en.st with
        | .movedOut => rejected "(Use-Copy)/(Use-Move) §5.1" Γ (.use i) Premise.useMovedOut []
        | .owned =>
          if en.ty.mult = .copy then
            accepted "(Use-Copy) §5.1" Γ (.use i) en.ty Γ []
          else
            accepted "(Use-Move) §5.1" Γ (.use i) en.ty (Γ.set i (en.setSt .movedOut)) []
  | .add e₁ e₂ =>
      let d₁ := explain Γ e₁
      match d₁.result with
      | some (.int, Γ₁) =>
        let d₂ := explain Γ₁ e₂
        (match d₂.result with
         | some (.int, Γ₂) => accepted "(Arith) §5.8" Γ (.add e₁ e₂) .int Γ₂ [d₁, d₂]
         | some (T, _) =>
             rejected "(Arith) §5.8" Γ (.add e₁ e₂) (Premise.operandNotInt T) [d₁, d₂]
         | none => rejected "(Arith) §5.8" Γ (.add e₁ e₂) Premise.subDerivation [d₁, d₂])
      | some (T, _) => rejected "(Arith) §5.8" Γ (.add e₁ e₂) (Premise.operandNotInt T) [d₁]
      | none => rejected "(Arith) §5.8" Γ (.add e₁ e₂) Premise.subDerivation [d₁]
  | .div e₁ e₂ =>
      let d₁ := explain Γ e₁
      match d₁.result with
      | some (.int, Γ₁) =>
        let d₂ := explain Γ₁ e₂
        (match d₂.result with
         | some (.int, Γ₂) => accepted "(Arith) §5.8" Γ (.div e₁ e₂) .int Γ₂ [d₁, d₂]
         | some (T, _) =>
             rejected "(Arith) §5.8" Γ (.div e₁ e₂) (Premise.operandNotInt T) [d₁, d₂]
         | none => rejected "(Arith) §5.8" Γ (.div e₁ e₂) Premise.subDerivation [d₁, d₂])
      | some (T, _) => rejected "(Arith) §5.8" Γ (.div e₁ e₂) (Premise.operandNotInt T) [d₁]
      | none => rejected "(Arith) §5.8" Γ (.div e₁ e₂) Premise.subDerivation [d₁]
  | .lt e₁ e₂ =>
      let d₁ := explain Γ e₁
      match d₁.result with
      | some (.int, Γ₁) =>
        let d₂ := explain Γ₁ e₂
        (match d₂.result with
         | some (.int, Γ₂) => accepted "(Ord) §5.8" Γ (.lt e₁ e₂) .bool Γ₂ [d₁, d₂]
         | some (T, _) => rejected "(Ord) §5.8" Γ (.lt e₁ e₂) (Premise.operandNotInt T) [d₁, d₂]
         | none => rejected "(Ord) §5.8" Γ (.lt e₁ e₂) Premise.subDerivation [d₁, d₂])
      | some (T, _) => rejected "(Ord) §5.8" Γ (.lt e₁ e₂) (Premise.operandNotInt T) [d₁]
      | none => rejected "(Ord) §5.8" Γ (.lt e₁ e₂) Premise.subDerivation [d₁]
  | .mkres κ e =>
      let d := explain Γ e
      match d.result with
      | some (.int, Γ') => accepted "(Struct-Intro) §5.8" Γ (.mkres κ e) (.res κ) Γ' [d]
      | some (T, _) =>
          rejected "(Struct-Intro) §5.8" Γ (.mkres κ e) (Premise.payloadNotInt T) [d]
      | none => rejected "(Struct-Intro) §5.8" Γ (.mkres κ e) Premise.subDerivation [d]
  | .consume e =>
      let d := explain Γ e
      match d.result with
      | some (.res _, Γ') => accepted "(Call) §5.8" Γ (.consume e) .int Γ' [d]
      | some (.int, _) => rejected "(Call) §5.8" Γ (.consume e) (Premise.consumeNotRes .int) [d]
      | some (.bool, _) => rejected "(Call) §5.8" Γ (.consume e) (Premise.consumeNotRes .bool) [d]
      | some (.unit, _) => rejected "(Call) §5.8" Γ (.consume e) (Premise.consumeNotRes .unit) [d]
      | none => rejected "(Call) §5.8" Γ (.consume e) Premise.subDerivation [d]
  | .drop i =>
      match Γ[i]? with
      | none => rejected "(@Drop-Copy)/(@Drop) §5.3" Γ (.drop i) Premise.unboundIndex []
      | some en =>
        match en.st with
        | .movedOut => rejected "(@Drop-Copy)/(@Drop) §5.3" Γ (.drop i) Premise.dropMovedOut []
        | .owned =>
          if en.ty.mult = .copy then
            accepted "(@Drop-Copy) §5.3" Γ (.drop i) .unit Γ []
          else
            accepted "(@Drop) §5.3" Γ (.drop i) .unit (Γ.set i (en.setSt .movedOut)) []
  | .letIn m e₁ e₂ =>
      let d₁ := explain Γ e₁
      match d₁.result with
      | none => rejected "(Let) §5.6 + scope-exit leak check" Γ (.letIn m e₁ e₂)
                  Premise.subDerivation [d₁]
      | some (T₁, Γ₁) =>
        let d₂ := explain ({ ty := T₁, mu := m, st := .owned } :: Γ₁) e₂
        (match d₂.result with
         | some (T₂, en' :: Γ₂) =>
             if en'.st = .owned ∧ T₁.mult = .linear then
               rejected "(Let) §5.6 + scope-exit leak check" Γ (.letIn m e₁ e₂)
                 (Premise.letLeak T₁) [d₁, d₂]
             else
               accepted "(Let) §5.6 + scope-exit leak check" Γ (.letIn m e₁ e₂) T₂ Γ₂ [d₁, d₂]
         | some (_, []) =>
             rejected "(Let) §5.6 + scope-exit leak check" Γ (.letIn m e₁ e₂)
               Premise.letBinderLost [d₁, d₂]
         | none =>
             rejected "(Let) §5.6 + scope-exit leak check" Γ (.letIn m e₁ e₂)
               Premise.subDerivation [d₁, d₂])
  | .assign i e =>
      match Γ[i]? with
      | none => rejected "(Assign) §5.2, 3.8:77" Γ (.assign i e) Premise.unboundIndex []
      | some en₀ =>
        if en₀.mu = true then
          let d := explain Γ e
          (match d.result with
           | some (T, Γ₁) =>
             if T = en₀.ty then
               match Γ₁[i]? with
               | some en₁ =>
                   if en₁.st = .movedOut ∨ en₀.ty.mult ≠ .linear then
                     accepted "(Assign) §5.2, 3.8:77" Γ (.assign i e) .unit
                       (Γ₁.set i (en₁.setSt .owned)) [d]
                   else
                     rejected "(Assign) §5.2, 3.8:77" Γ (.assign i e)
                       (Premise.linearOverwrite en₀.ty) [d]
               | none =>
                   rejected "(Assign) §5.2, 3.8:77" Γ (.assign i e)
                     Premise.assignTargetLost [d]
             else
               rejected "(Assign) §5.2, 3.8:77" Γ (.assign i e)
                 (Premise.assignTypeMismatch T en₀.ty) [d]
           | none =>
               rejected "(Assign) §5.2, 3.8:77" Γ (.assign i e) Premise.subDerivation [d])
        else rejected "(Assign) §5.2, 3.8:77" Γ (.assign i e) Premise.notMutable []
  | .seq e₁ e₂ =>
      let d₁ := explain Γ e₁
      match d₁.result with
      | some (T₁, Γ₁) =>
          if T₁.mult = .linear then
            rejected "(Seq) §5.3, 3.8:64" Γ (.seq e₁ e₂) (Premise.discardsLinear T₁) [d₁]
          else
            let d₂ := explain Γ₁ e₂
            (match d₂.result with
             | some (T₂, Γ₂) => accepted "(Seq) §5.3, 3.8:64" Γ (.seq e₁ e₂) T₂ Γ₂ [d₁, d₂]
             | none => rejected "(Seq) §5.3, 3.8:64" Γ (.seq e₁ e₂) Premise.subDerivation [d₁, d₂])
      | none => rejected "(Seq) §5.3, 3.8:64" Γ (.seq e₁ e₂) Premise.subDerivation [d₁]
  | .ite c e₁ e₂ =>
      let dc := explain Γ c
      match dc.result with
      | some (.bool, Γ₀) =>
        let d₁ := explain Γ₀ e₁
        let d₂ := explain Γ₀ e₂
        (match d₁.result, d₂.result with
         | some (T₁, Γ₁), some (T₂, Γ₂) =>
             if T₁ = T₂ then
               match Ctx.join Γ₁ Γ₂ with
               | some Γ' => accepted "(If) §5.5 join" Γ (.ite c e₁ e₂) T₁ Γ' [dc, d₁, d₂]
               | none =>
                   rejected "(If) §5.5 join" Γ (.ite c e₁ e₂)
                     (Premise.joinConflict (joinConflictEntry Γ₁ Γ₂)) [dc, d₁, d₂]
             else
               rejected "(If) §5.5 join" Γ (.ite c e₁ e₂)
                 (Premise.armTypeMismatch T₁ T₂) [dc, d₁, d₂]
         | _, _ => rejected "(If) §5.5 join" Γ (.ite c e₁ e₂) Premise.subDerivation [dc, d₁, d₂])
      | some (T, _) =>
          rejected "(If) §5.5 join" Γ (.ite c e₁ e₂) (Premise.condNotBool T) [dc]
      | none => rejected "(If) §5.5 join" Γ (.ite c e₁ e₂) Premise.subDerivation [dc]

/-- **The derivation is the checker.** Projecting a derivation to its
conclusion reproduces `check Γ e` exactly, so a rendered derivation can
never claim an acceptance or a rejection the verified checker (§5,
`check_sound`) does not make. -/
theorem explain_result : ∀ (e : Expr) (Γ : Ctx), (explain Γ e).result = check Γ e := by
  intro e
  induction e with
    (intro Γ
     simp only [explain, check])
  | intLit n => split <;> rfl
  | boolLit b => rfl
  | unitLit => rfl
  | use i =>
      (repeat' split) <;> first | rfl | simp_all [accepted, Deriv.result]
  | add e₁ e₂ ih₁ ih₂ =>
      simp only [ih₁, ih₂]
      (repeat' split) <;> first | rfl | simp_all [accepted, rejected, Deriv.result]
  | div e₁ e₂ ih₁ ih₂ =>
      simp only [ih₁, ih₂]
      (repeat' split) <;> first | rfl | simp_all [accepted, rejected, Deriv.result]
  | lt e₁ e₂ ih₁ ih₂ =>
      simp only [ih₁, ih₂]
      (repeat' split) <;> first | rfl | simp_all [accepted, rejected, Deriv.result]
  | mkres κ e ih =>
      simp only [ih]
      (repeat' split) <;> first | rfl | simp_all [accepted, rejected, Deriv.result]
  | consume e ih =>
      simp only [ih]
      (repeat' split) <;> first | rfl | simp_all [accepted, Deriv.result]
  | drop i =>
      (repeat' split) <;> first | rfl | simp_all [accepted, Deriv.result]
  | letIn m e₁ e₂ ih₁ ih₂ =>
      simp only [ih₁, ih₂]
      (repeat' split) <;> first | rfl | simp_all [accepted, rejected, Deriv.result]
  | assign i e ih =>
      simp only [ih]
      (repeat' split) <;> first | rfl | simp_all [accepted, rejected, Deriv.result]
  | seq e₁ e₂ ih₁ ih₂ =>
      simp only [ih₁, ih₂]
      (repeat' split) <;> first | rfl | simp_all [accepted, rejected, Deriv.result]
  | ite c e₁ e₂ ihc ih₁ ih₂ =>
      simp only [ihc]
      cases hc : check Γ c with
      | none => rfl
      | some p =>
        obtain ⟨T, Γ₀⟩ := p
        cases T with
        | int => rfl
        | unit => rfl
        | res κ => rfl
        | bool =>
            simp only [ih₁, ih₂]
            cases h₁ : check Γ₀ e₁ with
            | none => cases h₂ : check Γ₀ e₂ <;> rfl
            | some q₁ =>
              cases h₂ : check Γ₀ e₂ with
              | none => rfl
              | some q₂ =>
                obtain ⟨T₁, Γ₁⟩ := q₁
                obtain ⟨T₂, Γ₂⟩ := q₂
                simp only []
                split
                · cases hj : Ctx.join Γ₁ Γ₂ <;> rfl
                · rfl

/-! ## Runs

The step table: one row per evaluated node, in execution order (a node's
premises run before the node itself, so the table reads top to bottom as the
machine ran). Each row carries the store before and after, the drop events
the node emitted, and what the node produced. -/

/-- The machine's refusal, in §6's words, with the §7 bullet it violates
and the prose paragraph behind it. -/
def violationPremise : Violation → String
  | .useAfterMove =>
      "a read of a cell holding ⊘: the value was already moved out of this place " ++
      "((D-Use-Move) §6.3; 3.8:5; §7 “no use after move”)"
  | .useAfterDrop =>
      "a touch of a retired cell †: the binding's allocation was dropped and retired at " ++
      "scope exit ((D-EndScope) §6.1/§6.7; §7 “no use after drop”)"
  | .linearLeak =>
      "scope exit reached a live linear value: a linear obligation was never discharged " ++
      "(endscope §6.7, the §5.6 leak check executed; 3.8:32; §7 “consumed exactly once”)"
  | .linearOverwrite =>
      "an overwrite-drop of a live linear value: the assignment would consume a linear " ++
      "value the program never consumed (§6.8; 3.8:77; §7)"
  | .linearDiscard =>
      "a sequence discarded a linear value: the temporary's obligation was never " ++
      "discharged ((D-Seq) §6.7; 3.8:64; §7)"
  | .unbound =>
      "a dangling index: the environment has no location for this binding (elaboration " ++
      "resolves names before the core, §2, so no elaborated program reaches this)"
  | .typeConfusion =>
      "an operator met a wrong-shaped value; the statics (§5) exclude it, and `soundness` " ++
      "(§7) is the proof"

/-- What one node produced: a value, a defined trap (§6.12), or a refusal
(§6's stuck states) with the premise it broke. -/
inductive StepRes where
  | value (v : Val)
  | panicked (k : PanicKind)
  | refuse (why : Violation) (premise : String)

/-- (helper) How a node reports a sub-result it only passes on. -/
def StepRes.ofRes : EvalRes → StepRes
  | .ok _ v _ => .value v
  | .panic k => .panicked k
  | .stuck w => .refuse w (violationPremise w)

/-- (helper) The store a result leaves. A trap or a refusal reaches none
(`EvalRes` keeps no store for them), so the node reports the last store it
did reach. -/
def storeOf (fallback : Store) : EvalRes → Store
  | .ok H _ _ => H
  | _ => fallback

/-- One row of the step table: the node's nesting depth, the §6 rule it
took, the binder types in scope (so the expression prints with the source's
names), the expression, the store before and after, the drop events this
node emitted (§6.7/§6.8/§6.11), and what it produced. -/
structure Step where
  depth : Nat
  rule : String
  binders : List Ty
  expr : Expr
  storeBefore : Store
  storeAfter : Store
  events : List Event
  res : StepRes

/-- A run of the §6 machine: the step table in execution order and the
machine's final result. `traceEval_res` proves the final result is
`eval`'s. -/
structure Trace where
  steps : List Step
  res : EvalRes

/-- (helper) One row. -/
def mkStep (d : Nat) (Θ : List Ty) (e : Expr) (rule : String) (H H' : Store)
    (evs : List Event) (sr : StepRes) : Step :=
  { depth := d, rule := rule, binders := Θ, expr := e,
    storeBefore := H, storeAfter := H', events := evs, res := sr }

/-- (helper) Number the steps of a run from 1, in execution order. -/
def numbered : Nat → List Step → List (Nat × Step)
  | _, [] => []
  | n, s :: rest => (n, s) :: numbered (n + 1) rest

/-- (helper) `traced kids d Θ e rule H H' evs sr r`: the trace of a node
whose premises already contributed `kids`, whose own row runs `e` at depth
`d` under `rule` with binders `Θ`, taking the store from `H` to `H'`,
emitting `evs` and producing `sr`, and whose machine result is `r`. The own
row comes last, so the table is in execution order. -/
def traced (kids : List Step) (d : Nat) (Θ : List Ty) (e : Expr) (rule : String)
    (H H' : Store) (evs : List Event) (sr : StepRes) (r : EvalRes) : Trace :=
  ⟨kids ++ [mkStep d Θ e rule H H' evs sr], r⟩

/-- (helper) A node that never ran: one of its premises trapped or refused,
so the node only passes that outcome on. The row says so, because the same
refusal then repeats up the spine of the run. -/
def propagate (kids : List Step) (d : Nat) (Θ : List Ty) (e : Expr) (rule : String)
    (H H' : Store) (r : EvalRes) : Trace :=
  traced kids d Θ e (rule ++ " — a premise did not complete") H H' [] (StepRes.ofRes r) r

/-- (helper) A node that refuses (§6's stuck states). -/
def refused (kids : List Step) (d : Nat) (Θ : List Ty) (e : Expr) (rule : String)
    (H H' : Store) (w : Violation) : Trace :=
  traced kids d Θ e rule H H' [] (.refuse w (violationPremise w)) (.stuck w)

/-- (helper) An operator that met a wrong-shaped value. §5 excludes it and
`soundness` (§7) proves so; it is here because `eval` is total. -/
def confused (kids : List Step) (d : Nat) (Θ : List Ty) (e : Expr) (rule : String)
    (H H' : Store) : Trace :=
  refused kids d Θ e rule H H' .typeConfusion

/-- The instrumented mirror of `eval` (§6): the same machine, recording one
row per evaluated node. `traceEval_res` proves the two agree on the final
result. `d` is the nesting depth and `Θ` the binder types in scope, which
travel with `ρ` so each row prints its expression with the source's names. -/
def traceEval (d : Nat) (Θ : List Ty) (H : Store) (ρ : Env) : Expr → Trace
  | .intLit n =>
      traced [] d Θ (.intLit n) "literal §6.3" H H [] (.value (.int n)) (.ok H (.int n) [])
  | .boolLit b =>
      traced [] d Θ (.boolLit b) "literal §6.3" H H [] (.value (.bool b)) (.ok H (.bool b) [])
  | .unitLit =>
      traced [] d Θ .unitLit "literal §6.3" H H [] (.value .unit) (.ok H .unit [])
  | .use i =>
      match ρ[i]? with
      | none => refused [] d Θ (.use i) "(D-Use-Copy)/(D-Use-Move) §6.3" H H .unbound
      | some ℓ =>
        match H[ℓ]? with
        | none => refused [] d Θ (.use i) "(D-Use-Copy)/(D-Use-Move) §6.3" H H .unbound
        | some .dead => refused [] d Θ (.use i) "(D-Use-Copy)/(D-Use-Move) §6.3" H H .useAfterDrop
        | some .moved => refused [] d Θ (.use i) "(D-Use-Copy)/(D-Use-Move) §6.3" H H .useAfterMove
        | some (.full v) =>
            if v.mult = .copy then
              traced [] d Θ (.use i) "(D-Use-Copy) §6.3" H H [] (.value v) (.ok H v [])
            else
              traced [] d Θ (.use i) "(D-Use-Move) §6.3" H (H.set ℓ .moved) []
                (.value v) (.ok (H.set ℓ .moved) v [])
  | .add e₁ e₂ =>
      let t₁ := traceEval (d + 1) Θ H ρ e₁
      match t₁.res with
      | .ok H₁ (.int n₁) tr₁ =>
        let t₂ := traceEval (d + 1) Θ H₁ ρ e₂
        (match t₂.res with
         | .ok H₂ (.int n₂) tr₂ =>
             if InBounds (n₁ + n₂) then
               traced (t₁.steps ++ t₂.steps) d Θ (.add e₁ e₂) "(D-Arith) §6.4" H H₂ []
                 (.value (.int (n₁ + n₂))) (.ok H₂ (.int (n₁ + n₂)) (tr₁ ++ tr₂))
             else
               traced (t₁.steps ++ t₂.steps) d Θ (.add e₁ e₂) "(D-Arith-Trap) §6.4" H H₂ []
                 (.panicked .overflow) (.panic .overflow)
         | .ok H₂ (.bool _) _ => confused (t₁.steps ++ t₂.steps) d Θ (.add e₁ e₂) "(D-Arith) §6.4" H H₂
         | .ok H₂ .unit _ => confused (t₁.steps ++ t₂.steps) d Θ (.add e₁ e₂) "(D-Arith) §6.4" H H₂
         | .ok H₂ (.res _ _) _ =>
             confused (t₁.steps ++ t₂.steps) d Θ (.add e₁ e₂) "(D-Arith) §6.4" H H₂
         | .panic k =>
             propagate (t₁.steps ++ t₂.steps) d Θ (.add e₁ e₂) "(D-Arith) §6.4" H H₁ (.panic k)
         | .stuck w =>
             propagate (t₁.steps ++ t₂.steps) d Θ (.add e₁ e₂) "(D-Arith) §6.4" H H₁ (.stuck w))
      | .ok H₁ (.bool _) _ => confused t₁.steps d Θ (.add e₁ e₂) "(D-Arith) §6.4" H H₁
      | .ok H₁ .unit _ => confused t₁.steps d Θ (.add e₁ e₂) "(D-Arith) §6.4" H H₁
      | .ok H₁ (.res _ _) _ => confused t₁.steps d Θ (.add e₁ e₂) "(D-Arith) §6.4" H H₁
      | .panic k => propagate t₁.steps d Θ (.add e₁ e₂) "(D-Arith) §6.4" H H (.panic k)
      | .stuck w => propagate t₁.steps d Θ (.add e₁ e₂) "(D-Arith) §6.4" H H (.stuck w)
  | .div e₁ e₂ =>
      let t₁ := traceEval (d + 1) Θ H ρ e₁
      match t₁.res with
      | .ok H₁ (.int n₁) tr₁ =>
        let t₂ := traceEval (d + 1) Θ H₁ ρ e₂
        (match t₂.res with
         | .ok H₂ (.int n₂) tr₂ =>
             if n₂ = 0 then
               traced (t₁.steps ++ t₂.steps) d Θ (.div e₁ e₂) "(D-Div) §6.4 (trap)" H H₂ []
                 (.panicked .divZero) (.panic .divZero)
             else if InBounds (n₁.tdiv n₂) then
               traced (t₁.steps ++ t₂.steps) d Θ (.div e₁ e₂) "(D-Div) §6.4" H H₂ []
                 (.value (.int (n₁.tdiv n₂))) (.ok H₂ (.int (n₁.tdiv n₂)) (tr₁ ++ tr₂))
             else
               traced (t₁.steps ++ t₂.steps) d Θ (.div e₁ e₂) "(D-Div) §6.4 (trap)" H H₂ []
                 (.panicked .overflow) (.panic .overflow)
         | .ok H₂ (.bool _) _ => confused (t₁.steps ++ t₂.steps) d Θ (.div e₁ e₂) "(D-Div) §6.4" H H₂
         | .ok H₂ .unit _ => confused (t₁.steps ++ t₂.steps) d Θ (.div e₁ e₂) "(D-Div) §6.4" H H₂
         | .ok H₂ (.res _ _) _ => confused (t₁.steps ++ t₂.steps) d Θ (.div e₁ e₂) "(D-Div) §6.4" H H₂
         | .panic k =>
             propagate (t₁.steps ++ t₂.steps) d Θ (.div e₁ e₂) "(D-Div) §6.4" H H₁ (.panic k)
         | .stuck w =>
             propagate (t₁.steps ++ t₂.steps) d Θ (.div e₁ e₂) "(D-Div) §6.4" H H₁ (.stuck w))
      | .ok H₁ (.bool _) _ => confused t₁.steps d Θ (.div e₁ e₂) "(D-Div) §6.4" H H₁
      | .ok H₁ .unit _ => confused t₁.steps d Θ (.div e₁ e₂) "(D-Div) §6.4" H H₁
      | .ok H₁ (.res _ _) _ => confused t₁.steps d Θ (.div e₁ e₂) "(D-Div) §6.4" H H₁
      | .panic k => propagate t₁.steps d Θ (.div e₁ e₂) "(D-Div) §6.4" H H (.panic k)
      | .stuck w => propagate t₁.steps d Θ (.div e₁ e₂) "(D-Div) §6.4" H H (.stuck w)
  | .lt e₁ e₂ =>
      let t₁ := traceEval (d + 1) Θ H ρ e₁
      match t₁.res with
      | .ok H₁ (.int n₁) tr₁ =>
        let t₂ := traceEval (d + 1) Θ H₁ ρ e₂
        (match t₂.res with
         | .ok H₂ (.int n₂) tr₂ =>
             traced (t₁.steps ++ t₂.steps) d Θ (.lt e₁ e₂) "ordering compare §6.4" H H₂ []
               (.value (.bool (decide (n₁ < n₂)))) (.ok H₂ (.bool (decide (n₁ < n₂))) (tr₁ ++ tr₂))
         | .ok H₂ (.bool _) _ =>
             confused (t₁.steps ++ t₂.steps) d Θ (.lt e₁ e₂) "ordering compare §6.4" H H₂
         | .ok H₂ .unit _ =>
             confused (t₁.steps ++ t₂.steps) d Θ (.lt e₁ e₂) "ordering compare §6.4" H H₂
         | .ok H₂ (.res _ _) _ =>
             confused (t₁.steps ++ t₂.steps) d Θ (.lt e₁ e₂) "ordering compare §6.4" H H₂
         | .panic k =>
             propagate (t₁.steps ++ t₂.steps) d Θ (.lt e₁ e₂) "ordering compare §6.4" H H₁ (.panic k)
         | .stuck w =>
             propagate (t₁.steps ++ t₂.steps) d Θ (.lt e₁ e₂) "ordering compare §6.4" H H₁ (.stuck w))
      | .ok H₁ (.bool _) _ => confused t₁.steps d Θ (.lt e₁ e₂) "ordering compare §6.4" H H₁
      | .ok H₁ .unit _ => confused t₁.steps d Θ (.lt e₁ e₂) "ordering compare §6.4" H H₁
      | .ok H₁ (.res _ _) _ => confused t₁.steps d Θ (.lt e₁ e₂) "ordering compare §6.4" H H₁
      | .panic k => propagate t₁.steps d Θ (.lt e₁ e₂) "ordering compare §6.4" H H (.panic k)
      | .stuck w => propagate t₁.steps d Θ (.lt e₁ e₂) "ordering compare §6.4" H H (.stuck w)
  | .mkres κ e =>
      let t := traceEval (d + 1) Θ H ρ e
      match t.res with
      | .ok H' (.int n) tr =>
          traced t.steps d Θ (.mkres κ e) "(D-Struct) §6.5" H H' []
            (.value (.res κ n)) (.ok H' (.res κ n) tr)
      | .ok H' (.bool _) _ => confused t.steps d Θ (.mkres κ e) "(D-Struct) §6.5" H H'
      | .ok H' .unit _ => confused t.steps d Θ (.mkres κ e) "(D-Struct) §6.5" H H'
      | .ok H' (.res _ _) _ => confused t.steps d Θ (.mkres κ e) "(D-Struct) §6.5" H H'
      | .panic k => propagate t.steps d Θ (.mkres κ e) "(D-Struct) §6.5" H H (.panic k)
      | .stuck w => propagate t.steps d Θ (.mkres κ e) "(D-Struct) §6.5" H H (.stuck w)
  | .consume e =>
      let t := traceEval (d + 1) Θ H ρ e
      match t.res with
      | .ok H' (.res _ n) tr =>
          traced t.steps d Θ (.consume e) "(D-Call) §6.9" H H' [] (.value (.int n)) (.ok H' (.int n) tr)
      | .ok H' (.int _) _ => confused t.steps d Θ (.consume e) "(D-Call) §6.9" H H'
      | .ok H' (.bool _) _ => confused t.steps d Θ (.consume e) "(D-Call) §6.9" H H'
      | .ok H' .unit _ => confused t.steps d Θ (.consume e) "(D-Call) §6.9" H H'
      | .panic k => propagate t.steps d Θ (.consume e) "(D-Call) §6.9" H H (.panic k)
      | .stuck w => propagate t.steps d Θ (.consume e) "(D-Call) §6.9" H H (.stuck w)
  | .drop i =>
      match ρ[i]? with
      | none => refused [] d Θ (.drop i) "@drop §6.11" H H .unbound
      | some ℓ =>
        match H[ℓ]? with
        | none => refused [] d Θ (.drop i) "@drop §6.11" H H .unbound
        | some .dead => refused [] d Θ (.drop i) "@drop §6.11" H H .useAfterDrop
        | some .moved => refused [] d Θ (.drop i) "@drop §6.11" H H .useAfterMove
        | some (.full v) =>
            if v.mult = .copy then
              traced [] d Θ (.drop i) "@drop §6.11 (Copy: no glue)" H H [] (.value .unit) (.ok H .unit [])
            else
              traced [] d Θ (.drop i) "@drop §6.11" H (H.set ℓ .moved) [.drop ℓ v]
                (.value .unit) (.ok (H.set ℓ .moved) .unit [.drop ℓ v])
  | .letIn m e₁ e₂ =>
      let t₁ := traceEval (d + 1) Θ H ρ e₁
      match t₁.res with
      | .ok H₁ v₁ tr₁ =>
          let bind := mkStep (d + 1) Θ (.letIn m e₁ e₂) "(D-Let) §6.7 (mint the binding)"
            H₁ (H₁ ++ [.full v₁]) [] (.value v₁)
          let t₂ := traceEval (d + 1) (valTy v₁ :: Θ) (H₁ ++ [.full v₁]) (H₁.length :: ρ) e₂
          (match t₂.res with
           | .ok H₂ v₂ tr₂ =>
             (match H₂[H₁.length]? with
              | some (.full v') =>
                  match v'.mult with
                  | .linear =>
                      refused (t₁.steps ++ [bind] ++ t₂.steps) d Θ (.letIn m e₁ e₂)
                        "(D-EndScope) §6.7 (retire the binding)" H₂ H₂ .linearLeak
                  | .affine =>
                      traced (t₁.steps ++ [bind] ++ t₂.steps) d Θ (.letIn m e₁ e₂)
                        "(D-EndScope) §6.7 (retire the binding)" H₂ (H₂.set H₁.length .dead)
                        [.drop H₁.length v']
                        (.value v₂)
                        (.ok (H₂.set H₁.length .dead) v₂ (tr₁ ++ tr₂ ++ [.drop H₁.length v']))
                  | .copy =>
                      traced (t₁.steps ++ [bind] ++ t₂.steps) d Θ (.letIn m e₁ e₂)
                        "(D-EndScope) §6.7 (retire the binding)" H₂ (H₂.set H₁.length .dead) []
                        (.value v₂) (.ok (H₂.set H₁.length .dead) v₂ (tr₁ ++ tr₂))
              | some .moved =>
                  traced (t₁.steps ++ [bind] ++ t₂.steps) d Θ (.letIn m e₁ e₂)
                    "(D-EndScope) §6.7 (retire the binding)" H₂ (H₂.set H₁.length .dead) []
                    (.value v₂) (.ok (H₂.set H₁.length .dead) v₂ (tr₁ ++ tr₂))
              | some .dead =>
                  refused (t₁.steps ++ [bind] ++ t₂.steps) d Θ (.letIn m e₁ e₂)
                    "(D-EndScope) §6.7 (retire the binding)" H₂ H₂ .useAfterDrop
              | none =>
                  refused (t₁.steps ++ [bind] ++ t₂.steps) d Θ (.letIn m e₁ e₂)
                    "(D-EndScope) §6.7 (retire the binding)" H₂ H₂ .unbound)
           | .panic k =>
               propagate (t₁.steps ++ [bind] ++ t₂.steps) d Θ (.letIn m e₁ e₂)
                 "(D-Let) §6.7" H H₁ (.panic k)
           | .stuck w =>
               propagate (t₁.steps ++ [bind] ++ t₂.steps) d Θ (.letIn m e₁ e₂)
                 "(D-Let) §6.7" H H₁ (.stuck w))
      | .panic k => propagate t₁.steps d Θ (.letIn m e₁ e₂) "(D-Let) §6.7" H H (.panic k)
      | .stuck w => propagate t₁.steps d Θ (.letIn m e₁ e₂) "(D-Let) §6.7" H H (.stuck w)
  | .assign i e =>
      let t := traceEval (d + 1) Θ H ρ e
      match t.res with
      | .ok H₁ v tr =>
          (match ρ[i]? with
           | none => refused t.steps d Θ (.assign i e) "(D-Assign) §6.8" H H₁ .unbound
           | some ℓ =>
             match H₁[ℓ]? with
             | none => refused t.steps d Θ (.assign i e) "(D-Assign) §6.8" H H₁ .unbound
             | some .dead => refused t.steps d Θ (.assign i e) "(D-Assign) §6.8" H H₁ .useAfterDrop
             | some .moved =>
                 traced t.steps d Θ (.assign i e) "(D-Assign) §6.8 (reinitialization, 3.8:55)"
                   H (H₁.set ℓ (.full v)) [] (.value .unit) (.ok (H₁.set ℓ (.full v)) .unit tr)
             | some (.full vOld) =>
                 match vOld.mult with
                 | .linear =>
                     refused t.steps d Θ (.assign i e) "(D-Assign) §6.8" H H₁ .linearOverwrite
                 | .affine =>
                     traced t.steps d Θ (.assign i e) "(D-Assign) §6.8 (overwrite-drop)"
                       H (H₁.set ℓ (.full v)) [.drop ℓ vOld] (.value .unit)
                       (.ok (H₁.set ℓ (.full v)) .unit (tr ++ [.drop ℓ vOld]))
                 | .copy =>
                     traced t.steps d Θ (.assign i e) "(D-Assign) §6.8" H (H₁.set ℓ (.full v)) []
                       (.value .unit) (.ok (H₁.set ℓ (.full v)) .unit tr))
      | .panic k => propagate t.steps d Θ (.assign i e) "(D-Assign) §6.8" H H (.panic k)
      | .stuck w => propagate t.steps d Θ (.assign i e) "(D-Assign) §6.8" H H (.stuck w)
  | .seq e₁ e₂ =>
      let t₁ := traceEval (d + 1) Θ H ρ e₁
      match t₁.res with
      | .ok H₁ v₁ tr₁ =>
          (match v₁.mult with
           | .linear => refused t₁.steps d Θ (.seq e₁ e₂) "(D-Seq) §6.7" H H₁ .linearDiscard
           | .affine =>
               let discard := mkStep (d + 1) Θ (.seq e₁ e₂) "(D-Seq) §6.7 (drop the temporary)"
                 H₁ H₁ [.dropTemp v₁] (.value .unit)
               let t₂ := traceEval (d + 1) Θ H₁ ρ e₂
               traced (t₁.steps ++ [discard] ++ t₂.steps) d Θ (.seq e₁ e₂) "(D-Seq) §6.7"
                 H (storeOf H₁ t₂.res) [] (StepRes.ofRes t₂.res)
                 (t₂.res.withTrace (tr₁ ++ [.dropTemp v₁]))
           | .copy =>
               let t₂ := traceEval (d + 1) Θ H₁ ρ e₂
               traced (t₁.steps ++ t₂.steps) d Θ (.seq e₁ e₂) "(D-Seq) §6.7"
                 H (storeOf H₁ t₂.res) [] (StepRes.ofRes t₂.res) (t₂.res.withTrace tr₁))
      | .panic k => propagate t₁.steps d Θ (.seq e₁ e₂) "(D-Seq) §6.7" H H (.panic k)
      | .stuck w => propagate t₁.steps d Θ (.seq e₁ e₂) "(D-Seq) §6.7" H H (.stuck w)
  | .ite c e₁ e₂ =>
      let t₀ := traceEval (d + 1) Θ H ρ c
      match t₀.res with
      | .ok H₀ (.bool b) tr₀ =>
          if b then
            let t₁ := traceEval (d + 1) Θ H₀ ρ e₁
            traced (t₀.steps ++ t₁.steps) d Θ (.ite c e₁ e₂) "(D-If-T) §6.6"
              H (storeOf H₀ t₁.res) [] (StepRes.ofRes t₁.res) (t₁.res.withTrace tr₀)
          else
            let t₂ := traceEval (d + 1) Θ H₀ ρ e₂
            traced (t₀.steps ++ t₂.steps) d Θ (.ite c e₁ e₂) "(D-If-F) §6.6"
              H (storeOf H₀ t₂.res) [] (StepRes.ofRes t₂.res) (t₂.res.withTrace tr₀)
      | .ok H₀ (.int _) _ => confused t₀.steps d Θ (.ite c e₁ e₂) "(D-If-T)/(D-If-F) §6.6" H H₀
      | .ok H₀ .unit _ => confused t₀.steps d Θ (.ite c e₁ e₂) "(D-If-T)/(D-If-F) §6.6" H H₀
      | .ok H₀ (.res _ _) _ => confused t₀.steps d Θ (.ite c e₁ e₂) "(D-If-T)/(D-If-F) §6.6" H H₀
      | .panic k => propagate t₀.steps d Θ (.ite c e₁ e₂) "(D-If-T)/(D-If-F) §6.6" H H (.panic k)
      | .stuck w => propagate t₀.steps d Θ (.ite c e₁ e₂) "(D-If-T)/(D-If-F) §6.6" H H (.stuck w)

/-- **The trace is the machine.** Projecting a run to its final result
reproduces `eval H ρ e` exactly, so a rendered step table can never report
an outcome — a value, a §6.12 trap, or a refusal — the interpreter does not
produce. -/
theorem traceEval_res : ∀ (e : Expr) (d : Nat) (Θ : List Ty) (H : Store) (ρ : Env),
    (traceEval d Θ H ρ e).res = eval H ρ e := by
  intro e
  induction e with
    (intro d Θ H ρ
     simp only [traceEval, eval])
  | intLit n => rfl
  | boolLit b => rfl
  | unitLit => rfl
  | use i =>
      (repeat' split) <;> first | rfl | (simp_all [traced] <;> grind)
  | add e₁ e₂ ih₁ ih₂ =>
      simp only [ih₁, ih₂]
      (repeat' split) <;> first | rfl | (simp_all [traced, propagate] <;> grind)
  | div e₁ e₂ ih₁ ih₂ =>
      simp only [ih₁, ih₂]
      (repeat' split) <;> first | rfl | (simp_all [traced, propagate] <;> grind)
  | lt e₁ e₂ ih₁ ih₂ =>
      simp only [ih₁, ih₂]
      (repeat' split) <;> first | rfl | (simp_all [traced, propagate] <;> grind)
  | mkres κ e ih =>
      simp only [ih]
      (repeat' split) <;> first | rfl | (simp_all [traced, propagate] <;> grind)
  | consume e ih =>
      simp only [ih]
      (repeat' split) <;> first | rfl | (simp_all [traced, propagate] <;> grind)
  | drop i =>
      (repeat' split) <;> first | rfl | (simp_all [traced] <;> grind)
  | letIn m e₁ e₂ ih₁ ih₂ =>
      simp only [ih₁, ih₂]
      (repeat' split) <;> first | rfl | (simp_all [traced, refused, propagate, EvalRes.withTrace] <;> grind)
  | assign i e ih =>
      simp only [ih]
      (repeat' split) <;> first | rfl | (simp_all [traced, refused, propagate] <;> grind)
  | seq e₁ e₂ ih₁ ih₂ =>
      simp only [ih₁, ih₂]
      (repeat' split) <;> first | rfl | (simp_all [traced, propagate, EvalRes.withTrace] <;> grind)
  | ite c e₁ e₂ ihc ih₁ ih₂ =>
      simp only [ihc, ih₁, ih₂]
      (repeat' split) <;> first | rfl | (simp_all [traced, propagate, EvalRes.withTrace] <;> grind)

end Explain
end RueCore
