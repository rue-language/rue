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

Node labels name a calculus rule only where the node *is* an instance of
it. A struct literal is one — it is labelled (Struct-Intro) §5.8 and
(D-Struct) §6.5 — but the fragment's `consume` is not: the calculus takes a
struct apart through a projection, which is a place and so RUE-2231's, so
that node is labelled "whole-value elimination" with a section pointer and
its rejections say what the restriction is, rather than claiming a rule the
mechanization does not cover.

## Frames in the step table

A run spans frames: a call pushes one and a `return` or a normal completion
pops one. Four administrative rows make that visible, in the machine's own
vocabulary rather than as the expression they belong to: `(D-Call) §6.9
(push the frame)`, which shows the minted parameter cells; `(D-Return-Value)
§6.9 (pop the frame)`, which shows the drops `run-all-scope-drops` ran;
`(D-Return) §6.9 (unwind the frame)`, which shows the same walk taken early;
and the row where a call takes an unwound `return` as its value, labelled
`(D-Return)/(D-Return-Main)` because which of the two fired depends on
whether the call is the bottom of the stack, and a single row cannot tell.
A callee's rows are nested one depth further and spelled with the callee's
own binder names.

## Fuel

`traceEval` is indexed by the same fuel as `eval` and reports `outOfFuel` the
same way: one row saying the interpreter stopped. `fuel_mono`
(`Soundness.lean`) is why a rendering at a sufficient bound is *the*
rendering.
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
partial def exprLine (P : Program) (R : Ty) : List Ty → Expr → String
  | _, .intLit _ _ n => if n < 0 then "(" ++ toString n ++ ")" else toString n
  | _, .floatLit _ l => l.spell
  | _, .boolLit b => if b then "true" else "false"
  | _, .unitLit => "()"
  | Γ, .use i => Print.useName Γ i
  | Γ, .binop .totalCmp e₁ e₂ =>
      "@total_cmp(" ++ exprLine P R Γ e₁ ++ ", " ++ exprLine P R Γ e₂ ++ ")"
  | Γ, .binop op e₁ e₂ =>
      "(" ++ exprLine P R Γ e₁ ++ " " ++ Print.binOpSym op ++ " " ++ exprLine P R Γ e₂ ++ ")"
  | Γ, .unop op e => "(" ++ Print.unOpSym op ++ exprLine P R Γ e ++ ")"
  | Γ, .intCast w s e =>
      "@intCast<" ++ Print.tyName (.int w s) ++ ">(" ++ exprLine P R Γ e ++ ")"
  | Γ, .fintrin k e => Print.fintrinName k ++ "(" ++ exprLine P R Γ e ++ ")"
  | _, .panic msg => "@panic(" ++ Print.quoted msg ++ ")"
  | Γ, .dbg e => "@dbg(" ++ exprLine P R Γ e ++ ")"
  | Γ, .mkStruct s args =>
      Print.tyName (.struct s) ++ " { " ++
        String.intercalate ", "
          (Print.fieldInits 0 (args.map (fun a => exprLine P R Γ a))) ++ " }"
  | Γ, .consume e =>
      let s := match Print.tyOf P R Γ e with
        | some (.struct s) => s
        | _ => 0
      Print.consumeName s ++ "(" ++ exprLine P R Γ e ++ ")"
  | Γ, .drop i => "@drop(" ++ Print.useName Γ i ++ ")"
  | Γ, .letIn m e₁ e₂ =>
      let T₁ := (Print.tyOf P R Γ e₁).getD (.int .w64 .signed)
      "{ let " ++ (if m then "mut " else "") ++ Print.binderName Γ.length ++ ": " ++
        Print.tyName T₁ ++ " = " ++ exprLine P R Γ e₁ ++ "; " ++
        exprLine P R (T₁ :: Γ) e₂ ++ " }"
  | Γ, .assign i e => "{ " ++ Print.useName Γ i ++ " = " ++ exprLine P R Γ e ++ "; }"
  | Γ, .seq e₁ e₂ => "{ " ++ exprLine P R Γ e₁ ++ "; " ++ exprLine P R Γ e₂ ++ " }"
  | Γ, .ite c e₁ e₂ =>
      "if " ++ exprLine P R Γ c ++ " { " ++ exprLine P R Γ e₁ ++ " } else { " ++
        exprLine P R Γ e₂ ++ " }"
  | Γ, .call f args =>
      Print.fnName f ++ "(" ++
        String.intercalate ", " (args.map (fun a => exprLine P R Γ a)) ++ ")"
  | Γ, .ret e => "return " ++ exprLine P R Γ e

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
  | .int w s _ => .int w s
  | .float w _ => .float w
  | .bool _ => .bool
  | .unit => .unit
  | .struct s _ => .struct s

/-- (helper) A value, as §6.1 writes it: a scalar as itself, a struct value
as `{ v1, …, vk }_S` with its declaration's name. -/
partial def valLine : Val → String
  | .int _ _ n => toString n
  | .float w f => f.render w
  | .bool b => if b then "true" else "false"
  | .unit => "()"
  | .struct s vs =>
      Print.tyName (.struct s) ++ " { " ++
        String.intercalate ", " (vs.map valLine) ++ " }"

/-- (helper) A store cell (§6.1's `c ::= v | ⊘`, plus the retired `†`). -/
def cellLine : Cell → String
  | .full v => valLine v
  | .moved => "⊘ (moved out)"
  | .dead => "† (retired)"

/-- (helper) A store location. Locations are indices and are never reused
(§6.1). -/
def locName (ℓ : Nat) : String := "ℓ" ++ toString ℓ

/-- (helper) A list of locations, as a scope record is written (§6.1). -/
def locsLine (locs : List Nat) : String :=
  "[" ++ String.intercalate ", " (locs.map locName) ++ "]"

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

/-- (helper) A function's signature, as §2's `F` production writes it and as
`Print.lean` prints it. -/
def fnHeader (i : Nat) (fd : FnDef) : String :=
  "fn " ++ Print.fnName i ++ "(" ++
    String.intercalate ", " (Print.paramList 0 fd.params) ++ ") -> " ++
    Print.tyName fd.ret

/-- (helper) One drop event (§6.7/§6.8/§6.9/§6.11): where a drop starts, and
each user destructor it runs — the one event a printed Rue program can
observe (`Print.lean`). -/
def eventLine : Event → String
  | .drop ℓ v => "drop " ++ locName ℓ ++ " = " ++ valLine v
  | .dropTemp v => "drop temporary " ++ valLine v
  | .dtor s v => "run drop fn " ++ Print.tyName (.struct s) ++ "(" ++ valLine v ++ ")"
  | .dbg v => "@dbg prints " ++ valLine v

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

/-- Elaboration resolves an integer literal to a concrete `int(w,s)` and
rejects one that does not denote a value of it (`4.1:2`, `4.1:3`), so the
core never sees an out-of-range literal. -/
def litOutOfRange (T : Ty) : String :=
  "the integer literal does not denote a value of " ++ Print.tyName T ++
  " — elaboration resolves a literal to a concrete int(w,s) and rejects one out of " ++
  "its range (4.1:2, 4.1:3); the bounds are `intMin`/`intMax` in `Syntax.lean`"

/-- (helper) A premise whose own derivation failed: the reason is the
rejected sub-derivation nested under this rule, not this rule itself. -/
def subDerivation : String :=
  "a premise's own derivation failed — the reason is the rejected premise nested " ++
  "under this rule"

/-- (Arith)/(Ord) premise: the left operand is an integer (§5.8; `4.2:1`).
The bitwise and shift operators reject a `bool` operand for the same reason
(`4.3a:18`, `4.3a:19`). -/
def operandNotInt (T : Ty) : String :=
  "an operand has type " ++ Print.tyName T ++ ", but (Arith)/(Ord) require both operands " ++
  "to share one int(w,s) (§5.8; 4.2:1; the bitwise and shift operators take no other " ++
  "type at all, 4.3a:18, 4.3a:19)"

/-- (helper) The operand is not a scalar any binary rule of §5.8 admits. -/
def operandNotScalar (T : Ty) : String :=
  "the operand has type " ++ Print.tyName T ++
    ", and §5.8 states the binary operators at int(w,s) ((Arith)/(Ord)) and at float(w) " ++
    "((Float-Arith)/(Float-Ord)/(Total-Cmp)) only"

/-- (helper) The operator has no rule at the operands' float type: `%` and
the bitwise and shift operators are rejected on floats by the absence of a
rule (`3.12:25`, §5.8). -/
def opNotOnFloat (op : BinOp) (T : Ty) : String :=
  "§5.8 gives `" ++ Print.binOpSym op ++ "` no rule at " ++ Print.tyName T ++
    ": (Float-Arith) admits + - * / only (3.12:25), and the bitwise and shift operators " ++
    "are stated at int(w,s), a float being a datum rather than a bit pattern"

/-- (helper) `@total_cmp` has no integer rule (`3.12:31`). -/
def opNotOnInt (op : BinOp) (T : Ty) : String :=
  "§5.8 gives `@total_cmp` no rule at " ++ Print.tyName T ++
    ": 3.12:31 gives it two operands of one floating-point type" ++
    (if op = .totalCmp then "" else "")

/-- (helper) A float intrinsic whose operand is not a float. -/
def fintrinNotFloat (k : FloatIntrin) (T : Ty) : String :=
  "§5.8's rule for `" ++ Print.fintrinName k ++ "` takes a float(w) operand; this one has type " ++
    Print.tyName T

/-- (helper) `@int_to_float` takes an integer operand (`3.12:16`). -/
def intToFloatNotInt (T : Ty) : String :=
  "(Int-To-Float) §5.8 takes an operand of an integer type (3.12:16); this one has type " ++
    Print.tyName T

/-- (helper) `@float_cast` converts between the two widths and only between
them (`3.12:19`). -/
def floatCastSameWidth (T : Ty) : String :=
  "(Float-Cast) §5.8 carries the side condition w' ≠ w (3.12:19): @float_cast converts " ++
    "between f32 and f64 and only between them, and both ends here are " ++ Print.tyName T

/-- (Arith)/(Ord) premise: **one** `int(w,s)` for both operands (§5.8;
`4.2:1`; there is no implicit widening). For a shift this is `4.3a:9`: the
amount has the shifted value's own type. -/
def operandWidthMismatch (T₁ T₂ : Ty) : String :=
  "the operands have types " ++ Print.tyName T₁ ++ " and " ++ Print.tyName T₂ ++
  ", but (Arith)/(Ord) require one int(w,s) for both and Rue has no implicit " ++
  "widening (§5.8; 4.2:1; for a shift the amount takes the shifted value's own " ++
  "type, 4.3a:9)"

/-- (Neg) §5.8's signed premise (`4.2:6`; rejecting `neg` on an unsigned type
is `4.2:14`). -/
def negNotSigned (T : Ty) : String :=
  "the operand has type " ++ Print.tyName T ++ ", but (Neg) negates a signed integer " ++
  "only — there is no value for it to produce on an unsigned type (§5.8; 4.2:6, 4.2:14)"

/-- (Not) §5.8's `bool` premise (`4.4:2`). -/
def notNotBool (T : Ty) : String :=
  "the operand has type " ++ Print.tyName T ++ ", but (Not) demands bool (§5.8; 4.4:2)"

/-- (BitNot) §5.8's integer premise (`4.3a:3`, `4.3a:4`, `4.3a:18`). -/
def bitnotNotInt (T : Ty) : String :=
  "the operand has type " ++ Print.tyName T ++ ", but the bitwise complement acts on an " ++
  "integer's w-bit pattern (§5.8; 4.3a:3, 4.3a:4; 4.3a:18 restricts every bitwise " ++
  "operator to the integer types)"

/-- `@intCast`'s operand premise (`4.13:25`). -/
def castNotInt (T : Ty) : String :=
  "the operand has type " ++ Print.tyName T ++ ", but `@intCast` converts between the " ++
  "integer types only (4.13:25; its target is likewise an integer type, 4.13:27)"

/-- (Dbg) §5.8's operand restriction, which is the compiler's (E0702). -/
def dbgNotObservable (T : Ty) : String :=
  "the operand has type " ++ Print.tyName T ++ ", but `@dbg` renders a scalar — an " ++
  "int(w,s), a float or a bool ((Dbg) §5.8; the compiler rejects an aggregate with " ++
  "E0702), and the fragment has no floats"

/-- (Struct-Intro) §5.8's first premise, `S = struct { f1: T1, …, fk: Tk }`:
the program has no declaration at this index. Elaboration resolves a type's
name before the core (§2), so no elaborated program reaches this. -/
def unknownStruct : String :=
  "the program has no struct declaration at this index ((Struct-Intro) premise " ++
  "`S = struct { f1: T1, …, fk: Tk }`, §5.8; elaboration resolves a type name before " ++
  "the core, §2, so no elaborated program reaches this premise)"

/-- (Struct-Intro) §5.8's "all k fields supplied, each exactly once"
(`3.6:5`, `3.6:6`). -/
def fieldCountMismatch : String :=
  "the literal does not supply exactly one initializer per declared field " ++
  "((Struct-Intro) premise `all k fields supplied, each exactly once`, §5.8; 3.6:5, 3.6:6)"

/-- (Struct-Intro) §5.8's per-field premise `Γ;Σ_{i-1};Λ ⊢ ei ⇒ Ti ⊣ Σi`. -/
def fieldTypeMismatch (T field : Ty) : String :=
  "a field initializer has type " ++ Print.tyName T ++ " but its field is declared " ++
  Print.tyName field ++ " ((Struct-Intro) premise `Γ;Σ_{i-1};Λ ⊢ ei ⇒ Ti ⊣ Σi`, §5.8; " ++
  "initializers are presented in declaration order, 3.6:15)"

/-- The fragment's whole-value elimination takes a struct by value, which is
a §4.2 use of its operand's places. -/
def consumeNotStruct (T : Ty) : String :=
  "the eliminated operand has type " ++ Print.tyName T ++ ", but the fragment's " ++
  "whole-value elimination takes a struct by value (§4.2; the calculus reads a field " ++
  "through a projection, which is a place and so out of this fragment)"

/-- The fragment's whole-value elimination is defined only on a declaration
with an `int` first field, every field `int`, and no destructor — a
destructor would make even the read a rejected projection (`3.9:34`,
E0456). -/
def notConsumable : String :=
  "the eliminated struct is not one the fragment can take apart: it needs a first " ++
  "field, every field an int, and no destructor — `3.9:34` (E0456) rejects every " ++
  "projection out of a value whose type declares one (`StructDecl.Consumable`; the " ++
  "calculus's own eliminator is a projection, which this fragment does not have)"

/-- §5.6 scope exit, the residual-linear leak check; prose `3.8:32`. -/
def letLeak (T : Ty) : String :=
  "the residual state of the `let` binder is Owned and its type " ++ Print.tyName T ++
  " is Linear — a linear value reached end of scope unconsumed " ++
  "(§5.6 leak check; 3.8:32; the compiler reports E0406)"

/-- (helper) Unreachable: `Typed.skel_preserved` forbids a rule from
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

/-- (helper) Unreachable: the target survives the right-hand side by
skeleton preservation (`Typed.skel_preserved`). -/
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

/-- (Call) §5.8's first premise, `Γ ⊢ g : (T₁,…,Tₘ) → Tr`: the program has
no function at this index. Elaboration resolves a callee's name before the
core (§2), so no elaborated program reaches this. -/
def unknownCallee : String :=
  "the program has no function at this index ((Call) premise " ++
  "`g : fn(m1 x1:T1, …) -> Tr`, §5.8; elaboration resolves a callee's name before " ++
  "the core, §2, so no elaborated program reaches this premise)"

/-- (Call) §5.8's arity premise (`4.10:3`). -/
def argCountMismatch : String :=
  "the argument list does not match the callee's parameter list in count " ++
  "((Call) premise `the m argument forms a1..am match the m parameters in count and mode`, " ++
  "§5.8; 4.10:3)"

/-- (Call) §5.8's per-argument premise (`4.10:4`). -/
def argTypeMismatch (T param : Ty) : String :=
  "an argument has type " ++ Print.tyName T ++ " but its parameter is declared " ++
  Print.tyName param ++ " ((Call) premise `Γ;Σ_{i-1};Λ ⊢ a_i ⇒ Ti ⊣ Σi`, §5.8; 4.10:4)"

/-- (Return-Value) §5.7's premise `e ⇒ T_ret`: the operand is checked
against the enclosing function's declared return type. -/
def returnTypeMismatch (T R : Ty) : String :=
  "the returned expression has type " ++ Print.tyName T ++ " but the function is " ++
  "declared to return " ++ Print.tyName R ++ " ((Return-Value) premise " ++
  "`Γ;Σ;Λ ⊢ e ⇒ T_ret ⊣ Σ_e`, §5.7; 4.10:5). The checker types a `return` at the " ++
  "enclosing return type rather than at whatever its context wants, which is the " ++
  "one place it is narrower than the rule (`Checker.lean`)"

/-- (Return-Value) §5.7's `⊥_exit` obligation, which is §5.6's scope-exit
check taken frame-wide (`3.8:62`, and (Fn) §5.8's second clause). -/
def returnLeak : String :=
  "a binding of this frame is still Owned at a linear type where the `return` ends " ++
  "every open scope at once — the `⊥_exit` edge carries §5.6's obligation " ++
  "((Return-Value) §5.7; (Fn) §5.8's second clause; 3.8:62; the compiler reports E0406)"

end Premise

/-- (helper) The rule name a binary operator's node carries: §5.8 types the
ordering compares by (Ord) and everything else in the `⊕` set by (Arith). -/
def binopRule (op : BinOp) : String :=
  if op.isCompare then "(Ord) §5.8" else "(Arith) §5.8"

/-- (helper) The §5.8 rule a one-operand float intrinsic applies. -/
def fintrinRule : FloatIntrin → String
  | .intToFloat _ => "(Int-To-Float) §5.8"
  | .floatToInt _ _ => "(Float-To-Int) §5.8"
  | .floatCast _ => "(Float-Cast) §5.8"
  | .roundOp _ => "(Float-Round) §5.8"

/-- (helper) The §5.8 rule a binary operator applies at a float operand type:
(Float-Arith), (Float-Ord) or (Total-Cmp). -/
def floatBinopRule (op : BinOp) : String :=
  if op = .totalCmp then "(Total-Cmp) §5.8"
  else if op.isCompare then "(Float-Ord) §5.8" else "(Float-Arith) §5.8"

/-- (helper) The first entry on which the §5.5 join fails, named as the
source names it, so a join rejection can point at a binding. -/
def joinConflictEntry (D : StructEnv) : Ctx → Ctx → Option String
  | a :: as, b :: bs =>
      if (a.join D b).isNone then
        some (Print.binderName as.length ++ ": " ++ Print.tyName a.ty ++ " is " ++
          ownStateName a.st ++ " in the then-arm and " ++ ownStateName b.st ++ " in the else-arm")
      else joinConflictEntry D as bs
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

mutual
/-- The instrumented mirror of `check` (§5): the same algorithm, recording
the rule it applied at every node and, where it rejects, the premise that
failed. `explain_result` proves the two agree. -/
def explain (P : Program) (R : Ty) (Γ : Ctx) : Expr → Deriv
  | .intLit w s n =>
      if InBounds w s n then accepted "(Lit) §5.8" Γ (.intLit w s n) (.int w s) Γ []
      else rejected "(Lit) §5.8" Γ (.intLit w s n) (Premise.litOutOfRange (.int w s)) []
  | .boolLit b => accepted "(Lit) §5.8" Γ (.boolLit b) .bool Γ []
  | .unitLit => accepted "(Lit) §5.8" Γ .unitLit .unit Γ []
  | .use i =>
      match Γ[i]? with
      | none => rejected "(Use-Copy)/(Use-Move) §5.1" Γ (.use i) Premise.unboundIndex []
      | some en =>
        match en.st with
        | .movedOut => rejected "(Use-Copy)/(Use-Move) §5.1" Γ (.use i) Premise.useMovedOut []
        | .owned =>
          if en.ty.mult P.structs = .copy then
            accepted "(Use-Copy) §5.1" Γ (.use i) en.ty Γ []
          else
            accepted "(Use-Move) §5.1" Γ (.use i) en.ty (Γ.set i (en.setSt .movedOut)) []
  | .binop op e₁ e₂ =>
      let rule := binopRule op
      let frule := floatBinopRule op
      let d₁ := explain P R Γ e₁
      match d₁.result with
      | some (.int w s, Γ₁) =>
        let d₂ := explain P R Γ₁ e₂
        (match d₂.result with
         | some (.int w' s', Γ₂) =>
             if w' = w ∧ s' = s ∧ op.intAdmits = true then
               accepted rule Γ (.binop op e₁ e₂) (op.resultTy (.int w s)) Γ₂ [d₁, d₂]
             else if w' = w ∧ s' = s then
               rejected rule Γ (.binop op e₁ e₂)
                 (Premise.opNotOnInt op (.int w s)) [d₁, d₂]
             else
               rejected rule Γ (.binop op e₁ e₂)
                 (Premise.operandWidthMismatch (.int w s) (.int w' s')) [d₁, d₂]
         | some (T, _) => rejected rule Γ (.binop op e₁ e₂) (Premise.operandNotInt T) [d₁, d₂]
         | none => rejected rule Γ (.binop op e₁ e₂) Premise.subDerivation [d₁, d₂])
      | some (.float w, Γ₁) =>
        let d₂ := explain P R Γ₁ e₂
        (match d₂.result with
         | some (.float w', Γ₂) =>
             if w' = w ∧ op.floatAdmits = true then
               accepted frule Γ (.binop op e₁ e₂) (op.resultTy (.float w)) Γ₂ [d₁, d₂]
             else if w' = w then
               rejected frule Γ (.binop op e₁ e₂)
                 (Premise.opNotOnFloat op (.float w)) [d₁, d₂]
             else
               rejected frule Γ (.binop op e₁ e₂)
                 (Premise.operandWidthMismatch (.float w) (.float w')) [d₁, d₂]
         | some (T, _) => rejected frule Γ (.binop op e₁ e₂) (Premise.operandNotScalar T) [d₁, d₂]
         | none => rejected frule Γ (.binop op e₁ e₂) Premise.subDerivation [d₁, d₂])
      | some (T, _) => rejected rule Γ (.binop op e₁ e₂) (Premise.operandNotScalar T) [d₁]
      | none => rejected rule Γ (.binop op e₁ e₂) Premise.subDerivation [d₁]
  | .floatLit w l => accepted "(Lit) §5.8" Γ (.floatLit w l) (.float w) Γ []
  | .fintrin (.intToFloat w) e =>
      let d := explain P R Γ e
      (match d.result with
       | some (.int _ _, Γ') =>
           accepted "(Int-To-Float) §5.8" Γ (.fintrin (.intToFloat w) e) (.float w) Γ' [d]
       | some (T, _) =>
           rejected "(Int-To-Float) §5.8" Γ (.fintrin (.intToFloat w) e)
             (Premise.intToFloatNotInt T) [d]
       | none =>
           rejected "(Int-To-Float) §5.8" Γ (.fintrin (.intToFloat w) e)
             Premise.subDerivation [d])
  | .fintrin k e =>
      let rule := fintrinRule k
      let d := explain P R Γ e
      (match d.result with
       | some (.float w, Γ') =>
           if k.floatSrc w then
             accepted rule Γ (.fintrin k e) (k.resTy w) Γ' [d]
           else
             rejected rule Γ (.fintrin k e) (Premise.floatCastSameWidth (.float w)) [d]
       | some (T, _) => rejected rule Γ (.fintrin k e) (Premise.fintrinNotFloat k T) [d]
       | none => rejected rule Γ (.fintrin k e) Premise.subDerivation [d])
  | .unop .neg e =>
      let d := explain P R Γ e
      (match d.result with
       | some (.int w .signed, Γ') =>
           accepted "(Neg) §5.8" Γ (.unop .neg e) (.int w .signed) Γ' [d]
       | some (.float w, Γ') =>
           accepted "(Float-Neg) §5.8" Γ (.unop .neg e) (.float w) Γ' [d]
       | some (T, _) => rejected "(Neg) §5.8" Γ (.unop .neg e) (Premise.negNotSigned T) [d]
       | none => rejected "(Neg) §5.8" Γ (.unop .neg e) Premise.subDerivation [d])
  | .unop .not e =>
      let d := explain P R Γ e
      (match d.result with
       | some (.bool, Γ') => accepted "(Not) §5.8" Γ (.unop .not e) .bool Γ' [d]
       | some (T, _) => rejected "(Not) §5.8" Γ (.unop .not e) (Premise.notNotBool T) [d]
       | none => rejected "(Not) §5.8" Γ (.unop .not e) Premise.subDerivation [d])
  | .unop .bitnot e =>
      let d := explain P R Γ e
      (match d.result with
       | some (.int w s, Γ') =>
           accepted "(BitNot) §5.8" Γ (.unop .bitnot e) (.int w s) Γ' [d]
       | some (T, _) =>
           rejected "(BitNot) §5.8" Γ (.unop .bitnot e) (Premise.bitnotNotInt T) [d]
       | none => rejected "(BitNot) §5.8" Γ (.unop .bitnot e) Premise.subDerivation [d])
  | .intCast w s e =>
      let d := explain P R Γ e
      (match d.result with
       | some (.int _ _, Γ') =>
           accepted "(Int-Cast) §5.8" Γ (.intCast w s e) (.int w s) Γ' [d]
       | some (T, _) =>
           rejected "(Int-Cast) §5.8" Γ (.intCast w s e) (Premise.castNotInt T) [d]
       | none => rejected "(Int-Cast) §5.8" Γ (.intCast w s e) Premise.subDerivation [d])
  | .panic msg => accepted "(Panic) §5.8 + (Sub-Never) §5.7" Γ (.panic msg) R Γ []
  | .dbg e =>
      let d := explain P R Γ e
      (match d.result with
       | some (T, Γ') =>
           if T.observable then accepted "(Dbg) §5.8" Γ (.dbg e) .unit Γ' [d]
           else rejected "(Dbg) §5.8" Γ (.dbg e) (Premise.dbgNotObservable T) [d]
       | none => rejected "(Dbg) §5.8" Γ (.dbg e) Premise.subDerivation [d])
  | .mkStruct s args =>
      match P.structs[s]? with
      | none => rejected "(Struct-Intro) §5.8" Γ (.mkStruct s args) Premise.unknownStruct []
      | some sd =>
        (match explainArgs P R Γ args sd.fields with
         | (some Γ', kids) =>
             accepted "(Struct-Intro) §5.8" Γ (.mkStruct s args) (.struct s) Γ' kids
         | (none, kids) =>
             rejected "(Struct-Intro) §5.8" Γ (.mkStruct s args)
               (fieldsPremise P R Γ args sd.fields) kids)
  | .consume e =>
      let d := explain P R Γ e
      match d.result with
      | some (.struct s, Γ') =>
        (match P.structs[s]? with
         | none =>
             rejected "§5.8 whole-value elimination" Γ (.consume e) Premise.unknownStruct [d]
         | some sd =>
             if sd.Consumable then
               accepted "§5.8 whole-value elimination" Γ (.consume e) sd.payloadTy Γ' [d]
             else rejected "§5.8 whole-value elimination" Γ (.consume e) Premise.notConsumable [d])
      | some (.float w, _) =>
          rejected "§5.8 whole-value elimination" Γ (.consume e)
            (Premise.consumeNotStruct (.float w)) [d]
      | some (.int w s, _) =>
          rejected "§5.8 whole-value elimination" Γ (.consume e)
            (Premise.consumeNotStruct (.int w s)) [d]
      | some (.bool, _) =>
          rejected "§5.8 whole-value elimination" Γ (.consume e) (Premise.consumeNotStruct .bool) [d]
      | some (.unit, _) =>
          rejected "§5.8 whole-value elimination" Γ (.consume e) (Premise.consumeNotStruct .unit) [d]
      | none => rejected "§5.8 whole-value elimination" Γ (.consume e) Premise.subDerivation [d]
  | .drop i =>
      match Γ[i]? with
      | none => rejected "(@Drop-Copy)/(@Drop) §5.3" Γ (.drop i) Premise.unboundIndex []
      | some en =>
        match en.st with
        | .movedOut => rejected "(@Drop-Copy)/(@Drop) §5.3" Γ (.drop i) Premise.dropMovedOut []
        | .owned =>
          if en.ty.mult P.structs = .copy then
            accepted "(@Drop-Copy) §5.3" Γ (.drop i) .unit Γ []
          else
            accepted "(@Drop) §5.3" Γ (.drop i) .unit (Γ.set i (en.setSt .movedOut)) []
  | .letIn m e₁ e₂ =>
      let d₁ := explain P R Γ e₁
      match d₁.result with
      | none => rejected "(Let) §5.3 + the §5.6 scope-exit leak check" Γ (.letIn m e₁ e₂)
                  Premise.subDerivation [d₁]
      | some (T₁, Γ₁) =>
        let d₂ := explain P R ({ ty := T₁, mu := m, st := .owned } :: Γ₁) e₂
        (match d₂.result with
         | some (T₂, en' :: Γ₂) =>
             if en'.st = .owned ∧ T₁.mult P.structs = .linear then
               rejected "(Let) §5.3 + the §5.6 scope-exit leak check" Γ (.letIn m e₁ e₂)
                 (Premise.letLeak T₁) [d₁, d₂]
             else
               accepted "(Let) §5.3 + the §5.6 scope-exit leak check" Γ (.letIn m e₁ e₂) T₂ Γ₂ [d₁, d₂]
         | some (_, []) =>
             rejected "(Let) §5.3 + the §5.6 scope-exit leak check" Γ (.letIn m e₁ e₂)
               Premise.letBinderLost [d₁, d₂]
         | none =>
             rejected "(Let) §5.3 + the §5.6 scope-exit leak check" Γ (.letIn m e₁ e₂)
               Premise.subDerivation [d₁, d₂])
  | .assign i e =>
      match Γ[i]? with
      | none => rejected "(Assign) §5.2, 3.8:77" Γ (.assign i e) Premise.unboundIndex []
      | some en₀ =>
        if en₀.mu = true then
          let d := explain P R Γ e
          (match d.result with
           | some (T, Γ₁) =>
             if T = en₀.ty then
               match Γ₁[i]? with
               | some en₁ =>
                   if en₁.st = .movedOut ∨ en₀.ty.mult P.structs ≠ .linear then
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
      let d₁ := explain P R Γ e₁
      match d₁.result with
      | some (T₁, Γ₁) =>
          if T₁.mult P.structs = .linear then
            rejected "(Seq) §5.3, 3.8:64" Γ (.seq e₁ e₂) (Premise.discardsLinear T₁) [d₁]
          else
            let d₂ := explain P R Γ₁ e₂
            (match d₂.result with
             | some (T₂, Γ₂) => accepted "(Seq) §5.3, 3.8:64" Γ (.seq e₁ e₂) T₂ Γ₂ [d₁, d₂]
             | none => rejected "(Seq) §5.3, 3.8:64" Γ (.seq e₁ e₂) Premise.subDerivation [d₁, d₂])
      | none => rejected "(Seq) §5.3, 3.8:64" Γ (.seq e₁ e₂) Premise.subDerivation [d₁]
  | .ite c e₁ e₂ =>
      let dc := explain P R Γ c
      match dc.result with
      | some (.bool, Γ₀) =>
        let d₁ := explain P R Γ₀ e₁
        let d₂ := explain P R Γ₀ e₂
        (match d₁.result, d₂.result with
         | some (T₁, Γ₁), some (T₂, Γ₂) =>
             if T₁ = T₂ then
               match Ctx.join P.structs Γ₁ Γ₂ with
               | some Γ' => accepted "(If) §5.5 join" Γ (.ite c e₁ e₂) T₁ Γ' [dc, d₁, d₂]
               | none =>
                   rejected "(If) §5.5 join" Γ (.ite c e₁ e₂)
                     (Premise.joinConflict (joinConflictEntry P.structs Γ₁ Γ₂)) [dc, d₁, d₂]
             else
               rejected "(If) §5.5 join" Γ (.ite c e₁ e₂)
                 (Premise.armTypeMismatch T₁ T₂) [dc, d₁, d₂]
         | _, _ => rejected "(If) §5.5 join" Γ (.ite c e₁ e₂) Premise.subDerivation [dc, d₁, d₂])
      | some (T, _) =>
          rejected "(If) §5.5 join" Γ (.ite c e₁ e₂) (Premise.condNotBool T) [dc]
      | none => rejected "(If) §5.5 join" Γ (.ite c e₁ e₂) Premise.subDerivation [dc]
  | .call f args =>
      match P.fns[f]? with
      | none => rejected "(Call) §5.8" Γ (.call f args) Premise.unknownCallee []
      | some fd =>
        (match explainArgs P R Γ args (fd.params.map Param.ty) with
         | (some Γ', kids) => accepted "(Call) §5.8" Γ (.call f args) fd.ret Γ' kids
         | (none, kids) =>
             rejected "(Call) §5.8" Γ (.call f args)
               (argsPremise P R Γ args (fd.params.map Param.ty)) kids)
  | .ret e =>
      let d := explain P R Γ e
      match d.result with
      | none => rejected "(Return-Value) §5.7" Γ (.ret e) Premise.subDerivation [d]
      | some (T, Γ₁) =>
          if T = R ∧ NoOwnedLinear P.structs Γ₁ then
            accepted "(Return-Value) §5.7" Γ (.ret e) R Γ₁ [d]
          else if T = R then
            rejected "(Return-Value) §5.7" Γ (.ret e) Premise.returnLeak [d]
          else
            rejected "(Return-Value) §5.7" Γ (.ret e) (Premise.returnTypeMismatch T R) [d]

/-- The instrumented mirror of `checkArgs` (§5.8's (Call) argument list):
the sub-derivations in argument order, and the outgoing `Σ` when every
argument checked at its parameter's type. -/
def explainArgs (P : Program) (R : Ty) : Ctx → List Expr → List Ty → Option Ctx × List Deriv
  | Γ, [], [] => (some Γ, [])
  | Γ, e :: es, T :: Ts =>
      let d := explain P R Γ e
      (match d.result with
       | some (T', Γ₁) =>
           if T' = T then
             let rest := explainArgs P R Γ₁ es Ts
             (rest.1, d :: rest.2)
           else (none, [d])
       | none => (none, [d]))
  | _, _, _ => (none, [])

/-- The premise a rejected argument list failed: a count mismatch (`4.10:3`),
or the first argument whose type is not its parameter's (`4.10:4`) — the two
per-argument premises of (Call) §5.8. -/
def argsPremise (P : Program) (R : Ty) : Ctx → List Expr → List Ty → String
  | _, [], [] => Premise.argCountMismatch
  | Γ, e :: es, T :: Ts =>
      (match (explain P R Γ e).result with
       | some (T', Γ₁) =>
           if T' = T then argsPremise P R Γ₁ es Ts else Premise.argTypeMismatch T' T
       | none => Premise.subDerivation)
  | _, _, _ => Premise.argCountMismatch

/-- The premise a rejected field list failed: the wrong number of
initializers (`3.6:5`, `3.6:6`), or the first one whose type is not its
field's (`3.6:15`: they are presented in declaration order) — the two
per-field premises of (Struct-Intro) §5.8. -/
def fieldsPremise (P : Program) (R : Ty) : Ctx → List Expr → List Ty → String
  | _, [], [] => Premise.fieldCountMismatch
  | Γ, e :: es, T :: Ts =>
      (match (explain P R Γ e).result with
       | some (T', Γ₁) =>
           if T' = T then fieldsPremise P R Γ₁ es Ts else Premise.fieldTypeMismatch T' T
       | none => Premise.subDerivation)
  | _, _, _ => Premise.fieldCountMismatch
end

mutual
/-- **The derivation is the checker.** Projecting a derivation to its
conclusion reproduces `check P R Γ e` exactly, so a rendered derivation can
never claim an acceptance or a rejection the verified checker (§5,
`check_sound`) does not make. -/
theorem explain_result {P : Program} {R : Ty} : ∀ (e : Expr) (Γ : Ctx),
    (explain P R Γ e).result = check P R Γ e
  | .intLit w s n, Γ => by
      simp only [explain, check]; split <;> rfl
  | .floatLit w l, Γ => rfl
  | .boolLit b, Γ => rfl
  | .unitLit, Γ => rfl
  | .use i, Γ => by
      simp only [explain, check]
      (repeat' split) <;> first | rfl | simp_all [accepted, Deriv.result]
  | .binop op e₁ e₂, Γ => by
      simp only [explain, check, explain_result e₁, explain_result e₂]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, rejected, Deriv.result] <;> grind)
  | .unop .neg e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, rejected, Deriv.result] <;> grind)
  | .unop .not e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, rejected, Deriv.result] <;> grind)
  | .unop .bitnot e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, rejected, Deriv.result] <;> grind)
  | .intCast w s e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, rejected, Deriv.result] <;> grind)
  | .fintrin (.intToFloat w) e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, rejected, Deriv.result] <;> grind)
  | .fintrin (.floatToInt w s) e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, rejected, Deriv.result] <;> grind)
  | .fintrin (.floatCast w) e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, rejected, Deriv.result] <;> grind)
  | .fintrin (.roundOp k) e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, rejected, Deriv.result] <;> grind)
  | .panic msg, Γ => rfl
  | .dbg e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, Deriv.result] <;> grind)
  | .mkStruct s args, Γ => by
      simp only [explain, check]
      cases hs : P.structs[s]? with
      | none => rfl
      | some sd =>
        dsimp only
        have hargs := explainArgs_result (P := P) (R := R) args Γ sd.fields
        revert hargs
        cases explainArgs P R Γ args sd.fields with
        | mk res kids =>
          intro hargs
          cases res with
          | none => rw [← hargs]; rfl
          | some Γ' => rw [← hargs]; rfl
  | .consume e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, rejected, Deriv.result] <;> grind)
  | .drop i, Γ => by
      simp only [explain, check]
      (repeat' split) <;> first | rfl | simp_all [accepted, Deriv.result]
  | .letIn m e₁ e₂, Γ => by
      simp only [explain, check, explain_result e₁, explain_result e₂]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, rejected, Deriv.result] <;> grind)
  | .assign i e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, rejected, Deriv.result] <;> grind)
  | .seq e₁ e₂, Γ => by
      simp only [explain, check, explain_result e₁, explain_result e₂]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, rejected, Deriv.result] <;> grind)
  | .ite c e₁ e₂, Γ => by
      simp only [explain, check, explain_result c]
      cases hc : check P R Γ c with
      | none => rfl
      | some p =>
        obtain ⟨T, Γ₀⟩ := p
        cases T with
        | int => rfl
        | float => rfl
        | unit => rfl
        | struct s' => rfl
        | bool =>
            simp only [explain_result e₁, explain_result e₂]
            cases h₁ : check P R Γ₀ e₁ with
            | none => cases h₂ : check P R Γ₀ e₂ <;> rfl
            | some q₁ =>
              cases h₂ : check P R Γ₀ e₂ with
              | none => rfl
              | some q₂ =>
                obtain ⟨T₁, Γ₁⟩ := q₁
                obtain ⟨T₂, Γ₂⟩ := q₂
                simp only []
                split
                · cases hj : Ctx.join P.structs Γ₁ Γ₂ <;> rfl
                · rfl
  | .call f args, Γ => by
      simp only [explain, check]
      cases hf : P.fns[f]? with
      | none => rfl
      | some fd =>
        dsimp only
        have hargs := explainArgs_result (P := P) (R := R) args Γ (fd.params.map Param.ty)
        revert hargs
        cases explainArgs P R Γ args (fd.params.map Param.ty) with
        | mk res kids =>
          intro hargs
          cases res with
          | none => rw [← hargs]; rfl
          | some Γ' => rw [← hargs]; rfl
  | .ret e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;> first | rfl | simp_all [accepted, Deriv.result]

/-- **The argument-list derivations are the checker's** ((Call) §5.8). -/
theorem explainArgs_result {P : Program} {R : Ty} : ∀ (es : List Expr) (Γ : Ctx) (Ts : List Ty),
    (explainArgs P R Γ es Ts).1 = checkArgs P R Γ es Ts
  | [], Γ, [] => rfl
  | [], Γ, _ :: _ => rfl
  | e :: es, Γ, [] => rfl
  | e :: es, Γ, T :: Ts => by
      simp only [explainArgs, checkArgs, explain_result e]
      cases hr : check P R Γ e with
      | none => rfl
      | some p =>
        obtain ⟨T', Γ₁⟩ := p
        simp only []
        split
        · exact explainArgs_result es Γ₁ Ts
        · rfl
end

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
      "a touch of a retired cell †: the binding's allocation was dropped and retired " ++
      "at a scope exit or a frame teardown ((D-EndScope) §6.1/§6.7, " ++
      "`run-all-scope-drops` §6.9; §7 “no use after drop”)"
  | .linearLeak =>
      "a scope exit or a frame teardown reached a live linear value: a linear " ++
      "obligation was never discharged (endscope §6.7 / `run-all-scope-drops` §6.9, " ++
      "the §5.6 leak check executed; 3.8:32; §7 “consumed exactly once”)"
  | .linearOverwrite =>
      "an overwrite-drop of a live linear value: the assignment would consume a linear " ++
      "value the program never consumed (§6.8; 3.8:77; §7)"
  | .linearDiscard =>
      "a sequence discarded a linear value: no §6 rule fires here — §5.3's discard " ++
      "check (3.8:64) should have rejected the program, so the interpreter names the " ++
      "refusal instead, and §7 forbids it"
  | .unbound =>
      "a dangling index or a callee the program does not have: elaboration resolves " ++
      "names before the core (§2), so no elaborated program reaches this"
  | .typeConfusion =>
      "an operator met a wrong-shaped value, or a call's argument count did not match " ++
      "its callee's parameter list; the statics (§5) exclude both, and `soundness` " ++
      "(§7) is the proof"

/-- What one node produced: a value, a value an unwinding `return` handed
past it (§6.9), a defined trap (§6.12), a refusal (§6's stuck states) with
the premise it broke, or the interpreter's admission that it ran out of
fuel. -/
inductive StepRes where
  | value (v : Val)
  | unwound (v : Val)
  | panicked (k : PanicKind)
  | refuse (why : Violation) (premise : String)
  | exhausted

/-- (helper) How a node reports a sub-result it only passes on. -/
def StepRes.ofRes : EvalRes → StepRes
  | .ok _ v _ => .value v
  | .returned _ v _ => .unwound v
  | .panic k _ => .panicked k
  | .stuck w => .refuse w (violationPremise w)
  | .outOfFuel => .exhausted

/-- One row of the step table: the node's nesting depth, the §6 rule it
took, the binder types in scope (so the expression prints with the source's
names), the expression, the store before and after, the drop events this
node emitted (§6.7/§6.8/§6.9/§6.11), and what it produced. -/
structure Step where
  depth : Nat
  rule : String
  binders : List Ty
  /-- The enclosing function's declared return type, which is the type
  `Checker.lean` gives a `return` — so a row inside a callee renders its
  expression with that callee's types, not the entry point's. -/
  retTy : Ty
  expr : Expr
  /-- What the expression column shows, when the row is one of the machine's
  administrative steps (`let`'s mint, `endscope`, a temporary's drop, a frame
  push or pop) and the whole expression it belongs to would only repeat the
  row above. -/
  shown : Option String
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
def mkStep (d : Nat) (Θ : List Ty) (R : Ty) (e : Expr) (rule : String) (H H' : Store)
    (evs : List Event) (sr : StepRes) : Step :=
  { depth := d, rule := rule, binders := Θ, retTy := R, expr := e, shown := none,
    storeBefore := H, storeAfter := H', events := evs, res := sr }

/-- (helper) One of the machine's administrative rows, shown in its own form
rather than as the expression it belongs to. -/
def adminStep (d : Nat) (Θ : List Ty) (R : Ty) (e : Expr) (rule shown : String)
    (H H' : Store) (evs : List Event) (sr : StepRes) : Step :=
  { mkStep d Θ R e rule H H' evs sr with shown := some shown }

/-- (helper) What a row's expression column shows: the machine's own form
for an administrative row, otherwise the node's expression in the surface
syntax, with the binder names and the return type of the frame the row
belongs to. -/
def Step.text (P : Program) (s : Step) : String :=
  s.shown.getD (exprLine P s.retTy s.binders s.expr)

/-- (helper) The store the machine actually reached: the one the most
recent recorded row left, or `fallback` when the node recorded none. No §6
rule un-allocates a cell, so a row that refuses or traps must never print a
store from before the rows that already ran. -/
def lastStore (kids : List Step) (fallback : Store) : Store :=
  match kids.getLast? with
  | some s => s.storeAfter
  | none => fallback

/-- (helper) Number the steps of a run from 1, in execution order. -/
def numbered : Nat → List Step → List (Nat × Step)
  | _, [] => []
  | n, s :: rest => (n, s) :: numbered (n + 1) rest

/-- (helper) `traced kids d Θ e rule H H' evs sr r`: the trace of a node
whose premises already contributed `kids`, whose own row runs `e` at depth
`d` under `rule` with binders `Θ`, taking the store from `H` to `H'`,
emitting `evs` and producing `sr`, and whose machine result is `r`. The own
row comes last, so the table is in execution order. -/
def traced (kids : List Step) (d : Nat) (Θ : List Ty) (R : Ty) (e : Expr) (rule : String)
    (H H' : Store) (evs : List Event) (sr : StepRes) (r : EvalRes) : Trace :=
  ⟨kids ++ [mkStep d Θ R e rule H H' evs sr], r⟩

/-- (helper) The same, with the own row shown in the machine's
administrative form. -/
def tracedAs (kids : List Step) (d : Nat) (Θ : List Ty) (R : Ty) (e : Expr)
    (rule shown : String) (H H' : Store) (evs : List Event) (sr : StepRes)
    (r : EvalRes) : Trace :=
  ⟨kids ++ [adminStep d Θ R e rule shown H H' evs sr], r⟩

/-- (helper) A node whose own rule never fired, with the reason spelled
out for that rule: the row's outgoing store is the last one the machine
reached, never a store from before the rows that already ran. -/
def didNotRun (kids : List Step) (d : Nat) (Θ : List Ty) (R : Ty) (e : Expr) (rule : String)
    (H : Store) (r : EvalRes) : Trace :=
  traced kids d Θ R e rule H (lastStore kids H) [] (StepRes.ofRes r) r

/-- (helper) A node that never ran because one of its premises trapped,
refused, unwound past it, or ran out of fuel, so it only passes that outcome
on. The row says so, because the same outcome then repeats up the spine of
the run. -/
def propagate (kids : List Step) (d : Nat) (Θ : List Ty) (R : Ty) (e : Expr) (rule : String)
    (H : Store) (r : EvalRes) : Trace :=
  didNotRun kids d Θ R e (rule ++ " — a premise did not complete") H r

/-- (helper) A node that refuses (§6's stuck states). Its outgoing store is
the last one the machine reached, for the same reason as `didNotRun`'s. -/
def refused (kids : List Step) (d : Nat) (Θ : List Ty) (R : Ty) (e : Expr) (rule : String)
    (H : Store) (w : Violation) : Trace :=
  traced kids d Θ R e rule H (lastStore kids H) [] (.refuse w (violationPremise w)) (.stuck w)

/-- (helper) The label for a `let` whose body did not complete: `(D-Let)`
has already fired — its own row is above — and `(D-EndScope)` never ran, so
the binding's drop never ran either (§6.7). An early `return` is the one
outcome where the drop still runs, because `run-all-scope-drops` (§6.9)
walks the frame's record instead; a trap (§6.12) abandons the configuration
and runs neither. -/
def scopeNeverClosed : String :=
  "(D-EndScope) §6.7 — the body did not complete, so this scope never closed " ++
  "(an early return runs the drop through the frame's scope record instead; a " ++
  "trap runs no drop at all)"

/-- (helper) The label for a call whose callee did not complete because it
**trapped**. No frame is popped there: §6.2's (Panic-Lift) carries `↯κ` out
of every evaluation context, the suspended caller's included, so
`run-all-scope-drops` never runs and the callee's open scopes are abandoned
with the configuration (§6.12). Saying "(D-Return-Value) (pop the frame)"
here would name a rule that did not fire. -/
def trapLiftsPastCall : String :=
  "(Panic-Lift) §6.2 — the callee trapped, so no frame is popped: §6.12 " ++
  "abandons the configuration and `run-all-scope-drops` never runs"

/-- (helper) An operator that met a wrong-shaped value. §5 excludes it and
`soundness` (§7) proves so; it is here because `eval` is total. -/
def confused (kids : List Step) (d : Nat) (Θ : List Ty) (R : Ty) (e : Expr) (rule : String)
    (H : Store) : Trace :=
  refused kids d Θ R e rule H .typeConfusion

/-- (helper) The §6.4 rule group a binary operator's row names. The float
rules are named by the operand type at the row (`binopDynRuleAt`); this is the
integer reading, which is what a row without float operands shows. -/
def binopDynRule : BinOp → String
  | .add | .sub | .mul => "(D-Arith) §6.4"
  | .div => "(D-Div) §6.4"
  | .rem => "§6.4's remainder arm, beside (D-Div)"
  | .bitAnd | .bitOr | .bitXor => "(D-Bit) §6.4"
  | .shl => "(D-Shl) §6.4"
  | .shr => "(D-Shr) §6.4"
  | .lt | .le | .gt | .ge => "ordering compare §6.4"
  | .totalCmp => "(D-Total-Cmp) §6.4"

/-- (helper) The same, read at the operand's *type*: §6.4 states the integer
rules over `n_T` and the float ones over `f_T`, and no operator rule's premises
are met by both, so a row's rule is decided by the value that reached it. -/
def binopDynRuleAt (op : BinOp) : Val → String
  | .float _ _ =>
      match op with
      | .add | .sub | .mul | .div => "(D-Float-Arith) §6.4"
      | .lt | .le | .gt | .ge => "(D-Float-Ord) §6.4"
      | .totalCmp => "(D-Total-Cmp) §6.4"
      | _ => binopDynRule op
  | _ => binopDynRule op

/-- (helper) The §6.4 rule a one-operand float intrinsic's row names. -/
def fintrinDynRule : FloatIntrin → String
  | .intToFloat _ => "(D-Int-To-Float) §6.4"
  | .floatToInt _ _ => "(D-Float-To-Int) §6.4"
  | .floatCast _ => "(D-Float-Cast) §6.4"
  | .roundOp _ => "(D-Float-Round) §6.4"

/-- (helper) The §6.4 rule a unary operator's row names. `neg` is
(D-Arith)'s unary case at an integer and `(D-Float-Neg)` at a float
(`unopDynRuleAt`); `not` and `bitnot` are total. -/
def unopDynRule : UnOp → String
  | .neg => "(D-Arith) §6.4, the unary case"
  | .not => "§6.4's `not` on bool"
  | .bitnot => "(D-Bit) §6.4, the complement"

/-- (helper) The same, read at the operand's type: `neg` is the one unary
operator §6.4 states twice, and the float case is total (`3.12:24`). -/
def unopDynRuleAt (op : UnOp) : Val → String
  | .float _ _ => match op with
    | .neg => "(D-Float-Neg) §6.4"
    | _ => unopDynRule op
  | _ => unopDynRule op

/-- The rows and outcome of a call's argument list (helper). -/
structure ArgsTrace where
  steps : List Step
  res : ArgsRes

/-- The instrumented mirror of `evalArgs`: the arguments' rows in
evaluation order (§6.2's left-to-right search through `g(v̄, …, E, …)`), and
the same outcome (§6.9). -/
def traceArgs (tev : Store → Expr → Trace) : Store → List Expr → ArgsTrace
  | H, [] => ⟨[], .ok H [] []⟩
  | H, e :: es =>
      let t := tev H e
      match t.res with
      | .ok H₁ v tr =>
          let ts := traceArgs tev H₁ es
          (match ts.res with
           | .ok H₂ vs tr₂ => ⟨t.steps ++ ts.steps, .ok H₂ (v :: vs) (tr ++ tr₂)⟩
           | .abort r => ⟨t.steps ++ ts.steps, .abort (r.withTrace tr)⟩)
      | r => ⟨t.steps, .abort r⟩

/-- The argument rows report the machine's own argument outcome (§6.9). -/
theorem traceArgs_res {tev : Store → Expr → Trace} {ev : Store → Expr → EvalRes}
    (h : ∀ H e, (tev H e).res = ev H e) :
    ∀ (H : Store) (es : List Expr), (traceArgs tev H es).res = evalArgs ev H es
  | _, [] => rfl
  | H, e :: es => by
      simp only [traceArgs, evalArgs, ← h H e]
      cases hr : (tev H e).res with
      | ok H₁ v tr =>
          simp only []
          rw [traceArgs_res h H₁ es]
          cases hr₂ : evalArgs ev H₁ es <;> rfl
      | _ => rfl

/-- The instrumented mirror of `eval` (§6): the same machine, recording one
row per evaluated node. `traceEval_res` proves the two agree on the final
result. `d` is the nesting depth, `Θ` the binder types in scope and `R` the
enclosing function's return type, all of which travel with `φ` so each row
prints its expression with the source's names. -/
def traceEval (M : FloatOps) (P : Program) :
    Nat → Nat → List Ty → Ty → Store → Frame → Expr → Trace
  | 0, d, Θ, R, H, _, e =>
      traced [] d Θ R e "out of fuel — the interpreter stopped early (ADR-0097)" H H []
        .exhausted .outOfFuel
  | _ + 1, d, Θ, R, H, _, .intLit w s n =>
      traced [] d Θ R (.intLit w s n) "literal §6.3" H H []
        (.value (.int w s n)) (.ok H (.int w s n) [])
  | _ + 1, d, Θ, R, H, _, .floatLit w l =>
      traced [] d Θ R (.floatLit w l) "literal §6.3 (3.12:9 rounds the decimal)" H H []
        (.value (.float w (M.ofLit w l.sig l.negExp l.e)))
        (.ok H (.float w (M.ofLit w l.sig l.negExp l.e)) [])
  | _ + 1, d, Θ, R, H, _, .boolLit b =>
      traced [] d Θ R (.boolLit b) "literal §6.3" H H [] (.value (.bool b)) (.ok H (.bool b) [])
  | _ + 1, d, Θ, R, H, _, .unitLit =>
      traced [] d Θ R .unitLit "literal §6.3" H H [] (.value .unit) (.ok H .unit [])
  | _ + 1, d, Θ, R, H, φ, .use i =>
      match φ.env[i]? with
      | none => refused [] d Θ R (.use i) "(D-Use-Copy)/(D-Use-Move) §6.3" H .unbound
      | some ℓ =>
        match H[ℓ]? with
        | none => refused [] d Θ R (.use i) "(D-Use-Copy)/(D-Use-Move) §6.3" H .unbound
        | some .dead => refused [] d Θ R (.use i) "(D-Use-Copy)/(D-Use-Move) §6.3" H .useAfterDrop
        | some .moved => refused [] d Θ R (.use i) "(D-Use-Copy)/(D-Use-Move) §6.3" H .useAfterMove
        | some (.full v) =>
            if v.mult P.structs = .copy then
              traced [] d Θ R (.use i) "(D-Use-Copy) §6.3" H H [] (.value v) (.ok H v [])
            else
              traced [] d Θ R (.use i) "(D-Use-Move) §6.3" H (H.set ℓ .moved) []
                (.value v) (.ok (H.set ℓ .moved) v [])
  | _ + 1, d, Θ, R, H, φ, .drop i =>
      match φ.env[i]? with
      | none => refused [] d Θ R (.drop i) "@drop §6.11" H .unbound
      | some ℓ =>
        match H[ℓ]? with
        | none => refused [] d Θ R (.drop i) "@drop §6.11" H .unbound
        | some .dead => refused [] d Θ R (.drop i) "@drop §6.11" H .useAfterDrop
        | some .moved => refused [] d Θ R (.drop i) "@drop §6.11" H .useAfterMove
        | some (.full v) =>
            (match dropCell P.structs ℓ v with
             | .error w => refused [] d Θ R (.drop i) "@drop §6.11" H w
             | .ok evs =>
                 if v.mult P.structs = .copy then
                   traced [] d Θ R (.drop i) "@drop §6.11 (Copy: no glue)" H H [] (.value .unit)
                     (.ok H .unit [])
                 else
                   traced [] d Θ R (.drop i) "@drop §6.11" H (H.set ℓ .moved)
                     evs (.value .unit) (.ok (H.set ℓ .moved) .unit evs))
  | fuel + 1, d, Θ, R, H, φ, .binop op e₁ e₂ =>
      let rule := binopDynRule op
      let t₁ := traceEval M P fuel (d + 1) Θ R H φ e₁
      match t₁.res with
      | .ok H₁ v₁ tr₁ =>
        let t₂ := traceEval M P fuel (d + 1) Θ R H₁ φ e₂
        (match t₂.res with
         | .ok H₂ v₂ tr₂ =>
             (match evalBinOp M op v₁ v₂ with
              | .val v =>
                  traced (t₁.steps ++ t₂.steps) d Θ R (.binop op e₁ e₂)
                    (binopDynRuleAt op v₁) H H₂ [] (.value v) (.ok H₂ v (tr₁ ++ tr₂))
              | .trap k =>
                  traced (t₁.steps ++ t₂.steps) d Θ R (.binop op e₁ e₂)
                    (binopDynRuleAt op v₁) H H₂ [] (.panicked k) (.panic k (tr₁ ++ tr₂))
              | .confused =>
                  confused (t₁.steps ++ t₂.steps) d Θ R (.binop op e₁ e₂)
                    (binopDynRuleAt op v₁) H)
         | r => propagate (t₁.steps ++ t₂.steps) d Θ R (.binop op e₁ e₂) rule H
                  (r.withTrace tr₁))
      | r => propagate t₁.steps d Θ R (.binop op e₁ e₂) rule H r
  | fuel + 1, d, Θ, R, H, φ, .unop op e =>
      let rule := unopDynRule op
      let t := traceEval M P fuel (d + 1) Θ R H φ e
      match t.res with
      | .ok H' v tr =>
          (match evalUnOp op v with
           | .val v' =>
               traced t.steps d Θ R (.unop op e) (unopDynRuleAt op v) H H' []
                 (.value v') (.ok H' v' tr)
           | .trap k =>
               traced t.steps d Θ R (.unop op e) rule H H' [] (.panicked k) (.panic k tr)
           | .confused => confused t.steps d Θ R (.unop op e) rule H)
      | r => propagate t.steps d Θ R (.unop op e) rule H r
  | fuel + 1, d, Θ, R, H, φ, .intCast w s e =>
      let t := traceEval M P fuel (d + 1) Θ R H φ e
      match t.res with
      | .ok H' v tr =>
          (match evalIntCast w s v with
           | .val v' =>
               traced t.steps d Θ R (.intCast w s e) "(D-Int-Cast) §6.4" H H' []
                 (.value v') (.ok H' v' tr)
           | .trap k =>
               traced t.steps d Θ R (.intCast w s e) "(D-Int-Cast-Trap) §6.4" H H' []
                 (.panicked k) (.panic k tr)
           | .confused => confused t.steps d Θ R (.intCast w s e) "(D-Int-Cast) §6.4" H)
      | r => propagate t.steps d Θ R (.intCast w s e) "(D-Int-Cast) §6.4" H r
  | fuel + 1, d, Θ, R, H, φ, .fintrin k e =>
      let rule := fintrinDynRule k
      let t := traceEval M P fuel (d + 1) Θ R H φ e
      match t.res with
      | .ok H' v tr =>
          (match evalFintrin M k v with
           | .val v' => traced t.steps d Θ R (.fintrin k e) rule H H' [] (.value v') (.ok H' v' tr)
           | .trap kk =>
               traced t.steps d Θ R (.fintrin k e) "(D-Float-To-Int-Trap) §6.4" H H' []
                 (.panicked kk) (.panic kk tr)
           | .confused => confused t.steps d Θ R (.fintrin k e) rule H)
      | r => propagate t.steps d Θ R (.fintrin k e) rule H r
  | _ + 1, d, Θ, R, H, _, .panic msg =>
      traced [] d Θ R (.panic msg) "(D-Panic) §6.12" H H [] (.panicked .user) (.panic .user [])
  | fuel + 1, d, Θ, R, H, φ, .dbg e =>
      let t := traceEval M P fuel (d + 1) Θ R H φ e
      match t.res with
      | .ok H' v tr =>
          traced t.steps d Θ R (.dbg e) "(Dbg) §5.8, the observable output of §6.12" H H'
            [.dbg v] (.value .unit) (.ok H' .unit (tr ++ [.dbg v]))
      | r => propagate t.steps d Θ R (.dbg e) "(Dbg) §5.8, the observable output of §6.12" H r
  | fuel + 1, d, Θ, R, H, φ, .mkStruct s args =>
      let ta := traceArgs (fun H' e' => traceEval M P fuel (d + 1) Θ R H' φ e') H args
      (match ta.res with
       | .abort r => didNotRun ta.steps d Θ R (.mkStruct s args) "(D-Struct) §6.5" H r
       | .ok H₁ vs tr =>
         match P.structs[s]? with
         | none => refused ta.steps d Θ R (.mkStruct s args) "(D-Struct) §6.5" H .unbound
         | some sd =>
             if sd.fields.length = vs.length then
               traced ta.steps d Θ R (.mkStruct s args) "(D-Struct) §6.5" H H₁ []
                 (.value (.struct s vs)) (.ok H₁ (.struct s vs) tr)
             else refused ta.steps d Θ R (.mkStruct s args) "(D-Struct) §6.5" H .typeConfusion)
  | fuel + 1, d, Θ, R, H, φ, .consume e =>
      let t := traceEval M P fuel (d + 1) Θ R H φ e
      match t.res with
      | .ok H' (.struct _ (.int w s n :: _)) tr =>
          traced t.steps d Θ R (.consume e) "§6.5 whole-value elimination" H H' []
            (.value (.int w s n)) (.ok H' (.int w s n) tr)
      | .ok _ _ _ => confused t.steps d Θ R (.consume e) "§6.5 whole-value elimination" H
      | r => propagate t.steps d Θ R (.consume e) "§6.5 whole-value elimination" H r
  | fuel + 1, d, Θ, R, H, φ, .letIn m e₁ e₂ =>
      let t₁ := traceEval M P fuel (d + 1) Θ R H φ e₁
      match t₁.res with
      | .ok H₁ v₁ tr₁ =>
          let bind := adminStep (d + 1) Θ R (.letIn m e₁ e₂) "(D-Let) §6.7 (mint the binding)"
            ("let " ++ (if m then "mut " else "") ++ Print.binderName Θ.length ++ " = " ++
              valLine v₁ ++ " at " ++ locName H₁.length)
            H₁ (H₁ ++ [.full v₁]) [] (.value v₁)
          let t₂ := traceEval M P fuel (d + 1) (valTy v₁ :: Θ) R (H₁ ++ [.full v₁])
            { env := H₁.length :: φ.env, scope := φ.scope ++ [H₁.length] } e₂
          (match t₂.res with
           | .ok H₂ v₂ tr₂ =>
             (match dropRetire P.structs H₂ H₁.length with
              | .error w =>
                  refused (t₁.steps ++ [bind] ++ t₂.steps) d Θ R (.letIn m e₁ e₂)
                    "(D-EndScope) §6.7 (retire the binding)" H₂ w
              | .ok (H₃, evs) =>
                  tracedAs (t₁.steps ++ [bind] ++ t₂.steps) d Θ R (.letIn m e₁ e₂)
                    "(D-EndScope) §6.7 (retire the binding)"
                    ("endscope(" ++ locsLine [H₁.length] ++ ")")
                    H₂ H₃ evs (.value v₂) (.ok H₃ v₂ (tr₁ ++ (tr₂ ++ evs))))
           | r =>
               didNotRun (t₁.steps ++ [bind] ++ t₂.steps) d Θ R (.letIn m e₁ e₂)
                 scopeNeverClosed H (r.withTrace tr₁))
      | r => propagate t₁.steps d Θ R (.letIn m e₁ e₂) "(D-Let) §6.7" H r
  | fuel + 1, d, Θ, R, H, φ, .assign i e =>
      let t := traceEval M P fuel (d + 1) Θ R H φ e
      match t.res with
      | .ok H₁ v tr =>
          (match φ.env[i]? with
           | none => refused t.steps d Θ R (.assign i e) "(D-Assign) §6.8" H .unbound
           | some ℓ =>
             match H₁[ℓ]? with
             | none => refused t.steps d Θ R (.assign i e) "(D-Assign) §6.8" H .unbound
             | some .dead => refused t.steps d Θ R (.assign i e) "(D-Assign) §6.8" H .useAfterDrop
             | some .moved =>
                 traced t.steps d Θ R (.assign i e) "(D-Assign) §6.8 (reinitialization, 3.8:55)"
                   H (H₁.set ℓ (.full v)) [] (.value .unit) (.ok (H₁.set ℓ (.full v)) .unit tr)
             | some (.full vOld) =>
                 if vOld.mult P.structs = .linear then
                   refused t.steps d Θ R (.assign i e) "(D-Assign) §6.8" H .linearOverwrite
                 else
                   match dropCell P.structs ℓ vOld with
                   | .error w => refused t.steps d Θ R (.assign i e) "(D-Assign) §6.8" H w
                   | .ok evs =>
                       traced t.steps d Θ R (.assign i e) "(D-Assign) §6.8 (overwrite-drop)"
                         H (H₁.set ℓ (.full v)) evs (.value .unit)
                         (.ok (H₁.set ℓ (.full v)) .unit (tr ++ evs)))
      | r => propagate t.steps d Θ R (.assign i e) "(D-Assign) §6.8" H r
  | fuel + 1, d, Θ, R, H, φ, .seq e₁ e₂ =>
      let t₁ := traceEval M P fuel (d + 1) Θ R H φ e₁
      match t₁.res with
      | .ok H₁ v₁ tr₁ =>
          (match v₁.mult P.structs with
           | .linear => refused t₁.steps d Θ R (.seq e₁ e₂) "(D-Seq) §6.7" H .linearDiscard
           | .affine =>
               match dropValue P.structs v₁ with
               | .error w => refused t₁.steps d Θ R (.seq e₁ e₂) "(D-Seq) §6.7" H w
               | .ok evs =>
               let discard := adminStep (d + 1) Θ R (.seq e₁ e₂) "(D-Seq) §6.7 (drop the temporary)"
                 ("drop(" ++ valLine v₁ ++ ")") H₁ H₁ (.dropTemp v₁ :: evs) (.value .unit)
               let t₂ := traceEval M P fuel (d + 1) Θ R H₁ φ e₂
               traced (t₁.steps ++ [discard] ++ t₂.steps) d Θ R (.seq e₁ e₂) "(D-Seq) §6.7"
                 H (lastStore (t₁.steps ++ [discard] ++ t₂.steps) H₁) []
                 (StepRes.ofRes t₂.res)
                 ((t₂.res.withTrace (.dropTemp v₁ :: evs)).withTrace tr₁)
           | .copy =>
               let t₂ := traceEval M P fuel (d + 1) Θ R H₁ φ e₂
               traced (t₁.steps ++ t₂.steps) d Θ R (.seq e₁ e₂) "(D-Seq) §6.7"
                 H (lastStore (t₁.steps ++ t₂.steps) H₁) [] (StepRes.ofRes t₂.res)
                 (t₂.res.withTrace tr₁))
      | r => propagate t₁.steps d Θ R (.seq e₁ e₂) "(D-Seq) §6.7" H r
  | fuel + 1, d, Θ, R, H, φ, .ite c e₁ e₂ =>
      let t₀ := traceEval M P fuel (d + 1) Θ R H φ c
      match t₀.res with
      | .ok H₀ (.bool b) tr₀ =>
          if b then
            let t₁ := traceEval M P fuel (d + 1) Θ R H₀ φ e₁
            traced (t₀.steps ++ t₁.steps) d Θ R (.ite c e₁ e₂) "(D-If-T) §6.6"
              H (lastStore (t₀.steps ++ t₁.steps) H₀) [] (StepRes.ofRes t₁.res)
              (t₁.res.withTrace tr₀)
          else
            let t₂ := traceEval M P fuel (d + 1) Θ R H₀ φ e₂
            traced (t₀.steps ++ t₂.steps) d Θ R (.ite c e₁ e₂) "(D-If-F) §6.6"
              H (lastStore (t₀.steps ++ t₂.steps) H₀) [] (StepRes.ofRes t₂.res)
              (t₂.res.withTrace tr₀)
      | .ok _ _ _ => confused t₀.steps d Θ R (.ite c e₁ e₂) "(D-If-T)/(D-If-F) §6.6" H
      | r => propagate t₀.steps d Θ R (.ite c e₁ e₂) "(D-If-T)/(D-If-F) §6.6" H r
  | fuel + 1, d, Θ, R, H, φ, .ret e =>
      let t := traceEval M P fuel (d + 1) Θ R H φ e
      match t.res with
      | .ok H₁ v tr =>
          (match runAllScopeDrops P.structs H₁ φ with
           | .error w =>
               refused t.steps d Θ R (.ret e) "(D-Return) §6.9 (unwind the frame)" H₁ w
           | .ok (H₂, evs) =>
               tracedAs t.steps d Θ R (.ret e) "(D-Return) §6.9 (unwind the frame)"
                 ("run-all-scope-drops(" ++ locsLine φ.scope.reverse ++ ")")
                 H₁ H₂ evs (.unwound v) (.returned H₂ v (tr ++ evs)))
      | r => propagate t.steps d Θ R (.ret e) "(D-Return) §6.9" H r
  | fuel + 1, d, Θ, R, H, φ, .call f args =>
      let ta := traceArgs (fun H' e' => traceEval M P fuel (d + 1) Θ R H' φ e') H args
      match ta.res with
      | .abort r => didNotRun ta.steps d Θ R (.call f args) "(D-Call) §6.9" H r
      | .ok H₁ vs tr =>
        match P.fns[f]? with
        | none => refused ta.steps d Θ R (.call f args) "(D-Call) §6.9" H .unbound
        | some fd =>
          if fd.params.length = vs.length then
            let minted := mintParams H₁ vs
            let φg : Frame := { env := minted.2.reverse, scope := minted.2 }
            let push := adminStep (d + 1) Θ R (.call f args) "(D-Call) §6.9 (push the frame)"
              (fnHeader f fd ++ "  with " ++ locsLine minted.2)
              H₁ minted.1 [] (.value .unit)
            let tb := traceEval M P fuel (d + 2) (Print.bodyBinders fd) fd.ret minted.1 φg fd.body
            (match tb.res with
             | .ok H₃ v tr₃ =>
               (match runAllScopeDrops P.structs H₃ φg with
                | .error w =>
                    refused (ta.steps ++ [push] ++ tb.steps) d Θ R (.call f args)
                      "(D-Return-Value) §6.9 (pop the frame)" H₃ w
                | .ok (H₄, evs) =>
                    tracedAs (ta.steps ++ [push] ++ tb.steps) d Θ R (.call f args)
                      "(D-Return-Value) §6.9 (pop the frame)"
                      ("run-all-scope-drops(" ++ locsLine minted.2.reverse ++ ")")
                      H₃ H₄ evs (.value v) (.ok H₄ v (tr ++ (tr₃ ++ evs))))
             | .returned H₃ v tr₃ =>
                 tracedAs (ta.steps ++ [push] ++ tb.steps) d Θ R (.call f args)
                   "(D-Return)/(D-Return-Main) §6.9 (the callee's return is the call's value)"
                   ("absorb " ++ valLine v)
                   H₃ H₃ [] (.value v) (.ok H₃ v (tr ++ tr₃))
             | r =>
                 didNotRun (ta.steps ++ [push] ++ tb.steps) d Θ R (.call f args)
                   (match r with
                    | .panic _ _ => trapLiftsPastCall
                    | _ => "(D-Return-Value) §6.9 (pop the frame)")
                   H (r.withTrace tr))
          else refused ta.steps d Θ R (.call f args) "(D-Call) §6.9" H .typeConfusion

/-- **The trace is the machine.** Projecting a run to its final result
reproduces `eval fuel P H φ e` exactly, so a rendered step table can never
report an outcome — a value, an unwinding `return`, a §6.12 trap, a refusal,
or exhausted fuel — the interpreter does not produce. -/
theorem traceEval_res (M : FloatOps) {P : Program} : ∀ (fuel : Nat) (d : Nat) (Θ : List Ty)
    (R : Ty) (H : Store) (φ : Frame) (e : Expr),
    (traceEval M P fuel d Θ R H φ e).res = eval M fuel P H φ e := by
  intro fuel
  induction fuel with
  | zero => intro d Θ R H φ e; rfl
  | succ fuel ih =>
      intro d Θ R H φ e
      cases e with
      | intLit n => rfl
      | floatLit w l => rfl
      | boolLit b => rfl
      | unitLit => rfl
      | use i =>
          simp only [traceEval, eval]
          (repeat' split) <;> first | rfl | (simp_all [traced] <;> grind)
      | drop i =>
          simp only [traceEval, eval]
          (repeat' split) <;> first | rfl | (simp_all [traced, refused] <;> grind)
      | binop op e₁ e₂ =>
          simp only [traceEval, eval, EvalRes.andThen, ih]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, didNotRun, propagate, confused, refused,
              OpRes.toRes, EvalRes.withTrace] <;> grind)
      | unop op e₁ =>
          simp only [traceEval, eval, EvalRes.andThen, ih]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, confused, refused,
              OpRes.toRes, EvalRes.withTrace] <;> grind)
      | fintrin k e₁ =>
          simp only [traceEval, eval, EvalRes.andThen, ih]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, confused, refused,
              OpRes.toRes, EvalRes.withTrace] <;> grind)
      | intCast w sg e₁ =>
          simp only [traceEval, eval, EvalRes.andThen, ih]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, confused, refused,
              OpRes.toRes, EvalRes.withTrace] <;> grind)
      | panic msg => rfl
      | dbg e₁ =>
          simp only [traceEval, eval, EvalRes.andThen, ih]
          (repeat' split) <;>
            first | rfl | (simp_all [traced,
              EvalRes.withTrace] <;> grind)
      | mkStruct s' args =>
          simp only [traceEval, eval,
            traceArgs_res (ev := fun H' e' => eval M fuel P H' φ e') (fun H' e' => ih _ _ _ _ _ e')]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, didNotRun, EvalRes.withTrace] <;> grind)
      | consume e₁ =>
          simp only [traceEval, eval, EvalRes.andThen, ih]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, confused, refused,
              EvalRes.withTrace] <;> grind)
      | letIn m e₁ e₂ =>
          simp only [traceEval, eval, EvalRes.andThen, ih]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, tracedAs, didNotRun, refused,
              EvalRes.withTrace] <;> grind)
      | assign i e₁ =>
          simp only [traceEval, eval, EvalRes.andThen, ih]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, refused,
              EvalRes.withTrace] <;> grind)
      | seq e₁ e₂ =>
          simp only [traceEval, eval, EvalRes.andThen, ih]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, refused,
              EvalRes.withTrace] <;> grind)
      | ite c e₁ e₂ =>
          simp only [traceEval, eval, EvalRes.andThen, ih]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, confused, refused,
              EvalRes.withTrace] <;> grind)
      | ret e₁ =>
          simp only [traceEval, eval, EvalRes.andThen, ih]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, tracedAs, refused,
              EvalRes.withTrace] <;> grind)
      | call f args =>
          simp only [traceEval, eval,
            traceArgs_res (ev := fun H' e' => eval M fuel P H' φ e') (fun H' e' => ih _ _ _ _ _ e')]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, tracedAs, didNotRun, refused, EvalRes.absorb,
              EvalRes.withTrace] <;> grind)

/-! ## A whole program

A page explains a *program*: one derivation per function body (§5.8's (Fn)
checks each under its own entry context) and one run, entered at index `0`
exactly as `Dynamics.run` enters it. -/

/-- The derivation of every function body, with the function's index and
definition, in program order — one per (Fn) §5.8 obligation, each checked
from that function's own entry context. -/
def programDerivs (P : Program) : Nat → List FnDef → List (Nat × FnDef × Deriv)
  | _, [] => []
  | i, fd :: rest =>
      (i, fd, explain P fd.ret (fnCtx fd) fd.body) :: programDerivs P (i + 1) rest

/-- The run of a whole program: the entry call, from the empty store and the
empty frame (§6.12's top-level result). -/
def runTrace (M : FloatOps) (P : Program) (fuel : Nat) : Trace :=
  let T := match P.fns[0]? with
    | some fd => fd.ret
    | none => .int .w64 .signed
  traceEval M P fuel 0 [] T [] { env := [], scope := [] } (.call 0 [])

/-- **The program's run is the program's outcome** (§6.12). -/
theorem runTrace_res (M : FloatOps) (P : Program) (fuel : Nat) :
    (runTrace M P fuel).res = run M P fuel :=
  traceEval_res M fuel 0 [] _ [] _ _

end Explain
end RueCore
