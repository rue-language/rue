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
(D-Struct) §6.5 — and so is a projection, which is a use of a place and
carries (Use-Copy)/(Use-Move) §5.1, or, where its path has a
declared-`linear` proper prefix, (Use-Declared-Linear-Destructure) §5.1 with
(D-Use-Declared-Linear) §6.3 under it. Its rejections name that rule's own
premises: `fully-owned(Σ, d)` read at the **consumed** place, and
`¬ linear-residue(S, π_s)`, which is what the compiler reports as E0474. A
node whose rule the fragment restricts rather than models carries the
restriction in the rejection text instead of a label it cannot claim.

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

/-- (helper) The dynamic tail `[e₁]π₁…[eₖ]πₖ` of a place below a dynamic
index, on one line, from the rendered indices and the type of the part
already printed (`Print.pathSuffix` spells each constant path). -/
def dynTailLine (D : Decls) : Option Ty → List String → List (List Nat) → String
  | T, s :: ss, π :: πs =>
      let E := match T with
        | some (.array E _) => some E
        | _ => none
      "[" ++ s ++ "]" ++ Print.pathSuffix D E π ++
        dynTailLine D (E.bind fun E => E.atPath D π) ss πs
  | _, _, _ => ""

/-- (helper) A one-line rendering of a core expression, in the Rue surface
syntax of `Print.expr` and with its `v<depth>` binder names, but with the
block forms (`let`, assignment, sequencing, `if`) folded onto one line so a
derivation node or a trace row stays one row. -/
partial def exprLine (P : Program) (R : Ty) : List Ty → Expr → String
  | _, .intLit _ _ n => if n < 0 then "(" ++ toString n ++ ")" else toString n
  | _, .floatLit _ l => l.spell
  | _, .boolLit b => if b then "true" else "false"
  | _, .unitLit => "()"
  | Γ, .use pl => Print.place Γ pl
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
  | Γ, .mkEnum e k args =>
      Print.tyName (.enum e) ++ "." ++ Print.variantName k ++
        (if args.isEmpty then ""
         else "(" ++ String.intercalate ", " (args.map (fun a => exprLine P R Γ a)) ++ ")")
  | Γ, .«match» scrut arms =>
      -- One line, with each arm's payload binders named the way `Print` names
      -- them, so a derivation node reads like the printed program.
      let variants := match Print.tyOf P R Γ scrut with
        | some (.enum e) => ((P.decls.enums[e]?).map EnumDecl.variants).getD []
        | _ => []
      let name := match Print.tyOf P R Γ scrut with
        | some (.enum e) => Print.tyName (.enum e)
        | _ => "E0"
      "match " ++ exprLine P R Γ scrut ++ " { " ++
        String.intercalate ", "
          ((arms.zipIdx).map (fun (a, k) =>
            let Ts := ((variants[k]?).getD [])
            let binders := (List.range Ts.length).map (fun j => Print.binderName (Γ.length + j))
            name ++ "." ++ Print.variantName k ++
              (if binders.isEmpty then "" else "(" ++ String.intercalate ", " binders ++ ")") ++
              " => " ++ exprLine P R (Ts.reverse ++ Γ) a)) ++ " }"
  | Γ, .mkArray _ args =>
      "[" ++ String.intercalate ", " (args.map (fun a => exprLine P R Γ a)) ++ "]"
  | Γ, .repeatArray _ e n => "[" ++ exprLine P R Γ e ++ "; " ++ toString n ++ "]"
  | Γ, .indexRead pl idx πs =>
      Print.place Γ pl ++
        dynTailLine P.decls (Print.placeTy P Γ pl) (idx.map (exprLine P R Γ)) πs
  | Γ, .indexWrite pl idx πs e =>
      "{ " ++ Print.place Γ pl ++
        dynTailLine P.decls (Print.placeTy P Γ pl) (idx.map (exprLine P R Γ)) πs ++ " = " ++
        exprLine P R Γ e ++ "; }"
  | Γ, .indexDrop pl idx πs =>
      "@drop(" ++ Print.place Γ pl ++
        dynTailLine P.decls (Print.placeTy P Γ pl) (idx.map (exprLine P R Γ)) πs ++ ")"
  | Γ, .drop pl => "@drop(" ++ Print.place Γ pl ++ ")"
  | Γ, .letIn m e₁ e₂ =>
      let T₁ := (Print.tyOf P R Γ e₁).getD (.int .w64 .signed)
      "{ let " ++ (if m then "mut " else "") ++ Print.binderName Γ.length ++ ": " ++
        Print.tyName T₁ ++ " = " ++ exprLine P R Γ e₁ ++ "; " ++
        exprLine P R (T₁ :: Γ) e₂ ++ " }"
  | Γ, .assign pl e => "{ " ++ Print.place Γ pl ++ " = " ++ exprLine P R Γ e ++ "; }"
  | Γ, .seq e₁ e₂ => "{ " ++ exprLine P R Γ e₁ ++ "; " ++ exprLine P R Γ e₂ ++ " }"
  | Γ, .ite c e₁ e₂ =>
      "if " ++ exprLine P R Γ c ++ " { " ++ exprLine P R Γ e₁ ++ " } else { " ++
        exprLine P R Γ e₂ ++ " }"
  | Γ, .call f args =>
      Print.fnName f ++ "(" ++
        String.intercalate ", " (args.map (fun a => exprLine P R Γ a)) ++ ")"
  | Γ, .ret e => "return " ++ exprLine P R Γ e
  | _, .brk => "break"
  | Γ, .loop e => "loop { " ++ exprLine P R Γ e ++ " }"

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
  | .struct s _ _ => .struct s
  | .enum e _ _ _ => .enum e
  | .array T _ vs => .array T vs.length

/-- (helper) A value identity, as the trace shows it: `#i` after the
aggregate it names (`introVal`, `Dynamics.lean`). A copy of a `Copy` value
carries its original's identity; `no_double_free` (`Trace.lean`) counts only
the non-`Copy` ones, so a repeated identity on a `Copy` value is expected, and
on anything else it would be the double free the theorem rules out. -/
def idTag (i : Nat) : String := "#" ++ toString i

/-- (helper) A value, as §6.1 writes it: a scalar as itself, a struct value
as `{ v1, …, vk }_S` with its declaration's name, and every aggregate followed
by its identity (`idTag`). -/
partial def valLine : Val → String
  | .int _ _ n => toString n
  | .float w f => f.render w
  | .bool b => if b then "true" else "false"
  | .unit => "()"
  | .struct s i vs =>
      Print.tyName (.struct s) ++ " { " ++
        String.intercalate ", " (vs.map valLine) ++ " }" ++ idTag i
  | .enum e k i vs =>
      -- §6.1's `Kj⟨ v1, …, va ⟩`: the tag, and the payload when there is one.
      Print.tyName (.enum e) ++ "." ++ Print.variantName k ++
        (if vs.isEmpty then "⟨⟩" else "⟨" ++ String.intercalate ", " (vs.map valLine) ++ "⟩") ++
        idTag i
  | .array _ i vs => "[" ++ String.intercalate ", " (vs.map valLine) ++ "]" ++ idTag i

/-- (helper) Cell contents (§6.1's `c ::= v | ⊘`), as a tree: a `⊘` may sit
at any node after a partial move (§4.2). -/
partial def contentsLine : Contents → String
  | .hole => "⊘"
  | .int _ _ n => toString n
  | .float w f => f.render w
  | .bool b => if b then "true" else "false"
  | .unit => "()"
  | .struct s i cs =>
      Print.tyName (.struct s) ++ " { " ++
        String.intercalate ", " (cs.map contentsLine) ++ " }" ++ idTag i
  | .enum e k i cs =>
      Print.tyName (.enum e) ++ "." ++ Print.variantName k ++
        (if cs.isEmpty then "⟨⟩" else "⟨" ++ String.intercalate ", " (cs.map contentsLine) ++ "⟩") ++
        idTag i
  | .array _ i cs => "[" ++ String.intercalate ", " (cs.map contentsLine) ++ "]" ++ idTag i

/-- (helper) A store cell: its contents, or `†` — a retired binding, or the
slot a minted value identity reserved (`introVal`), which never held a value. -/
def cellLine : Cell → String
  | .full c => contentsLine c
  | .dead => "†"

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

mutual
/-- (helper) A `Σ` state, spelled as §5 spells it: `Owned` and `MovedOut` at
a whole path, and — for a path a partial move has opened up — `Owned` at the
node itself with its fields' own states beside it, named by the declaration
slots `Print.fieldName` gives them. A slot no partial move touched is `Owned`
and is not listed. -/
partial def ownStateName : OwnSt → String
  | .owned => "Owned"
  | .movedOut => "MovedOut"
  | .fields ts =>
      "Owned{ " ++ String.intercalate ", " (ownStateFields 0 ts) ++ " }"

/-- (helper) One entry per recorded field slot, `x<j>: state`. -/
partial def ownStateFields : Nat → List OwnSt → List String
  | _, [] => []
  | j, t :: ts => (Print.fieldName j ++ ": " ++ ownStateName t) :: ownStateFields (j + 1) ts
end

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

/-- (helper) A checked type: an ordinary type's name, or `never` (§5.7). -/
def cTyName : CTy → String
  | .never => "never"
  | .ty T => Print.tyName T

/-- (helper) §5.3's outgoing result `Ω`: the normal state, or `⊥` when there
is none, followed by the recorded `⟨break, Σ⟩` deliveries when there are
any. -/
def outLine (Ω : Out) : String :=
  let norm := match Ω.norm with
    | some Γ => ctxLine Γ
    | none => "⊥"
  if Ω.brk.isEmpty then norm
  else norm ++ "; Δ = {" ++
    String.intercalate ", " (Ω.brk.map fun Γ => "⟨break, " ++ ctxLine Γ ++ "⟩") ++ "}"

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
  | .drop ℓ c => "drop " ++ locName ℓ ++ " = " ++ contentsLine c
  | .dropTemp v => "drop temporary " ++ valLine v
  | .dtor s c => "run drop fn " ++ Print.tyName (.struct s) ++ "(" ++ contentsLine c ++ ")"
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

/-- (Lit) §5.8 premise at a float literal: `3.12:10` makes a float literal whose value rounds to
an infinity at its width a **compile-time** rejection (`E0206`). `3.12:9`
rounds, so an inexact decimal is fine and so is an underflow to zero; only
overflow is refused. -/
def floatLitInfinite (w : FloatWidth) : String :=
  "the float literal's value rounds to an infinity at " ++ Print.tyName (.float w) ++
  ", which 3.12:10 requires be rejected at compile time (E0206) — the threshold is " ++
  "`FloatWidth.overflowNum`, half an ulp above the width's largest finite value; " ++
  "3.12:9 rounds every smaller decimal, an underflow to zero included"

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

/-- `Owned-Base` (§5.1): the base of a projection must currently own its
storage, so a path under a `MovedOut` prefix is one Σ does not have
(`3.8:53`). -/
def pathUnderMoved : String :=
  "a proper prefix of the path is MovedOut, so Σ has no state for the place at all " ++
  "((Owned-Base) §5.1, `Σ(p) = Owned` for the base; 3.8:53; the compiler reports E0205)"

/-- `Γ ⊢ p : T` (§5 preamble): the path must select a declared field at every
step. Elaboration resolves a field name to its slot (`3.6:15`), so no
elaborated program reaches this. -/
def pathNotField : String :=
  "a step of the path is not a declared field of the type it is taken from " ++
  "((Γ ⊢ p : T), §5 preamble; elaboration resolves a field name to its declaration " ++
  "slot, 3.6:15, so no elaborated program reaches this premise)"

/-- (Use-Move) §5.1's `fully-owned(Σ, p)` premise (`3.8:26`). -/
def usePartiallyMoved (T : Ty) : String :=
  "the place is only partially owned: a path under it is MovedOut, and (Use-Move) " ++
  "hands the whole value of type " ++ Print.tyName T ++ " to a new owner " ++
  "((Use-Move) premise `fully-owned(Σ, p)`, §5.1; 3.8:5/24/26/53; the compiler " ++
  "reports E0205 \"use of partially moved value\")"

/-- (Use-Move) §5.1's and (@Drop) §5.3's `3.9:34` premise (E0456). -/
def moveUnderDtor : String :=
  "a proper prefix of the path has a type that declares a destructor, so the field " ++
  "may not be moved out of it — the destructor runs on the whole value and would " ++
  "observe the hole ((Use-Move)/(@Drop) premise, §5.1, §5.3; 3.9:34; the compiler " ++
  "reports E0456)"

/-- (Use-Declared-Linear-Destructure) §5.1's `fully-owned(Σ, d)` premise,
which the rule reads at the **consumed place** `d` — the smallest enclosing
declared-`linear` place — rather than at the projected leaf (`3.8:26`).

Two states fail it and both are reachable. `Σ(d) = MovedOut` is the second
read of a place its own first destructure consumed, and the compiler reports
E0205 there. A hole *strictly under* `d` needs an inner declared-`linear`
place `d'`, because that is the only thing a destructure writes `⊘` at below
`d`; a later access at `d` then also retains `d'`, so the compiler reaches the
program through its residue check and reports E0474 on that field instead.
The order of `explain`'s tests, not a difference of opinion, is what decides
which premise is named. -/
def destructurePartiallyMoved (Td : Ty) : String :=
  "the smallest enclosing declared-`linear` place, of type " ++ Print.tyName Td ++
  ", is not fully owned: the place itself or a path under it is MovedOut, and the " ++
  "destructure hands the selected leaf to a new owner while destroying the rest " ++
  "((Use-Declared-Linear-Destructure) premise `fully-owned(Σ, d)`, §5.1; 3.8:26; " ++
  "the compiler reports E0205 where the place itself was consumed, and E0474 on the " ++
  "retained inner declared-`linear` place where a path under it was)"

/-- (Use-Declared-Linear-Destructure) §5.1's `¬ linear-residue(S, π_s)`
premise: the destructure destroys its residue at once, so a retained place of
linear type would be dropped unconsumed (`3.8:60`, E0474). -/
def residueCarriesLinear (Td : Ty) : String :=
  "the residue of the destructure carries a linear value: a field access that " ++
  "destructures " ++ Print.tyName Td ++ " extracts the selected leaf and destroys " ++
  "every retained place immediately, so a linear one would be dropped without ever " ++
  "being consumed ((Use-Declared-Linear-Destructure) premise " ++
  "`¬ linear-residue(S, π_s)`, §5.1; 3.8:60; the compiler reports E0474)"

/-- §4.2's "element moves only at the root" (`3.8:68`, E0904): a move or a
`@drop` may take an element out of the root binding's array and of no array
reached through a further step (`rootIdxOnly`, `Syntax.lean`). -/
def moveAtIndex : String :=
  "the move takes a step at an array that is not the root binding — a nested index " ++
  "(`a[c][c']`) or an array reached through a field (`h.a[c]`) — and `3.8:68` admits " ++
  "an element move only \"applied directly to the root binding\"; the compiler reports " ++
  "E0904 \"cannot move out of indexed position\""

/-- `3.8:72`/`7.1:46` (E0480): an assignment whose destination steps into an
array demands the whole array (`assignArrayOk`, `Statics.lean`). -/
def assignIntoPartialArray : String :=
  "the destination writes into an array one of whose elements has been moved out, and " ++
  "`3.8:72` forbids that \"to an element, or through an element\" alike — an element " ++
  "write does not reinstate per-element ownership (`7.1:46`), so the whole array must " ++
  "be reinitialized instead; the compiler reports E0480"

/-- `7.1:38`: the element type of a repeat literal must be `Copy` (E0905). -/
def repeatNotCopy (T : Ty) : String :=
  "the repeat form materializes `n` copies of one value, which is only well defined " ++
  "at a `Copy` element type, and " ++ Print.tyName T ++ " is not one (7.1:38; the " ++
  "compiler reports E0905)"

/-- (Array-Intro) §5.8: every element shares the one element type (`3.5:3`,
`7.1:3`). -/
def elemTypeMismatch (T elem : Ty) : String :=
  "an element has type " ++ Print.tyName T ++ " where the array's element type is " ++
  Print.tyName elem ++ " — all elements share one type ((Array-Intro) §5.8; 3.5:3, 7.1:3)"

/-- `4.11:3`: the base of an index expression must have an array type. -/
def notAnArray (T : Ty) : String :=
  "the indexed place has type " ++ Print.tyName T ++ ", which is not an array type " ++
  "(4.11:3)"

/-- `4.11:4`: an index expression must have an integer type. -/
def indexNotInt (T : Ty) : String :=
  "the index has type " ++ Print.tyName T ++ ", and an index must be an integer " ++
  "(4.11:4)"

/-- §4.2's `Untrackable(OrdinaryDynamic)` plan has a successful rule only at a
`Copy` leaf type: (Use-Untrackable-Dynamic-Copy) §5.1, and "there is no
successful static rule … when `class(T) ∈ {Affine,Linear}`" (E0904). The leaf
is the element itself or a place below it (`a[i]`, `a[i].x0`). This is about a
dynamic-index **read**, which is a use; the write's own refusal is
`linearOverwrite` below, because an assignment destination is not one. -/
def elementNotCopy (T : Ty) : String :=
  "the type " ++ Print.tyName T ++ " read below the dynamic index is not `Copy`, and a " ++
  "dynamic index has no rule there: the compiler cannot know which element a runtime " ++
  "index moved ((Use-Untrackable-Dynamic-Copy) §5.1; 3.8:70, 7.1:28; the compiler " ++
  "reports E0904)"

/-- The core form's own shape: a place below a dynamic index has one or more
dynamic steps, each paired with the constant path after it (§2's `p [ e ]`
production, iterated). Elaboration never builds any other, so this is an
ill-formed core term rather than a program the compiler would see. -/
def dynShape : String :=
  "the dynamic place has no dynamic step, or its index list and its constant-path " ++
  "list do not pair up one to one (an ill-formed core term; elaboration never builds it)"

/-- `3.8:70`/`7.1:45`: a non-constant index may not be used while an element is
moved out, because the compiler cannot know whether it denotes a moved-out
one. -/
def indexPartiallyMoved : String :=
  "a path under the array is MovedOut, so a non-constant index may denote a " ++
  "moved-out element — which the compiler cannot decide (3.8:70, 7.1:45)"

/-- §4.2's `Untrackable(DeclaredLinearDynamic)`, which the calculus declares
ill-formed: a **dynamic** index *read* at a place with a proper prefix of
declared-`linear` struct type, above the dynamic index or below it.
(Use-Untrackable-Dynamic-Copy) §5.1 carries `declaredPrefix … = none` and
`Ty.dynNoDeclared` to keep it without an instance. A dynamic-index *write* is
not a use and carries no plan premise (`Typed.indexWrite`). -/
def declaredLinearPrefix : String :=
  "a proper prefix of the place is a struct declared `linear`, so a dynamic index " ++
  "read here would be §4.2's `Untrackable(DeclaredLinearDynamic)` plan — which the " ++
  "calculus declares ill-formed, because no static rule can know whether the runtime " ++
  "index names a place the destructure of 3.8:33 already consumed (3.8:33, 3.8:70; " ++
  "the compiler reports E0904 on the read)"

/-- (@Drop) §5.3's last premise: a partially moved place may not be dropped
whole while a linear sub-place under it is still owned. -/
def dropStrandsLinear : String :=
  "a path under the place is MovedOut while a linear sub-place below it is still " ++
  "Owned, so the drop would destroy a linear value the program never consumed " ++
  "((@Drop) §5.3's residual side condition; 3.8:32; the compiler reports E0406)"

/-- §5.6 scope exit, the residual-linear leak check; prose `3.8:32`. -/
def letLeak (T : Ty) : String :=
  "the residual state of the `let` binder still carries a linear value at type " ++
  Print.tyName T ++ " — a linear value reached end of scope unconsumed " ++
  "(§5.6's `residual-linear(Σ, x, T)`, read on the residue after any partial move; " ++
  "3.8:32; the compiler reports E0406)"

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
prose `3.8:77` (the RUE-387 premise), keyed on the destination's type. At a
dynamic index the destination's type is the **leaf** type, and a place under a
runtime index can never be proven `MovedOut`, so this is the whole of what
(Assign)'s last premise refuses there. -/
def linearOverwrite (T : Ty) : String :=
  "overwrite of a live linear value: the place is not MovedOut after the " ++
  "right-hand side and its type " ++ Print.tyName T ++ " carries a linear value " ++
  "((Assign) premise `Σ1(p) = MovedOut ∨ ¬carries_linear(T)`, §5.2, read on the " ++
  "destination's type; 3.8:77; the compiler reports E0493)"

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

/-- (If) premise `Σ' = join(Σ1, Σ2)` (§5.5) and (Match) §5.5's n-way join,
which is that same join folded over the arms (`Ctx.joinAll`); prose `3.8:50`.
`who` names the entry the arms disagree on, when one can be named; at a
`match` it names them by arm index, since there is no then- or else-arm and
there may be more than two. -/
def joinConflict (atMatch : Bool) (who : Option String) : String :=
  (if atMatch then "the arms" else "the two arms") ++
  " disagree on a linear-carrying entry" ++
  (match who with | some w => " — " ++ w | none => "") ++
  ", so a linear value is consumed on only some paths (" ++
  (if atMatch then
     "(Match) premise `Σ' = joinAll(Σ1, …, Σn)`, §5.5 — (If)'s binary join " ++
     "`Σ' = join(Σ1, Σ2)`, §5.5, folded over the arms"
   else "(If) premise `Σ' = join(Σ1, Σ2)`, §5.5") ++
  "; 3.8:50; the compiler reports E0443)"

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

/-- (Enum-Intro)/(Match) §5.5's first premise, `E = enum { K1(T̄1), …, Kn(T̄n) }`:
the program has no enum declaration at this index. Elaboration resolves a type's
name before the core (§2), so no elaborated program reaches this. -/
def unknownEnum : String :=
  "the program has no enum declaration at this index ((Enum-Intro)/(Match) premise " ++
  "`E = enum { K1(T̄1), …, Kn(T̄n) }`, §5.5; elaboration resolves a type name before " ++
  "the core, §2, so no elaborated program reaches this premise)"

/-- (Enum-Intro) §5.5's premise that the tag names a variant of `E`
(`6.3:16`; the compiler reports E0420). -/
def unknownVariant : String :=
  "the enum has no variant at this tag ((Enum-Intro) premise `E = enum { …, Kj(T̄j), … }`, " ++
  "§5.5; 6.3:5, 6.3:16; the compiler reports E0420)"

/-- (Enum-Intro) §5.5's payload arity premise (`6.3:16`). -/
def payloadCountMismatch : String :=
  "the construction does not supply exactly one argument per declared payload component " ++
  "((Enum-Intro) premise `Γ;Σ_{i-1};Λ ⊢ ei ⇒ Tji ⊣ Σi` over the whole tuple, §5.5; 6.3:16)"

/-- (Enum-Intro) §5.5's per-component premise (`6.3:16`). -/
def payloadTypeMismatch (T comp : Ty) : String :=
  "a payload argument has type " ++ Print.tyName T ++ " but its component is declared " ++
  Print.tyName comp ++ " ((Enum-Intro) premise `Γ;Σ_{i-1};Λ ⊢ ei ⇒ Tji ⊣ Σi`, §5.5; 6.3:16)"

/-- (Match) §5.5's premise `Γ;Σ;Λ ⊢ e0 ⇒ E ⊣ Σ0`: the scrutinee is an enum.
The core `match` is the enum elimination only — a bool or integer scrutinee is
an elaboration obligation §5.5 states, not a core form. -/
def scrutNotEnum (T : Ty) : String :=
  "the scrutinee has type " ++ Print.tyName T ++ ", but the core `match` eliminates an " ++
  "enum ((Match) premise `Γ;Σ;Λ ⊢ e0 ⇒ E ⊣ Σ0`, §5.5; a bool or integer scrutinee " ++
  "elaborates to nested `if`s, which is an elaboration obligation §5.5 states)"

/-- (Match) §5.5's exhaustiveness premise: exactly one arm per variant, in
declaration order (`4.7:9`, `4.7:10`). -/
def armCountMismatch (arms variants : Nat) : String :=
  "the match has " ++ toString arms ++ " arm(s) for an enum with " ++ toString variants ++
  " variant(s); (Match) requires exactly one arm per variant, in declaration order " ++
  "((Match) premise `arms are exhaustive: exactly the variants K1..Kn`, §5.5; 4.7:9, 4.7:10)"

/-- (Match) §5.5's per-arm §5.6 obligation: a payload local the arm neither
moves nor consumes is a leak (`6.3:17`; the compiler reports E0406). -/
def armLeak : String :=
  "an arm leaves one of its payload locals Owned at a linear type where the arm ends its " ++
  "scope, which is §5.6's leak check on the payload binding ((Match) premise " ++
  "`each pattern local x_{ij} leaves scope at arm end`, §5.5; 6.3:17, 3.8:32; the " ++
  "compiler reports E0406)"

/-- §5.7's loop-head state does not exist, or the iteration that computes it
(`headIter`, `Checker.lean`) did not reach it. -/
def loopHeadNone : String :=
  "no loop-head state: iterating `Σ_h = join(Σ, B_h)` from the entry state fails — the " ++
  "body is refused at a candidate head (the derivation below is the body at the entry " ++
  "state), or the entry and a back-edge state disagree on a linear-carrying binding, " ++
  "so the join is undefined (§5.7's `head(Σ, e)`; 3.8:79, 3.8:50; a value moved by one " ++
  "iteration is `MovedOut` at the head, and the compiler reports E0205 \"moved in a " ++
  "previous iteration\")"

/-- The body's type is not `unit`, which every loop rule of §5.7 asks. -/
def loopBodyNotUnit (T : Ty) : String :=
  "the loop body has type " ++ Print.tyName T ++ ", but §5.7 types a loop body at unit " ++
  "((Loop-Break)/(Loop-Div) premise `Γ;Σ_h;Λ ⊢ e ⇒ unit`)"

/-- (helper) Unreachable: `headIter` stops only at a head the step leaves
unchanged, which is the equation checked here. -/
def loopHeadNotFixpoint : String :=
  "the head the iteration found does not solve `Σ_h = head(Σ, e)` (§5.7); the iteration " ++
  "stops only at a head the step leaves unchanged, so no program reaches this premise"

/-- The `⟨diverge, Σ_h⟩` delivery's residual check (§5.6/§5.7's retained
non-panic check, frame-wide). -/
def divergeLeak : String :=
  "the loop re-enters itself forever and a binding of this frame is still Owned at a " ++
  "linear type at the loop head — the `⟨diverge, Σ_h⟩` delivery keeps §5.6's non-panic " ++
  "residual check ((Loop-Div-Backedge) §5.7; (Fn) §5.8; 3.8:62; the compiler reports E0406)"

/-- (Loop-Break) §5.7's discharge of the loop-local scopes an exit ends. -/
def breakLeak : String :=
  "a `break` leaves a binding the loop body opened Owned at a linear type — the exit ends " ++
  "its scope, where §5.6's obligation is discharged ((Loop-Break) §5.7, `outside_loop`; " ++
  "3.8:32; the compiler reports E0406)"

/-- (Loop-Break) §5.7's exit join, `join({ outside_loop(Σ_x) | Σ_x ∈ X })`
(`3.8:80`). -/
def exitJoinConflict (who : Option String) : String :=
  "the reachable exits disagree on a linear-carrying binding" ++
  (match who with | some w => " — " ++ w | none => "") ++
  ", so a linear value is consumed on only some exits ((Loop-Break) premise " ++
  "`Ω_exit = join({ outside_loop(Σ_x) | Σ_x ∈ X })`, §5.7; 3.8:80, 3.8:50; the compiler " ++
  "reports E0443)"

/-- An operator whose operand diverges: the checker names no type for it.
§5.3's (Strict-Bottom) concludes at the operator's own type, and that type is
read off the operand's, which a `never` operand does not fix, so `check`
refuses rather than guess (the incompleteness `Checker.lean`'s docstring
records). -/
def neverOperand : String :=
  "the operand diverges (its type is `never`), so the checker cannot name the operator's " ++
  "type ((Strict-Bottom) §5.3 concludes at the construct's own type `T_E`, which a `never` " ++
  "operand does not fix; a completeness limit of `check`, not a rule of the calculus)"

/-- Two arm types that do not meet: one type each, and different (§5.5's
single `T`, with (Sub-Never) §5.7 letting a diverging arm meet anything). -/
def armTypeMismatchC : CTy → CTy → String
  | .ty T₁, .ty T₂ => armTypeMismatch T₁ T₂
  | _, _ => subDerivation

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
source names it, so a join rejection can point at a binding. `lhs` and `rhs`
name the two states being joined: (If)'s then- and else-arm, or, at a `match`,
the arms the fold has already joined and the arm it failed on. -/
def joinConflictEntry (D : Decls) (lhs rhs : String) : Ctx → Ctx → Option String
  | a :: as, b :: bs =>
      if (a.join D b).isNone then
        some (Print.binderName as.length ++ ": " ++ Print.tyName a.ty ++ " is " ++
          ownStateName a.st ++ " in " ++ lhs ++ " and " ++ ownStateName b.st ++ " in " ++ rhs)
      else joinConflictEntry D lhs rhs as bs
  | _, _ => none

/-- (helper) The first arm at which (Match) §5.5's folded join fails, named the
way `joinConflictEntry` names the two-arm case — by **index**, because a
`match` has n arms and no then- or else-arm. `j` is the index of the arm the
fold is about to join, so the accumulator is arm `0` alone at `j = 1` and the
join of arms `0`–`j-1` above it. `one` and `many` name the things joined —
`arm`/`arms` at a `match`, `exit`/`exits` at (Loop-Break) §5.7's join over a
loop's `break` exits. -/
def joinFoldConflict (D : Decls) (one many : String) : Nat → Ctx → List Ctx → Option String
  | _, _, [] => none
  | j, acc, Γ :: Γs =>
      match Ctx.join D acc Γ with
      | some acc' => joinFoldConflict D one many (j + 1) acc' Γs
      | none =>
          joinConflictEntry D
            (if j == 1 then one ++ " 0" else many ++ " 0–" ++ toString (j - 1))
            (one ++ " " ++ toString j) acc Γ

/-- (helper) The same over the whole arm list, which is the fold `Ctx.joinAll`
takes (helper). -/
def joinAllConflict (D : Decls) : List Ctx → Option String
  | [] => none
  | Γ :: Γs => joinFoldConflict D "arm" "arms" 1 Γ Γs

/-- (helper) The same over a loop's exits, (Loop-Break) §5.7's join over the
`outside_loop` states of its `break`s, numbered in delivery order. -/
def exitJoinConflict (D : Decls) : List Ctx → Option String
  | [] => none
  | Γ :: Γs => joinFoldConflict D "exit" "exits" 1 Γ Γs

/-! ## Derivations -/

/-- What a rule concluded at one node: the §5 judgment's right-hand side
`⇒ T ⊣ Σ'`, or the premise that failed. -/
inductive Verdict where
  | accept (ty : CTy) (out : Out)
  | reject (premise : String)

/-- A derivation tree for the §5 judgment `Γ;Σ ⊢ e ⇒ T ⊣ Ω`: one node per
rule, carrying the rule's name as the calculus writes it, the incoming fused
`Γ;Σ`, the expression the rule concluded about, its verdict, and the
sub-derivations of its premises, in premise order. -/
inductive Deriv where
  | node (rule : String) (ctxIn : Ctx) (expr : Expr) (verdict : Verdict) (kids : List Deriv)

/-- The derivation's conclusion, in `check`'s shape: the type and outgoing
`Σ` of an accepted node, nothing for a rejected one. `explain_result` is the
proof that this projection is exactly `check` (§5 as an algorithm). -/
def Deriv.result : Deriv → Option (CTy × Out)
  | .node _ _ _ (.accept c Ω) _ => some (c, Ω)
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

/-- (helper) Why `Ty.atDyn` found no type for a dynamic tail: the first
dynamic step taken at something other than an array (`4.11:3`), or a constant
path that is not a path of the element it starts at. -/
def dynAtFailure (D : Decls) : Ty → List (List Nat) → String
  | .array E _, π :: πs =>
      (match E.atPath D π with
       | some T' => dynAtFailure D T' πs
       | none => Premise.pathNotField)
  | T, _ :: _ => Premise.notAnArray T
  | _, [] => Premise.pathNotField

/-- (helper) An accepting node. -/
def accepted (rule : String) (Γ : Ctx) (e : Expr) (c : CTy) (Ω : Out)
    (kids : List Deriv) : Deriv :=
  .node rule Γ e (.accept c Ω) kids

/-- (helper) An accepting node that continues at a type, delivering nothing:
every rule with no subexpression. -/
def acceptedAt (rule : String) (Γ : Ctx) (e : Expr) (T : Ty) (Γ' : Ctx)
    (kids : List Deriv) : Deriv :=
  accepted rule Γ e (.ty T) ⟨some Γ', []⟩ kids

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
      if InBounds w s n then acceptedAt "(Lit) §5.8" Γ (.intLit w s n) (.int w s) Γ []
      else rejected "(Lit) §5.8" Γ (.intLit w s n) (Premise.litOutOfRange (.int w s)) []
  | .boolLit b => acceptedAt "(Lit) §5.8" Γ (.boolLit b) .bool Γ []
  | .unitLit => acceptedAt "(Lit) §5.8" Γ .unitLit .unit Γ []
  | .use pl =>
      match Γ[pl.root]? with
      | none => rejected "(Use-Copy)/(Use-Move) §5.1" Γ (.use pl) Premise.unboundIndex []
      | some en =>
        match declaredPrefix P.decls en.ty pl.path with
        | some (πd, πs) =>
            (match en.st.get πd, en.ty.atPath P.decls πd, en.ty.atPath P.decls pl.path with
             | some u, some Td, some T =>
                 if u.fullyOwned ∧ linearResidue P.decls Td πs = false ∧
                     noDtorPrefix P.decls en.ty pl.path then
                   acceptedAt "(Use-Declared-Linear-Destructure) §5.1" Γ (.use pl) T
                     (Γ.set pl.root (en.setSt (en.st.setAt πd .movedOut))) []
                 else if !u.fullyOwned then
                   rejected "(Use-Declared-Linear-Destructure) §5.1" Γ (.use pl)
                     (Premise.destructurePartiallyMoved Td) []
                 else if linearResidue P.decls Td πs then
                   rejected "(Use-Declared-Linear-Destructure) §5.1" Γ (.use pl)
                     (Premise.residueCarriesLinear Td) []
                 else
                   rejected "(Use-Declared-Linear-Destructure) §5.1" Γ (.use pl)
                     Premise.moveUnderDtor []
             | none, _, _ =>
                 rejected "(Use-Declared-Linear-Destructure) §5.1" Γ (.use pl)
                   Premise.pathUnderMoved []
             | _, _, _ =>
                 rejected "(Use-Declared-Linear-Destructure) §5.1" Γ (.use pl)
                   Premise.pathNotField [])
        | none =>
          (match en.st.get pl.path, en.ty.atPath P.decls pl.path with
           | some u, some T =>
               if T.mult P.decls = .copy then
                 (if u.fullyOwned then acceptedAt "(Use-Copy) §5.1" Γ (.use pl) T Γ []
                  else rejected "(Use-Copy) §5.1" Γ (.use pl) (Premise.usePartiallyMoved T) [])
               else
                 (if u.fullyOwned ∧ noDtorPrefix P.decls en.ty pl.path ∧
                      rootIdxOnly P.decls en.ty pl.path then
                    acceptedAt "(Use-Move) §5.1" Γ (.use pl) T
                      (Γ.set pl.root (en.setSt (en.st.setAt pl.path .movedOut))) []
                  else if !rootIdxOnly P.decls en.ty pl.path then
                    rejected "(Use-Move) §5.1" Γ (.use pl) Premise.moveAtIndex []
                  else if u.fullyOwned then
                    rejected "(Use-Move) §5.1" Γ (.use pl) Premise.moveUnderDtor []
                  else rejected "(Use-Move) §5.1" Γ (.use pl) (Premise.usePartiallyMoved T) [])
           | none, _ => rejected "(Use-Copy)/(Use-Move) §5.1" Γ (.use pl) Premise.pathUnderMoved []
           | _, none => rejected "(Use-Copy)/(Use-Move) §5.1" Γ (.use pl) Premise.pathNotField [])
  | .binop op e₁ e₂ =>
      let rule := binopRule op
      let frule := floatBinopRule op
      let brule := binopRule op ++ " with (Strict-Bottom) §5.3"
      let fbrule := floatBinopRule op ++ " with (Strict-Bottom) §5.3"
      let d₁ := explain P R Γ e₁
      match d₁.result with
      | some (.ty (.int w s), ⟨some Γ₁, Δ₁⟩) =>
        let d₂ := explain P R Γ₁ e₂
        (match d₂.result with
         | some (.ty (.int w' s'), Ω₂) =>
             if w' = w ∧ s' = s ∧ op.intAdmits = true then
               accepted rule Γ (.binop op e₁ e₂) (.ty (op.resultTy (.int w s))) (Ω₂.add Δ₁)
                 [d₁, d₂]
             else if w' = w ∧ s' = s then
               rejected rule Γ (.binop op e₁ e₂)
                 (Premise.opNotOnInt op (.int w s)) [d₁, d₂]
             else
               rejected rule Γ (.binop op e₁ e₂)
                 (Premise.operandWidthMismatch (.int w s) (.int w' s')) [d₁, d₂]
         | some (.never, Ω₂) =>
             if op.intAdmits = true then
               accepted rule Γ (.binop op e₁ e₂) (.ty (op.resultTy (.int w s))) (Ω₂.add Δ₁)
                 [d₁, d₂]
             else
               rejected rule Γ (.binop op e₁ e₂) (Premise.opNotOnInt op (.int w s)) [d₁, d₂]
         | some (.ty T, _) => rejected rule Γ (.binop op e₁ e₂) (Premise.operandNotInt T) [d₁, d₂]
         | none => rejected rule Γ (.binop op e₁ e₂) Premise.subDerivation [d₁, d₂])
      | some (.ty (.int w s), ⟨none, Δ₁⟩) =>
          if op.intAdmits = true then
            accepted brule Γ (.binop op e₁ e₂) (.ty (op.resultTy (.int w s))) ⟨none, Δ₁⟩ [d₁]
          else rejected brule Γ (.binop op e₁ e₂) (Premise.opNotOnInt op (.int w s)) [d₁]
      | some (.ty (.float w), ⟨some Γ₁, Δ₁⟩) =>
        let d₂ := explain P R Γ₁ e₂
        (match d₂.result with
         | some (.ty (.float w'), Ω₂) =>
             if w' = w ∧ op.floatAdmits = true then
               accepted frule Γ (.binop op e₁ e₂) (.ty (op.resultTy (.float w))) (Ω₂.add Δ₁)
                 [d₁, d₂]
             else if w' = w then
               rejected frule Γ (.binop op e₁ e₂)
                 (Premise.opNotOnFloat op (.float w)) [d₁, d₂]
             else
               rejected frule Γ (.binop op e₁ e₂)
                 (Premise.operandWidthMismatch (.float w) (.float w')) [d₁, d₂]
         | some (.never, Ω₂) =>
             if op.floatAdmits = true then
               accepted frule Γ (.binop op e₁ e₂) (.ty (op.resultTy (.float w))) (Ω₂.add Δ₁)
                 [d₁, d₂]
             else
               rejected frule Γ (.binop op e₁ e₂) (Premise.opNotOnFloat op (.float w)) [d₁, d₂]
         | some (.ty T, _) =>
             rejected frule Γ (.binop op e₁ e₂) (Premise.operandNotScalar T) [d₁, d₂]
         | none => rejected frule Γ (.binop op e₁ e₂) Premise.subDerivation [d₁, d₂])
      | some (.ty (.float w), ⟨none, Δ₁⟩) =>
          if op.floatAdmits = true then
            accepted fbrule Γ (.binop op e₁ e₂) (.ty (op.resultTy (.float w))) ⟨none, Δ₁⟩ [d₁]
          else rejected fbrule Γ (.binop op e₁ e₂) (Premise.opNotOnFloat op (.float w)) [d₁]
      | some (.ty T, _) => rejected rule Γ (.binop op e₁ e₂) (Premise.operandNotScalar T) [d₁]
      | some (.never, _) => rejected rule Γ (.binop op e₁ e₂) Premise.neverOperand [d₁]
      | none => rejected rule Γ (.binop op e₁ e₂) Premise.subDerivation [d₁]
  | .floatLit w l =>
      if l.RoundsFinite w then acceptedAt "(Lit) §5.8" Γ (.floatLit w l) (.float w) Γ []
      else rejected "(Lit) §5.8" Γ (.floatLit w l) (Premise.floatLitInfinite w) []
  | .fintrin (.intToFloat w) e =>
      let d := explain P R Γ e
      (match d.result with
       | some (.ty (.int _ _), Ω) =>
           accepted "(Int-To-Float) §5.8" Γ (.fintrin (.intToFloat w) e) (.ty (.float w)) Ω [d]
       | some (.ty T, _) =>
           rejected "(Int-To-Float) §5.8" Γ (.fintrin (.intToFloat w) e)
             (Premise.intToFloatNotInt T) [d]
       | some (.never, _) =>
           rejected "(Int-To-Float) §5.8" Γ (.fintrin (.intToFloat w) e)
             Premise.neverOperand [d]
       | none =>
           rejected "(Int-To-Float) §5.8" Γ (.fintrin (.intToFloat w) e)
             Premise.subDerivation [d])
  | .fintrin k e =>
      let rule := fintrinRule k
      let d := explain P R Γ e
      (match d.result with
       | some (.ty (.float w), Ω) =>
           if k.floatSrc w then
             accepted rule Γ (.fintrin k e) (.ty (k.resTy w)) Ω [d]
           else
             rejected rule Γ (.fintrin k e) (Premise.floatCastSameWidth (.float w)) [d]
       | some (.ty T, _) => rejected rule Γ (.fintrin k e) (Premise.fintrinNotFloat k T) [d]
       | some (.never, _) => rejected rule Γ (.fintrin k e) Premise.neverOperand [d]
       | none => rejected rule Γ (.fintrin k e) Premise.subDerivation [d])
  | .unop .neg e =>
      let d := explain P R Γ e
      (match d.result with
       | some (.ty (.int w .signed), Ω) =>
           accepted "(Neg) §5.8" Γ (.unop .neg e) (.ty (.int w .signed)) Ω [d]
       | some (.ty (.float w), Ω) =>
           accepted "(Float-Neg) §5.8" Γ (.unop .neg e) (.ty (.float w)) Ω [d]
       | some (.ty T, _) => rejected "(Neg) §5.8" Γ (.unop .neg e) (Premise.negNotSigned T) [d]
       | some (.never, _) => rejected "(Neg) §5.8" Γ (.unop .neg e) Premise.neverOperand [d]
       | none => rejected "(Neg) §5.8" Γ (.unop .neg e) Premise.subDerivation [d])
  | .unop .not e =>
      let d := explain P R Γ e
      (match d.result with
       | some (.ty .bool, Ω) => accepted "(Not) §5.8" Γ (.unop .not e) (.ty .bool) Ω [d]
       | some (.ty T, _) => rejected "(Not) §5.8" Γ (.unop .not e) (Premise.notNotBool T) [d]
       | some (.never, _) => rejected "(Not) §5.8" Γ (.unop .not e) Premise.neverOperand [d]
       | none => rejected "(Not) §5.8" Γ (.unop .not e) Premise.subDerivation [d])
  | .unop .bitnot e =>
      let d := explain P R Γ e
      (match d.result with
       | some (.ty (.int w s), Ω) =>
           accepted "(BitNot) §5.8" Γ (.unop .bitnot e) (.ty (.int w s)) Ω [d]
       | some (.ty T, _) =>
           rejected "(BitNot) §5.8" Γ (.unop .bitnot e) (Premise.bitnotNotInt T) [d]
       | some (.never, _) =>
           rejected "(BitNot) §5.8" Γ (.unop .bitnot e) Premise.neverOperand [d]
       | none => rejected "(BitNot) §5.8" Γ (.unop .bitnot e) Premise.subDerivation [d])
  | .intCast w s e =>
      let d := explain P R Γ e
      (match d.result with
       | some (.ty (.int _ _), Ω) =>
           accepted "(Int-Cast) §5.8" Γ (.intCast w s e) (.ty (.int w s)) Ω [d]
       | some (.ty T, _) =>
           rejected "(Int-Cast) §5.8" Γ (.intCast w s e) (Premise.castNotInt T) [d]
       | some (.never, _) =>
           rejected "(Int-Cast) §5.8" Γ (.intCast w s e) Premise.neverOperand [d]
       | none => rejected "(Int-Cast) §5.8" Γ (.intCast w s e) Premise.subDerivation [d])
  | .panic msg =>
      accepted "(Panic) §5.8 + (Sub-Never) §5.7" Γ (.panic msg) .never ⟨none, []⟩ []
  | .dbg e =>
      let d := explain P R Γ e
      (match d.result with
       | some (.ty T, Ω) =>
           if T.observable then accepted "(Dbg) §5.8" Γ (.dbg e) (.ty .unit) Ω [d]
           else rejected "(Dbg) §5.8" Γ (.dbg e) (Premise.dbgNotObservable T) [d]
       | some (.never, _) => rejected "(Dbg) §5.8" Γ (.dbg e) Premise.neverOperand [d]
       | none => rejected "(Dbg) §5.8" Γ (.dbg e) Premise.subDerivation [d])
  | .mkStruct s args =>
      match P.decls.structs[s]? with
      | none => rejected "(Struct-Intro) §5.8" Γ (.mkStruct s args) Premise.unknownStruct []
      | some sd =>
        (match explainArgs P R Γ args sd.fields with
         | (some Ω, kids) =>
             accepted "(Struct-Intro) §5.8" Γ (.mkStruct s args) (.ty (.struct s)) Ω kids
         | (none, kids) =>
             rejected "(Struct-Intro) §5.8" Γ (.mkStruct s args)
               (fieldsPremise P R Γ args sd.fields) kids)
  | .mkEnum e k args =>
      match P.decls.enums[e]? with
      | none => rejected "(Enum-Intro) §5.5" Γ (.mkEnum e k args) Premise.unknownEnum []
      | some ed =>
        (match ed.variants[k]? with
         | none =>
             rejected "(Enum-Intro) §5.5" Γ (.mkEnum e k args) Premise.unknownVariant []
         | some Ts =>
           (match explainArgs P R Γ args Ts with
            | (some Ω, kids) =>
                accepted "(Enum-Intro) §5.5" Γ (.mkEnum e k args) (.ty (.enum e)) Ω kids
            | (none, kids) =>
                rejected "(Enum-Intro) §5.5" Γ (.mkEnum e k args)
                  (payloadPremise P R Γ args Ts) kids))
  | .«match» scrut arms =>
      let ds := explain P R Γ scrut
      match ds.result with
      | some (.ty (.enum e), ⟨some Γ₀, Δ₀⟩) =>
        (match P.decls.enums[e]? with
         | none => rejected "(Match) §5.5" Γ (.«match» scrut arms) Premise.unknownEnum [ds]
         | some ed =>
           if arms.length = ed.variants.length then
             (match checkArms P R Γ₀ (firstArmTy P R Γ₀ arms ed.variants) arms ed.variants with
              | none =>
                  rejected "(Match) §5.5" Γ (.«match» scrut arms)
                    (armsPremise P R Γ₀ (firstArmTy P R Γ₀ arms ed.variants) arms ed.variants)
                    (ds :: explainArms P R Γ₀ arms ed.variants)
              | some (os, Δs) =>
                (match Ctx.joinOpts P.decls os with
                 | some o =>
                     accepted "(Match) §5.5 join" Γ (.«match» scrut arms)
                       (firstArmTy P R Γ₀ arms ed.variants) ⟨o, Δs ++ Δ₀⟩
                       (ds :: explainArms P R Γ₀ arms ed.variants)
                 | none =>
                     rejected "(Match) §5.5 join" Γ (.«match» scrut arms)
                       (Premise.joinConflict true (joinAllConflict P.decls (os.filterMap id)))
                       (ds :: explainArms P R Γ₀ arms ed.variants)))
           else
             rejected "(Match) §5.5" Γ (.«match» scrut arms)
               (Premise.armCountMismatch arms.length ed.variants.length) [ds])
      | some (.ty (.enum _), ⟨none, Δ₀⟩) =>
          accepted "(Strict-Bottom) §5.3 at a match scrutinee" Γ (.«match» scrut arms) .never
            ⟨none, Δ₀⟩ [ds]
      | some (.never, ⟨none, Δ₀⟩) =>
          accepted "(Strict-Bottom) §5.3 at a match scrutinee" Γ (.«match» scrut arms) .never
            ⟨none, Δ₀⟩ [ds]
      | some (.ty T, _) =>
          rejected "(Match) §5.5" Γ (.«match» scrut arms) (Premise.scrutNotEnum T) [ds]
      | some (.never, _) =>
          rejected "(Match) §5.5" Γ (.«match» scrut arms) Premise.subDerivation [ds]
      | none => rejected "(Match) §5.5" Γ (.«match» scrut arms) Premise.subDerivation [ds]
  | .mkArray T args =>
      (match explainArgs P R Γ args (List.replicate args.length T) with
       | (some Ω, kids) =>
           accepted "(Array-Intro) §5.8" Γ (.mkArray T args) (.ty (.array T args.length)) Ω kids
       | (none, kids) =>
           rejected "(Array-Intro) §5.8" Γ (.mkArray T args)
             (elemsPremise P R Γ args (List.replicate args.length T)) kids)
  | .repeatArray T e n =>
      let rule := "(Array-Intro) §5.8, through §2's repeat elaboration"
      let d := explain P R Γ e
      (match d.result with
       | some (.ty T', Ω) =>
           if T' = T ∧ T.mult P.decls = .copy then
             accepted rule Γ (.repeatArray T e n) (.ty (.array T n)) Ω [d]
           else if T' = T then
             rejected rule Γ (.repeatArray T e n) (Premise.repeatNotCopy T) [d]
           else rejected rule Γ (.repeatArray T e n) (Premise.elemTypeMismatch T' T) [d]
       | some (.never, _) => rejected rule Γ (.repeatArray T e n) Premise.neverOperand [d]
       | none => rejected rule Γ (.repeatArray T e n) Premise.subDerivation [d])
  | .indexRead pl idx πs =>
      let rule := "(Use-Untrackable-Dynamic-Copy) §5.1"
      let brule := "(Use-Untrackable-Dynamic-Copy) §5.1 with (Strict-Bottom) §5.3"
      let ri := explainIdx P R Γ idx
      (match ri.1 with
       | none => rejected rule Γ (.indexRead pl idx πs) (idxPremise P R Γ idx) ri.2
       | some (_, ⟨some Γ₁, Δ⟩) =>
         (match Γ₁[pl.root]? with
          | none => rejected rule Γ (.indexRead pl idx πs) Premise.unboundIndex ri.2
          | some en =>
            match en.st.get pl.path, en.ty.atPath P.decls pl.path with
            | some u, some Ta =>
              (match Ta.atDyn P.decls πs with
               | some T =>
                   if idx.length = πs.length ∧ πs ≠ [] ∧ u.fullyOwned ∧
                       T.mult P.decls = .copy ∧
                       declaredPrefix P.decls en.ty pl.path = none ∧
                       Ta.dynNoDeclared P.decls πs then
                     accepted rule Γ (.indexRead pl idx πs) (.ty T) ⟨some Γ₁, Δ⟩ ri.2
                   else if ¬(idx.length = πs.length ∧ πs ≠ []) then
                     rejected rule Γ (.indexRead pl idx πs) Premise.dynShape ri.2
                   else if declaredPrefix P.decls en.ty pl.path ≠ none ∨
                       !Ta.dynNoDeclared P.decls πs then
                     rejected rule Γ (.indexRead pl idx πs) Premise.declaredLinearPrefix ri.2
                   else if !u.fullyOwned then
                     rejected rule Γ (.indexRead pl idx πs) Premise.indexPartiallyMoved ri.2
                   else rejected rule Γ (.indexRead pl idx πs) (Premise.elementNotCopy T) ri.2
               | none =>
                   rejected rule Γ (.indexRead pl idx πs) (dynAtFailure P.decls Ta πs) ri.2)
            | none, _ => rejected rule Γ (.indexRead pl idx πs) Premise.pathUnderMoved ri.2
            | _, none => rejected rule Γ (.indexRead pl idx πs) Premise.pathNotField ri.2)
       | some (_, ⟨none, Δ⟩) =>
         (match Γ[pl.root]? with
          | none => rejected brule Γ (.indexRead pl idx πs) Premise.unboundIndex ri.2
          | some en =>
            match en.ty.atPath P.decls pl.path with
            | some Ta =>
              (match Ta.atDyn P.decls πs with
               | some T =>
                   if idx.length = πs.length ∧ πs ≠ [] then
                     accepted brule Γ (.indexRead pl idx πs) (.ty T) ⟨none, Δ⟩ ri.2
                   else rejected brule Γ (.indexRead pl idx πs) Premise.dynShape ri.2
               | none =>
                   rejected brule Γ (.indexRead pl idx πs) (dynAtFailure P.decls Ta πs) ri.2)
            | none => rejected brule Γ (.indexRead pl idx πs) Premise.pathNotField ri.2))
  | .indexWrite pl idx πs e =>
      let rule := "(Assign) §5.2 below a dynamic index"
      let brule := "(Assign) §5.2 below a dynamic index, with (Strict-Bottom) §5.3"
      (match Γ[pl.root]? with
       | none => rejected rule Γ (.indexWrite pl idx πs e) Premise.unboundIndex []
       | some en₀ =>
         if en₀.mu = true then
           match en₀.st.get pl.path, en₀.ty.atPath P.decls pl.path with
           | some _, some Ta =>
             (match Ta.atDyn P.decls πs with
              | some T =>
                (let d := explain P R Γ e
                 match d.result with
                 | some (c, ⟨some Γ₁, Δ₁⟩) =>
                   if c.fits T then
                     (let ri := explainIdx P R Γ₁ idx
                      match ri.1 with
                      | some (_, ⟨some Γ₂, Δ₂⟩) =>
                        (match Γ₂[pl.root]? with
                         | some en₁ =>
                           (match en₁.st.get pl.path with
                            | some u₁ =>
                                if idx.length = πs.length ∧ πs ≠ [] ∧ u₁.fullyOwned ∧
                                    assignArrayOk P.decls en₁.st en₁.ty pl.path ∧
                                    T.mult P.decls ≠ .linear then
                                  accepted rule Γ (.indexWrite pl idx πs e) (.ty .unit)
                                    ⟨some (Γ₂.set pl.root (en₁.setSt (en₁.st.setAt pl.path .owned))),
                                      Δ₂ ++ Δ₁⟩
                                    (d :: ri.2)
                                else if ¬(idx.length = πs.length ∧ πs ≠ []) then
                                  rejected rule Γ (.indexWrite pl idx πs e)
                                    Premise.dynShape (d :: ri.2)
                                else if !u₁.fullyOwned then
                                  rejected rule Γ (.indexWrite pl idx πs e)
                                    Premise.indexPartiallyMoved (d :: ri.2)
                                else if !assignArrayOk P.decls en₁.st en₁.ty pl.path then
                                  rejected rule Γ (.indexWrite pl idx πs e)
                                    Premise.assignIntoPartialArray (d :: ri.2)
                                else
                                  rejected rule Γ (.indexWrite pl idx πs e)
                                    (Premise.linearOverwrite T) (d :: ri.2)
                            | none =>
                                rejected rule Γ (.indexWrite pl idx πs e)
                                  Premise.assignTargetLost (d :: ri.2))
                         | none =>
                             rejected rule Γ (.indexWrite pl idx πs e)
                               Premise.assignTargetLost (d :: ri.2))
                      | some (_, ⟨none, Δ₂⟩) =>
                          accepted brule Γ (.indexWrite pl idx πs e) (.ty .unit)
                            ⟨none, Δ₂ ++ Δ₁⟩ (d :: ri.2)
                      | none =>
                          rejected rule Γ (.indexWrite pl idx πs e)
                            (idxPremise P R Γ₁ idx) (d :: ri.2))
                   else
                     rejected rule Γ (.indexWrite pl idx πs e)
                       (Premise.assignTypeMismatch (c.pick T) T) [d]
                 | some (_, ⟨none, Δ₁⟩) =>
                     accepted brule Γ (.indexWrite pl idx πs e) (.ty .unit) ⟨none, Δ₁⟩ [d]
                 | none =>
                     rejected rule Γ (.indexWrite pl idx πs e) Premise.subDerivation [d])
              | none =>
                  rejected rule Γ (.indexWrite pl idx πs e) (dynAtFailure P.decls Ta πs) [])
           | none, _ => rejected rule Γ (.indexWrite pl idx πs e) Premise.pathUnderMoved []
           | _, none => rejected rule Γ (.indexWrite pl idx πs e) Premise.pathNotField []
         else rejected rule Γ (.indexWrite pl idx πs e) Premise.notMutable [])
  | .indexDrop pl idx πs =>
      let rule := "(@Drop-Copy) §5.3 below a dynamic index, with (Use-Untrackable-Dynamic-Copy) §5.1's premises"
      let brule := "(@Drop-Copy) §5.3 below a dynamic index, with (Strict-Bottom) §5.3"
      let ri := explainIdx P R Γ idx
      (match ri.1 with
       | none => rejected rule Γ (.indexDrop pl idx πs) (idxPremise P R Γ idx) ri.2
       | some (_, ⟨some Γ₁, Δ⟩) =>
         (match Γ₁[pl.root]? with
          | none => rejected rule Γ (.indexDrop pl idx πs) Premise.unboundIndex ri.2
          | some en =>
            match en.st.get pl.path, en.ty.atPath P.decls pl.path with
            | some u, some Ta =>
              (match Ta.atDyn P.decls πs with
               | some T =>
                   if idx.length = πs.length ∧ πs ≠ [] ∧ u.fullyOwned ∧
                       T.mult P.decls = .copy ∧
                       declaredPrefix P.decls en.ty pl.path = none ∧
                       Ta.dynNoDeclared P.decls πs then
                     accepted rule Γ (.indexDrop pl idx πs) (.ty .unit) ⟨some Γ₁, Δ⟩ ri.2
                   else if ¬(idx.length = πs.length ∧ πs ≠ []) then
                     rejected rule Γ (.indexDrop pl idx πs) Premise.dynShape ri.2
                   else if declaredPrefix P.decls en.ty pl.path ≠ none ∨
                       !Ta.dynNoDeclared P.decls πs then
                     rejected rule Γ (.indexDrop pl idx πs) Premise.declaredLinearPrefix ri.2
                   else if !u.fullyOwned then
                     rejected rule Γ (.indexDrop pl idx πs) Premise.indexPartiallyMoved ri.2
                   else rejected rule Γ (.indexDrop pl idx πs) (Premise.elementNotCopy T) ri.2
               | none =>
                   rejected rule Γ (.indexDrop pl idx πs) (dynAtFailure P.decls Ta πs) ri.2)
            | none, _ => rejected rule Γ (.indexDrop pl idx πs) Premise.pathUnderMoved ri.2
            | _, none => rejected rule Γ (.indexDrop pl idx πs) Premise.pathNotField ri.2)
       | some (_, ⟨none, Δ⟩) =>
         (match Γ[pl.root]? with
          | none => rejected brule Γ (.indexDrop pl idx πs) Premise.unboundIndex ri.2
          | some en =>
            match en.ty.atPath P.decls pl.path with
            | some Ta =>
              (match Ta.atDyn P.decls πs with
               | some _ =>
                   if idx.length = πs.length ∧ πs ≠ [] then
                     accepted brule Γ (.indexDrop pl idx πs) (.ty .unit) ⟨none, Δ⟩ ri.2
                   else rejected brule Γ (.indexDrop pl idx πs) Premise.dynShape ri.2
               | none =>
                   rejected brule Γ (.indexDrop pl idx πs) (dynAtFailure P.decls Ta πs) ri.2)
            | none => rejected brule Γ (.indexDrop pl idx πs) Premise.pathNotField ri.2))
  | .drop pl =>
      match Γ[pl.root]? with
      | none => rejected "(@Drop-Copy)/(@Drop) §5.3" Γ (.drop pl) Premise.unboundIndex []
      | some en =>
        match declaredPrefix P.decls en.ty pl.path with
        | some (πd, πs) =>
            (match en.st.get πd, en.ty.atPath P.decls πd, en.ty.atPath P.decls pl.path with
             | some u, some Td, some _T =>
                 if u.fullyOwned ∧ linearResidue P.decls Td πs = false ∧
                     noDtorPrefix P.decls en.ty pl.path then
                   acceptedAt "(@Drop) §5.3 at a declared-linear plan" Γ (.drop pl) .unit
                     (Γ.set pl.root (en.setSt (en.st.setAt πd .movedOut))) []
                 else if !u.fullyOwned then
                   rejected "(@Drop) §5.3 at a declared-linear plan" Γ (.drop pl)
                     (Premise.destructurePartiallyMoved Td) []
                 else if linearResidue P.decls Td πs then
                   rejected "(@Drop) §5.3 at a declared-linear plan" Γ (.drop pl)
                     (Premise.residueCarriesLinear Td) []
                 else
                   rejected "(@Drop) §5.3 at a declared-linear plan" Γ (.drop pl)
                     Premise.moveUnderDtor []
             | none, _, _ =>
                 rejected "(@Drop) §5.3 at a declared-linear plan" Γ (.drop pl)
                   Premise.pathUnderMoved []
             | _, _, _ =>
                 rejected "(@Drop) §5.3 at a declared-linear plan" Γ (.drop pl)
                   Premise.pathNotField [])
        | none =>
          (match en.st.get pl.path, en.ty.atPath P.decls pl.path with
           | some u, some T =>
               if T.mult P.decls = .copy then
                 (if u.fullyOwned then acceptedAt "(@Drop-Copy) §5.3" Γ (.drop pl) .unit Γ []
                  else rejected "(@Drop-Copy) §5.3" Γ (.drop pl) (Premise.usePartiallyMoved T) [])
               else
                 (if u.isOwned ∧ noDtorPrefix P.decls en.ty pl.path ∧
                     (u.fullyOwned = true ∨ residualLinearBelow P.decls u T = false) ∧
                     rootIdxOnly P.decls en.ty pl.path then
                    acceptedAt "(@Drop) §5.3" Γ (.drop pl) .unit
                      (Γ.set pl.root (en.setSt (en.st.setAt pl.path .movedOut))) []
                  else if !rootIdxOnly P.decls en.ty pl.path then
                    rejected "(@Drop) §5.3" Γ (.drop pl) Premise.moveAtIndex []
                  else if !u.isOwned then
                    rejected "(@Drop) §5.3" Γ (.drop pl) Premise.dropMovedOut []
                  else if !noDtorPrefix P.decls en.ty pl.path then
                    rejected "(@Drop) §5.3" Γ (.drop pl) Premise.moveUnderDtor []
                  else rejected "(@Drop) §5.3" Γ (.drop pl) Premise.dropStrandsLinear [])
           | none, _ => rejected "(@Drop-Copy)/(@Drop) §5.3" Γ (.drop pl) Premise.pathUnderMoved []
           | _, none => rejected "(@Drop-Copy)/(@Drop) §5.3" Γ (.drop pl) Premise.pathNotField [])
  | .letIn m e₁ e₂ =>
      let rule := "(Let) §5.3 + the §5.6 scope-exit leak check"
      let d₁ := explain P R Γ e₁
      match d₁.result with
      | some (.ty T₁, ⟨some Γ₁, Δ₁⟩) =>
        let d₂ := explain P R ({ ty := T₁, mu := m, st := .owned } :: Γ₁) e₂
        (match d₂.result with
         | some (c₂, ⟨some (en' :: Γ₂), Δ₂⟩) =>
             if residualLinear P.decls en'.st en'.ty then
               rejected rule Γ (.letIn m e₁ e₂) (Premise.letLeak T₁) [d₁, d₂]
             else
               accepted rule Γ (.letIn m e₁ e₂) c₂ ⟨some Γ₂, Δ₂ ++ Δ₁⟩ [d₁, d₂]
         | some (c₂, ⟨none, Δ₂⟩) =>
             accepted "(Let) §5.3, the body diverging" Γ (.letIn m e₁ e₂) c₂ ⟨none, Δ₂ ++ Δ₁⟩
               [d₁, d₂]
         | some (_, ⟨some [], _⟩) =>
             rejected rule Γ (.letIn m e₁ e₂) Premise.letBinderLost [d₁, d₂]
         | none => rejected rule Γ (.letIn m e₁ e₂) Premise.subDerivation [d₁, d₂])
      | some (_, ⟨none, Δ₁⟩) =>
          accepted "(Let-Bottom) §5.3 + (Sub-Never) §5.7" Γ (.letIn m e₁ e₂) .never ⟨none, Δ₁⟩
            [d₁]
      | some (.never, _) => rejected rule Γ (.letIn m e₁ e₂) Premise.subDerivation [d₁]
      | none => rejected rule Γ (.letIn m e₁ e₂) Premise.subDerivation [d₁]
  | .assign pl e =>
      match Γ[pl.root]? with
      | none => rejected "(Assign) §5.2, 3.8:77" Γ (.assign pl e) Premise.unboundIndex []
      | some en₀ =>
        if en₀.mu = true then
          match en₀.st.get pl.path, en₀.ty.atPath P.decls pl.path with
          | some _, some T =>
            (let d := explain P R Γ e
             match d.result with
             | some (c, ⟨some Γ₁, Δ⟩) =>
               if c.fits T then
                 (match Γ₁[pl.root]? with
                  | some en₁ =>
                    (match en₁.st.get pl.path with
                     | some u₁ =>
                         if assignArrayOk P.decls en₁.st en₁.ty pl.path ∧
                             overwriteOk P.decls u₁ T then
                           accepted "(Assign) §5.2, 3.8:77" Γ (.assign pl e) (.ty .unit)
                             ⟨some (Γ₁.set pl.root (en₁.setSt (en₁.st.setAt pl.path .owned))), Δ⟩
                             [d]
                         else if !assignArrayOk P.decls en₁.st en₁.ty pl.path then
                           rejected "(Assign) §5.2, 3.8:72, 7.1:46" Γ (.assign pl e)
                             Premise.assignIntoPartialArray [d]
                         else
                           rejected "(Assign) §5.2, 3.8:77" Γ (.assign pl e)
                             (Premise.linearOverwrite T) [d]
                     | none =>
                         rejected "(Assign) §5.2, 3.8:77" Γ (.assign pl e)
                           Premise.pathUnderMoved [d])
                  | none =>
                      rejected "(Assign) §5.2, 3.8:77" Γ (.assign pl e)
                        Premise.assignTargetLost [d])
               else
                 rejected "(Assign) §5.2, 3.8:77" Γ (.assign pl e)
                   (Premise.assignTypeMismatch (c.pick T) T) [d]
             | some (_, ⟨none, Δ⟩) =>
                 accepted "(Assign) §5.2 with (Strict-Bottom) §5.3" Γ (.assign pl e) (.ty .unit)
                   ⟨none, Δ⟩ [d]
             | none =>
                 rejected "(Assign) §5.2, 3.8:77" Γ (.assign pl e) Premise.subDerivation [d])
          | none, _ =>
              rejected "(Assign) §5.2, 3.8:77" Γ (.assign pl e) Premise.pathUnderMoved []
          | _, none =>
              rejected "(Assign) §5.2, 3.8:77" Γ (.assign pl e) Premise.pathNotField []
        else rejected "(Assign) §5.2, 3.8:77" Γ (.assign pl e) Premise.notMutable []
  | .seq e₁ e₂ =>
      let d₁ := explain P R Γ e₁
      match d₁.result with
      | some (.ty T₁, ⟨some Γ₁, Δ₁⟩) =>
          if T₁.mult P.decls = .linear then
            rejected "(Seq) §5.3, 3.8:64" Γ (.seq e₁ e₂) (Premise.discardsLinear T₁) [d₁]
          else
            let d₂ := explain P R Γ₁ e₂
            (match d₂.result with
             | some (c₂, Ω₂) => accepted "(Seq) §5.3, 3.8:64" Γ (.seq e₁ e₂) c₂ (Ω₂.add Δ₁) [d₁, d₂]
             | none => rejected "(Seq) §5.3, 3.8:64" Γ (.seq e₁ e₂) Premise.subDerivation [d₁, d₂])
      | some (_, ⟨none, Δ₁⟩) =>
          accepted "(Seq-Bottom) §5.3 + (Sub-Never) §5.7" Γ (.seq e₁ e₂) .never ⟨none, Δ₁⟩ [d₁]
      | some (.never, _) => rejected "(Seq) §5.3, 3.8:64" Γ (.seq e₁ e₂) Premise.subDerivation [d₁]
      | none => rejected "(Seq) §5.3, 3.8:64" Γ (.seq e₁ e₂) Premise.subDerivation [d₁]
  | .ite c e₁ e₂ =>
      let dc := explain P R Γ c
      match dc.result with
      | some (.ty .bool, ⟨some Γ₀, Δ₀⟩) =>
        let d₁ := explain P R Γ₀ e₁
        let d₂ := explain P R Γ₀ e₂
        (match d₁.result, d₂.result with
         | some (c₁, Ω₁), some (c₂, Ω₂) =>
             (match CTy.meet c₁ c₂ with
              | some c' =>
                (match Ctx.joinOpt P.decls Ω₁.norm Ω₂.norm with
                 | some o =>
                     accepted "(If) §5.5 join" Γ (.ite c e₁ e₂) c' ⟨o, Ω₁.brk ++ Ω₂.brk ++ Δ₀⟩
                       [dc, d₁, d₂]
                 | none =>
                     rejected "(If) §5.5 join" Γ (.ite c e₁ e₂)
                       (Premise.joinConflict false
                         (joinConflictEntry P.decls "the then-arm" "the else-arm"
                           (Ω₁.norm.getD []) (Ω₂.norm.getD [])))
                       [dc, d₁, d₂])
              | none =>
                  rejected "(If) §5.5 join" Γ (.ite c e₁ e₂)
                    (Premise.armTypeMismatchC c₁ c₂) [dc, d₁, d₂])
         | _, _ => rejected "(If) §5.5 join" Γ (.ite c e₁ e₂) Premise.subDerivation [dc, d₁, d₂])
      | some (cc, ⟨none, Δ₀⟩) =>
          if cc.fits .bool then
            accepted "(Strict-Bottom) §5.3 at a condition" Γ (.ite c e₁ e₂) .never ⟨none, Δ₀⟩ [dc]
          else
            rejected "(If) §5.5 join" Γ (.ite c e₁ e₂) (Premise.condNotBool (cc.pick .bool)) [dc]
      | some (.ty T, _) =>
          rejected "(If) §5.5 join" Γ (.ite c e₁ e₂) (Premise.condNotBool T) [dc]
      | some (.never, _) => rejected "(If) §5.5 join" Γ (.ite c e₁ e₂) Premise.subDerivation [dc]
      | none => rejected "(If) §5.5 join" Γ (.ite c e₁ e₂) Premise.subDerivation [dc]
  | .call f args =>
      match P.fns[f]? with
      | none => rejected "(Call) §5.8" Γ (.call f args) Premise.unknownCallee []
      | some fd =>
        (match explainArgs P R Γ args (fd.params.map Param.ty) with
         | (some Ω, kids) => accepted "(Call) §5.8" Γ (.call f args) (.ty fd.ret) Ω kids
         | (none, kids) =>
             rejected "(Call) §5.8" Γ (.call f args)
               (argsPremise P R Γ args (fd.params.map Param.ty)) kids)
  | .ret e =>
      let d := explain P R Γ e
      match d.result with
      | none => rejected "(Return-Value) §5.7" Γ (.ret e) Premise.subDerivation [d]
      | some (c, ⟨some Γ₁, Δ⟩) =>
          if c.fits R ∧ NoResidualLinear P.decls Γ₁ then
            accepted "(Return-Value) §5.7" Γ (.ret e) .never ⟨none, Δ⟩ [d]
          else if c.fits R then
            rejected "(Return-Value) §5.7" Γ (.ret e) Premise.returnLeak [d]
          else
            rejected "(Return-Value) §5.7" Γ (.ret e) (Premise.returnTypeMismatch (c.pick R) R) [d]
      | some (c, ⟨none, Δ⟩) =>
          if c.fits R then
            accepted "(Return-Bottom) §5.7" Γ (.ret e) .never ⟨none, Δ⟩ [d]
          else
            rejected "(Return-Bottom) §5.7" Γ (.ret e) (Premise.returnTypeMismatch (c.pick R) R) [d]
  | .brk => accepted "(Break) §5.7 + (Sub-Never) §5.7" Γ .brk .never ⟨none, [Γ]⟩ []
  | .loop e =>
      -- `check`'s loop, node for node: the head by iteration, the body at the
      -- head (the one sub-derivation), the equation, then the rule `4.8:21`'s
      -- syntactic classification picks.
      let rule := if e.breaks then "(Loop-Break) §5.7" else "(Loop-Div) §5.7"
      match headIter P.decls (fun Γ' => ((explain P R Γ' e).result).map (fun r => r.2.norm)) Γ
          (e.nodes + 2) Γ with
      | none => rejected rule Γ (.loop e) Premise.loopHeadNone [explain P R Γ e]
      | some Γh =>
        let d := explain P R Γh e
        match d.result with
        | some (c, Ωe) =>
          if c.fits .unit ∧ Ctx.joinOpt P.decls (some Γ) Ωe.norm = some (some Γh) ∧
              (Ωe.norm = none ∨ Ctx.Wf P.decls Γh) then
            if e.breaks then
              match Ωe.brk with
              | [] =>
                  if Ωe.norm = none ∨ NoResidualLinear P.decls Γh then
                    accepted "(Loop-Break) §5.7, no reachable exit" Γ (.loop e) (.ty .unit)
                      ⟨none, []⟩ [d]
                  else rejected rule Γ (.loop e) Premise.divergeLeak [d]
              | Γb₀ :: Γbs =>
                  if (Γb₀ :: Γbs).all
                      (fun Γb => decide (NoResidualLinear P.decls (Ctx.loopLocals Γh Γb))) then
                    match Ctx.joinAll P.decls ((Γb₀ :: Γbs).map (Ctx.outsideLoop Γh)) with
                    | some Γx => accepted rule Γ (.loop e) (.ty .unit) ⟨some Γx, []⟩ [d]
                    | none =>
                        rejected rule Γ (.loop e)
                          (Premise.exitJoinConflict
                            (exitJoinConflict P.decls ((Γb₀ :: Γbs).map (Ctx.outsideLoop Γh))))
                          [d]
                  else rejected rule Γ (.loop e) Premise.breakLeak [d]
            else if Ωe.norm = none ∨ NoResidualLinear P.decls Γh then
              accepted
                (if Ωe.norm.isSome then "(Loop-Div-Backedge) §5.7 + (Sub-Never) §5.7"
                 else "(Loop-Div) §5.7 + (Sub-Never) §5.7")
                Γ (.loop e) .never ⟨none, []⟩ [d]
            else rejected "(Loop-Div-Backedge) §5.7" Γ (.loop e) Premise.divergeLeak [d]
          else if c.fits .unit then
            rejected rule Γ (.loop e) Premise.loopHeadNotFixpoint [d]
          else rejected rule Γ (.loop e) (Premise.loopBodyNotUnit (c.pick .unit)) [d]
        | none => rejected rule Γ (.loop e) Premise.subDerivation [d]

/-- The instrumented mirror of `checkArgs` (§5.8's (Call) argument list):
the sub-derivations in argument order, and the outgoing `Ω` when every
argument checked at its parameter's type, or `⊥` from the first that
diverged (§5.3's (Strict-Bottom)). -/
def explainArgs (P : Program) (R : Ty) : Ctx → List Expr → List Ty → Option Out × List Deriv
  | Γ, [], [] => (some ⟨some Γ, []⟩, [])
  | Γ, e :: es, T :: Ts =>
      let d := explain P R Γ e
      (match d.result with
       | some (c, ⟨some Γ₁, Δ₁⟩) =>
           if c.fits T then
             let rest := explainArgs P R Γ₁ es Ts
             ((match rest.1 with
               | some Ω => some (Ω.add Δ₁)
               | none => none), d :: rest.2)
           else (none, [d])
       | some (c, ⟨none, Δ₁⟩) =>
           if c.fits T ∧ es.length = Ts.length then (some ⟨none, Δ₁⟩, [d]) else (none, [d])
       | none => (none, [d]))
  | _, _, _ => (none, [])

/-- The instrumented mirror of `checkIdx`: the index expressions'
sub-derivations in evaluation order, and their integer types with the outgoing
`Ω` when every one checked at an integer type (`4.11:4`). -/
def explainIdx (P : Program) (R : Ty) : Ctx → List Expr → Option (List Ty × Out) × List Deriv
  | Γ, [] => (some ([], ⟨some Γ, []⟩), [])
  | Γ, e :: es =>
      let d := explain P R Γ e
      (match d.result with
       | some (.ty (.int w s), ⟨some Γ₁, Δ₁⟩) =>
           let rest := explainIdx P R Γ₁ es
           ((match rest.1 with
             | some (Ts, Ω) => some (.int w s :: Ts, Ω.add Δ₁)
             | none => none), d :: rest.2)
       | some (.ty (.int w s), ⟨none, Δ₁⟩) =>
           (some (.int w s :: es.map (fun _ => .int w s), ⟨none, Δ₁⟩), [d])
       | _ => (none, [d]))

/-- The premise a rejected index list failed: the first index whose
derivation failed, or whose type is not an integer (`4.11:4`). -/
def idxPremise (P : Program) (R : Ty) : Ctx → List Expr → String
  | _, [] => Premise.subDerivation
  | Γ, e :: es =>
      (match (explain P R Γ e).result with
       | some (.ty (.int _ _), ⟨some Γ₁, _⟩) => idxPremise P R Γ₁ es
       | some (.ty T, _) => Premise.indexNotInt T
       | some (.never, _) => Premise.neverOperand
       | none => Premise.subDerivation)

/-- The premise a rejected argument list failed: a count mismatch (`4.10:3`),
or the first argument whose type is not its parameter's (`4.10:4`) — the two
per-argument premises of (Call) §5.8. -/
def argsPremise (P : Program) (R : Ty) : Ctx → List Expr → List Ty → String
  | _, [], [] => Premise.argCountMismatch
  | Γ, e :: es, T :: Ts =>
      (match (explain P R Γ e).result with
       | some (c, ⟨some Γ₁, _⟩) =>
           if c.fits T then argsPremise P R Γ₁ es Ts else Premise.argTypeMismatch (c.pick T) T
       | some (c, ⟨none, _⟩) =>
           if c.fits T then Premise.argCountMismatch else Premise.argTypeMismatch (c.pick T) T
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
       | some (c, ⟨some Γ₁, _⟩) =>
           if c.fits T then fieldsPremise P R Γ₁ es Ts
           else Premise.fieldTypeMismatch (c.pick T) T
       | some (c, ⟨none, _⟩) =>
           if c.fits T then Premise.fieldCountMismatch
           else Premise.fieldTypeMismatch (c.pick T) T
       | none => Premise.subDerivation)
  | _, _, _ => Premise.fieldCountMismatch

/-- The premise a rejected payload list failed: the wrong arity, or the first
component whose type is not its declared one — the per-component premises of
(Enum-Intro) §5.5 (`6.3:16`). -/
def payloadPremise (P : Program) (R : Ty) : Ctx → List Expr → List Ty → String
  | _, [], [] => Premise.payloadCountMismatch
  | Γ, e :: es, T :: Ts =>
      (match (explain P R Γ e).result with
       | some (c, ⟨some Γ₁, _⟩) =>
           if c.fits T then payloadPremise P R Γ₁ es Ts
           else Premise.payloadTypeMismatch (c.pick T) T
       | some (c, ⟨none, _⟩) =>
           if c.fits T then Premise.payloadCountMismatch
           else Premise.payloadTypeMismatch (c.pick T) T
       | none => Premise.subDerivation)
  | _, _, _ => Premise.payloadCountMismatch

/-- The sub-derivations of (Match) §5.5's arm premises: one per arm, each under
that variant's payload locals (`armCtx`), all from the same post-scrutinee
state. -/
def explainArms (P : Program) (R : Ty) (Γ₀ : Ctx) : List Expr → List (List Ty) → List Deriv
  | [], _ => []
  | _, [] => []
  | e :: es, Ts :: Tss => explain P R (armCtx Ts Γ₀) e :: explainArms P R Γ₀ es Tss

/-- The premise a rejected arm list failed: the first arm whose body does not
check, whose type is not the one the first typed arm fixed, or which, when it
continues, leaves a payload local unconsumed at the arm's end (§5.6). -/
def armsPremise (P : Program) (R : Ty) (Γ₀ : Ctx) (c : CTy) :
    List Expr → List (List Ty) → String
  | [], [] => Premise.subDerivation
  | e :: es, Ts :: Tss =>
      (match (explain P R (armCtx Ts Γ₀) e).result with
       | some (c', ⟨some Γb, _⟩) =>
           if !c'.fitsC c then Premise.armTypeMismatchC c' c
           else if !decide (NoResidualLinear P.decls (Γb.take Ts.length)) then Premise.armLeak
           else armsPremise P R Γ₀ c es Tss
       | some (c', ⟨none, _⟩) =>
           if !c'.fitsC c then Premise.armTypeMismatchC c' c
           else armsPremise P R Γ₀ c es Tss
       | none => Premise.subDerivation)
  | _, _ => Premise.subDerivation
/-- The premise a rejected element list failed: the first element whose type is
not the array's element type (`3.5:3`, `7.1:3`) — (Array-Intro) §5.8's only
per-element premise, since the length is the literal's own. -/
def elemsPremise (P : Program) (R : Ty) : Ctx → List Expr → List Ty → String
  | _, [], [] => Premise.subDerivation
  | Γ, e :: es, T :: Ts =>
      (match (explain P R Γ e).result with
       | some (c, ⟨some Γ₁, _⟩) =>
           if c.fits T then elemsPremise P R Γ₁ es Ts
           else Premise.elemTypeMismatch (c.pick T) T
       | some (c, ⟨none, _⟩) =>
           if c.fits T then Premise.subDerivation else Premise.elemTypeMismatch (c.pick T) T
       | none => Premise.subDerivation)
  | _, _, _ => Premise.subDerivation
end

-- The budget is per declaration, and the `assign` and `ret` arms of
-- `explain_result` are where it goes: each `check` arm now splits on the
-- operand's `Ω` (a continuing state or §5.7's `⊥`) as well as on its type, so
-- the case analysis roughly doubles (RUE-2368). The default fails at those two
-- arms and 400000 passes; the proof is unchanged.
set_option maxHeartbeats 400000 in
mutual
/-- **The derivation is the checker.** Projecting a derivation to its
conclusion reproduces `check P R Γ e` exactly, so a rendered derivation can
never claim an acceptance or a rejection the verified checker (§5,
`check_sound`) does not make. -/
theorem explain_result {P : Program} {R : Ty} : ∀ (e : Expr) (Γ : Ctx),
    (explain P R Γ e).result = check P R Γ e
  | .intLit w s n, Γ => by
      simp only [explain, check]; split <;> rfl
  | .floatLit w l, Γ => by
      simp only [explain, check]; split <;> rfl
  | .boolLit b, Γ => rfl
  | .unitLit, Γ => rfl
  | .use pl, Γ => by
      simp only [explain, check]
      (repeat' split) <;> first | rfl | simp_all [accepted, acceptedAt, rejected, Deriv.result]
  | .binop op e₁ e₂, Γ => by
      simp only [explain, check, explain_result e₁, explain_result e₂]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, acceptedAt, rejected, Deriv.result] <;> grind)
  | .unop .neg e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, acceptedAt, rejected, Deriv.result] <;> grind)
  | .unop .not e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, acceptedAt, rejected, Deriv.result] <;> grind)
  | .unop .bitnot e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, acceptedAt, rejected, Deriv.result] <;> grind)
  | .intCast w s e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, acceptedAt, rejected, Deriv.result] <;> grind)
  | .fintrin (.intToFloat w) e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, acceptedAt, rejected, Deriv.result] <;> grind)
  | .fintrin (.floatToInt w s) e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, acceptedAt, rejected, Deriv.result] <;> grind)
  | .fintrin (.floatCast w) e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, acceptedAt, rejected, Deriv.result] <;> grind)
  | .fintrin (.roundOp k) e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, acceptedAt, rejected, Deriv.result] <;> grind)
  | .panic msg, Γ => rfl
  | .dbg e, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, acceptedAt, Deriv.result] <;> grind)
  | .mkStruct s args, Γ => by
      simp only [explain, check]
      cases hs : P.decls.structs[s]? with
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
          | some Ω' => rw [← hargs]; rfl
  | .mkEnum e k args, Γ => by
      simp only [explain, check]
      cases hed : P.decls.enums[e]? with
      | none => rfl
      | some ed =>
        dsimp only
        cases hv : ed.variants[k]? with
        | none => rfl
        | some Ts =>
          dsimp only
          have hargs := explainArgs_result (P := P) (R := R) args Γ Ts
          revert hargs
          cases explainArgs P R Γ args Ts with
          | mk res kids =>
            intro hargs
            cases res with
            | none => rw [← hargs]; rfl
            | some Ω' => rw [← hargs]; rfl
  | .«match» scrut arms, Γ => by
      simp only [explain, check, explain_result scrut]
      cases hs : check P R Γ scrut with
      | none => rfl
      | some p =>
        obtain ⟨csc, o₀, Δ₀⟩ := p
        cases csc with
        | never => cases o₀ <;> rfl
        | ty Tsc =>
          cases Tsc with
          | int => rfl
          | float => rfl
          | bool => rfl
          | unit => rfl
          | struct s' => rfl
          | array Te n => rfl
          | enum e =>
            cases o₀ with
            | none => rfl
            | some Γ₀ =>
              dsimp only
              cases hed : P.decls.enums[e]? with
              | none => rfl
              | some ed =>
                dsimp only
                by_cases hlen : arms.length = ed.variants.length
                · simp only [if_pos hlen]
                  cases hc : checkArms P R Γ₀ (firstArmTy P R Γ₀ arms ed.variants) arms
                      ed.variants with
                  | none => rfl
                  | some r =>
                      obtain ⟨os, Δs⟩ := r
                      dsimp only
                      cases hj : Ctx.joinOpts P.decls os <;> rfl
                · simp only [if_neg hlen]; rfl
  | .mkArray Te args, Γ => by
      simp only [explain, check]
      have hargs := explainArgs_result (P := P) (R := R) args Γ (List.replicate args.length Te)
      revert hargs
      cases explainArgs P R Γ args (List.replicate args.length Te) with
      | mk res kids =>
        intro hargs
        cases res with
        | none => rw [← hargs]; rfl
        | some Γ' => rw [← hargs]; rfl
  | .repeatArray Te e n, Γ => by
      simp only [explain, check, explain_result e]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, acceptedAt, Deriv.result] <;> grind)
  | .indexRead pl idx πs, Γ => by
      simp only [explain, check, explainIdx_result idx]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, acceptedAt, rejected, Deriv.result] <;> grind)
  | .indexWrite pl idx πs e, Γ => by
      simp only [explain, check, explain_result e, explainIdx_result idx]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, acceptedAt, rejected, Deriv.result] <;> grind)
  | .indexDrop pl idx πs, Γ => by
      simp only [explain, check, explainIdx_result idx]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, acceptedAt, rejected, Deriv.result] <;> grind)
  | .drop pl, Γ => by
      simp only [explain, check]
      (repeat' split) <;> first | rfl | simp_all [accepted, acceptedAt, rejected, Deriv.result]
  | .letIn m e₁ e₂, Γ => by
      simp only [explain, check, explain_result e₁]
      cases h₁ : check P R Γ e₁ with
      | none => rfl
      | some p =>
        obtain ⟨c₁, o₁, Δ₁⟩ := p
        cases o₁ with
        | none => cases c₁ <;> rfl
        | some Γ₁ =>
          cases c₁ with
          | never => rfl
          | ty T₁ =>
            simp only [explain_result e₂]
            cases h₂ : check P R ({ ty := T₁, mu := m, st := .owned } :: Γ₁) e₂ with
            | none => rfl
            | some q =>
              obtain ⟨c₂, o₂, Δ₂⟩ := q
              cases o₂ with
              | none => rfl
              | some Γb =>
                cases Γb with
                | nil => rfl
                | cons en' Γ₂ => dsimp only; split <;> rfl
  | .assign pl e, Γ => by
      simp only [explain, check]
      cases hg : Γ[pl.root]? with
      | none => rfl
      | some en₀ =>
        dsimp only
        by_cases hmu : en₀.mu = true
        · simp only [hmu, if_true]
          cases hs : en₀.st.get pl.path with
          | none => rfl
          | some u₀ =>
            cases ht : en₀.ty.atPath P.decls pl.path with
            | none => rfl
            | some T =>
              dsimp only
              rw [explain_result e]
              cases hc : check P R Γ e with
              | none => rfl
              | some r =>
                obtain ⟨c, o, Δ⟩ := r
                cases o with
                | none => rfl
                | some Γ₁ =>
                  dsimp only
                  by_cases hT : c.fits T = true
                  · simp only [hT, if_true]
                    cases hg₁ : Γ₁[pl.root]? with
                    | none => rfl
                    | some en₁ =>
                      dsimp only
                      cases hu : en₁.st.get pl.path with
                      | none => rfl
                      | some u₁ =>
                        dsimp only
                        by_cases hA : assignArrayOk P.decls en₁.st en₁.ty pl.path = true ∧
                            overwriteOk P.decls u₁ T = true
                        · rw [if_pos hA, if_pos hA]; rfl
                        · rw [if_neg hA, if_neg hA]; split <;> rfl
                  · simp only [hT, Bool.false_eq_true, if_false]; rfl
        · simp only [hmu, Bool.false_eq_true, if_false]; rfl
  | .seq e₁ e₂, Γ => by
      simp only [explain, check, explain_result e₁, explain_result e₂]
      (repeat' split) <;>
        first | rfl | (simp_all [accepted, acceptedAt, rejected, Deriv.result] <;> grind)
  | .ite c e₁ e₂, Γ => by
      simp only [explain, check, explain_result c]
      cases hc : check P R Γ c with
      | none => rfl
      | some p =>
        obtain ⟨cc, o₀, Δ₀⟩ := p
        cases o₀ with
        | none =>
            cases cc with
            | never => (try dsimp only); split <;> rfl
            | ty T => cases T <;> ((try dsimp only); split <;> rfl)
        | some Γ₀ =>
          cases cc with
          | never => rfl
          | ty T =>
            cases T with
            | int => rfl
            | float => rfl
            | unit => rfl
            | struct s' => rfl
            | enum e' => rfl
            | array Te n => rfl
            | bool =>
                simp only [explain_result e₁, explain_result e₂]
                cases h₁ : check P R Γ₀ e₁ with
                | none => cases h₂ : check P R Γ₀ e₂ <;> rfl
                | some q₁ =>
                  cases h₂ : check P R Γ₀ e₂ with
                  | none => rfl
                  | some q₂ =>
                    obtain ⟨c₁, Ω₁⟩ := q₁
                    obtain ⟨c₂, Ω₂⟩ := q₂
                    simp only []
                    cases hm : CTy.meet c₁ c₂ with
                    | none => rfl
                    | some c' =>
                        dsimp only
                        cases hj : Ctx.joinOpt P.decls Ω₁.norm Ω₂.norm <;> rfl
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
          | some Ω' => rw [← hargs]; rfl
  | .ret e, Γ => by
      simp only [explain, check, explain_result e]
      cases hc : check P R Γ e with
      | none => rfl
      | some r =>
        obtain ⟨c, o, Δ⟩ := r
        cases o with
        | none => dsimp only; split <;> rfl
        | some Γ₁ =>
          dsimp only
          by_cases h : c.fits R = true ∧ NoResidualLinear P.decls Γ₁
          · rw [if_pos h, if_pos h]; rfl
          · rw [if_neg h, if_neg h]; split <;> rfl
  | .brk, Γ => rfl
  | .loop e, Γ => by
      simp only [explain, check, explain_result e]
      cases hh : headIter P.decls (fun Γ' => Option.map (fun r => r.snd.norm) (check P R Γ' e))
          Γ (e.nodes + 2) Γ with
      | none => rfl
      | some Γh =>
        dsimp only
        cases hc : check P R Γh e with
        | none => rfl
        | some r =>
          obtain ⟨c, Ωe⟩ := r
          dsimp only
          by_cases h₁ : c.fits .unit = true ∧ Ctx.joinOpt P.decls (some Γ) Ωe.norm = some (some Γh) ∧
              (Ωe.norm = none ∨ Ctx.Wf P.decls Γh)
          · simp only [if_pos h₁]
            by_cases hb : e.breaks = true
            · simp only [if_pos hb]
              cases Ωe.brk with
              | nil =>
                  dsimp only
                  by_cases h₂ : Ωe.norm = none ∨ NoResidualLinear P.decls Γh
                  · simp only [if_pos h₂]; rfl
                  · simp only [if_neg h₂]; rfl
              | cons Γb₀ Γbs =>
                  dsimp only
                  split
                  · rename_i h₃
                    try simp only [if_pos h₃]
                    cases Ctx.joinAll P.decls ((Γb₀ :: Γbs).map (Ctx.outsideLoop Γh)) <;> rfl
                  · rename_i h₃
                    try simp only [if_neg h₃]
                    rfl
            · simp only [if_neg hb]
              by_cases h₂ : Ωe.norm = none ∨ NoResidualLinear P.decls Γh
              · simp only [if_pos h₂]; rfl
              · simp only [if_neg h₂]; rfl
          · simp only [if_neg h₁]; split <;> rfl

/-- **The index-list derivations are the checker's** (helper). -/
theorem explainIdx_result {P : Program} {R : Ty} : ∀ (es : List Expr) (Γ : Ctx),
    (explainIdx P R Γ es).1 = checkIdx P R Γ es
  | [], Γ => rfl
  | e :: es, Γ => by
      simp only [explainIdx, checkIdx, explain_result e]
      cases hr : check P R Γ e with
      | none => rfl
      | some p =>
        obtain ⟨c, o, Δ⟩ := p
        cases c with
        | never => rfl
        | ty T' =>
          cases T' with
          | int w s =>
              cases o with
              | none => rfl
              | some Γ₁ =>
                simp only []
                rw [explainIdx_result es Γ₁]
                rcases checkIdx P R Γ₁ es with _ | ⟨_, _⟩ <;> rfl
          | float _ | bool | unit | struct _ | enum _ | array _ _ => rfl

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
        obtain ⟨c, o, Δ⟩ := p
        cases o with
        | none => simp only []; split <;> rfl
        | some Γ₁ =>
          simp only []
          split
          · rw [explainArgs_result es Γ₁ Ts]; rfl
          · rfl
end

/-! ## Runs

The step table: one row per evaluated node, in execution order (a node's
premises run before the node itself, so the table reads top to bottom as the
machine ran). Each row carries the store before and after, the drop events
the node emitted, and what the node produced. -/

/-- (helper) The copy-closure monitor's premise, `ownedUnderCopy`
(`Dynamics.lean`), as one literal: a chain of `++` here costs the digest's
equation generator more than its heartbeat limit allows. -/
def ownedUnderCopyPremise : String :=
  "an owned value under a Copy node: a Copy type's fields, payloads and elements are Copy (§3, 3.8:18, 6.3:19), so a copy of this aggregate would duplicate an owner; the machine's copy-closure monitor refuses it (§7 “no double free”), and `soundness` proves a checked program never reaches it"

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
      "an operator met a wrong-shaped value, a call's argument count did not match " ++
      "its callee's parameter list, or a dynamic-index read, a dynamic-index @drop or " ++
      "a repeat met a non-Copy value ((D-Use-Untrackable-Dynamic-Copy) §6.3, 7.1:38); " ++
      "the statics (§5) exclude all three, and `soundness` (§7) is the proof"
  | .ownedUnderCopy => ownedUnderCopyPremise

/-- What one node produced: a value, a value an unwinding `return` handed
past it (§6.9), a defined trap (§6.12), a refusal (§6's stuck states) with
the premise it broke, or the interpreter's admission that it ran out of
fuel. -/
inductive StepRes where
  | value (v : Val)
  | unwound (v : Val)
  | breaking
  | panicked (k : PanicKind)
  | refuse (why : Violation) (premise : String)
  | exhausted

/-- (helper) How a node reports a sub-result it only passes on. -/
def StepRes.ofRes : EvalRes → StepRes
  | .ok _ v _ => .value v
  | .returned _ v _ => .unwound v
  | .broke _ _ _ => .breaking
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
the binding's drop never ran either (§6.7). An early `return` and a `break`
are the outcomes where the drop still runs, because `run-all-scope-drops`
(§6.9) and the loop's `unwind-drops` (§6.10) walk the frame's record instead;
a trap (§6.12) abandons the configuration and runs neither. -/
def scopeNeverClosed : String :=
  "(D-EndScope) §6.7 — the body did not complete, so this scope never closed " ++
  "(an early return or a break runs the drop through the frame's scope record " ++
  "instead; a trap runs no drop at all)"

/-- (helper) The label for a call whose callee did not complete because it
**trapped**. No frame is popped there: §6.2's (Panic-Lift) carries `↯κ` out
of every evaluation context, the suspended caller's included, so
`run-all-scope-drops` never runs and the callee's open scopes are abandoned
with the configuration (§6.12). Saying "(D-Return-Value) (pop the frame)"
here would name a rule that did not fire. -/
def trapLiftsPastCall : String :=
  "(Panic-Lift) §6.2 — the callee trapped, so no frame is popped: §6.12 " ++
  "abandons the configuration and `run-all-scope-drops` never runs"

/-- (helper) An aggregate's introduction ((D-Struct), (D-Array) §6.5,
(D-Enum-Intro) §6.6, the repeat form), mirroring `introVal`: the value's
identity is minted as the next store index, reserved with `†`, and the row
shows the value with it (`idTag`); an aggregate the copy-closure monitor
refuses is refused here too. -/
def tracedIntro (P : Program) (kids : List Step) (d : Nat) (Θ : List Ty) (R : Ty) (e : Expr)
    (rule : String) (H H₁ : Store) (tr : List Event) (mk : Nat → Val) : Trace :=
  if (Contents.ofVal (mk H₁.length)).copyClosed P.decls then
    traced kids d Θ R e (rule ++ " (mint " ++ idTag H₁.length ++ ")") H (H₁ ++ [.dead]) []
      (.value (mk H₁.length)) (.ok (H₁ ++ [.dead]) (mk H₁.length) tr)
  else refused kids d Θ R e rule H .ownedUnderCopy

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

/-- An aggregate's introduction row carries `introVal`'s result, with the
arguments' trace prefixed (helper). Private, so the statement digest does not
reach the renderers through it (`violationPremise`'s equations are more than
the digest's heartbeat limit allows). -/
private theorem tracedIntro_res (P : Program) (kids : List Step) (d : Nat) (Θ : List Ty)
    (R : Ty) (e : Expr) (rule : String) (H H₁ : Store) (tr : List Event) (mk : Nat → Val) :
    (tracedIntro P kids d Θ R e rule H H₁ tr mk).res = (introVal P.decls H₁ mk).withTrace tr := by
  unfold tracedIntro introVal
  split <;> simp [traced, refused, EvalRes.withTrace]

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
  | _ + 1, d, Θ, R, H, φ, .use pl =>
      match φ.env[pl.root]? with
      | none => refused [] d Θ R (.use pl) "(D-Use-Copy)/(D-Use-Move) §6.3" H .unbound
      | some ℓ =>
        match H[ℓ]? with
        | none => refused [] d Θ R (.use pl) "(D-Use-Copy)/(D-Use-Move) §6.3" H .unbound
        | some .dead =>
            refused [] d Θ R (.use pl) "(D-Use-Copy)/(D-Use-Move) §6.3" H .useAfterDrop
        | some (.full c) =>
          match c.declaredPlan P.decls pl.path with
          | some (πd, πs) =>
            (match c.readAt πd with
             | .error w => refused [] d Θ R (.use pl) "(D-Use-Declared-Linear) §6.3" H w
             | .ok cd =>
               match cd.destructure P.decls πs with
               | .error w => refused [] d Θ R (.use pl) "(D-Use-Declared-Linear) §6.3" H w
               | .ok (leaf, evs) =>
                 match leaf.toVal with
                 | none =>
                     refused [] d Θ R (.use pl) "(D-Use-Declared-Linear) §6.3" H .useAfterMove
                 | some v =>
                   match c.writeAt πd .hole with
                   | none =>
                       refused [] d Θ R (.use pl) "(D-Use-Declared-Linear) §6.3" H .typeConfusion
                   | some c' =>
                       traced [] d Θ R (.use pl) "(D-Use-Declared-Linear) §6.3" H
                         (H.set ℓ (.full c')) evs (.value v) (.ok (H.set ℓ (.full c')) v evs))
          | none =>
            match c.readAt pl.path with
            | .error w => refused [] d Θ R (.use pl) "(D-Use-Copy)/(D-Use-Move) §6.3" H w
            | .ok sub =>
              match sub.toVal with
              | none =>
                  refused [] d Θ R (.use pl) "(D-Use-Copy)/(D-Use-Move) §6.3" H .useAfterMove
              | some v =>
                  if v.mult P.decls = .copy then
                    traced [] d Θ R (.use pl) "(D-Use-Copy) §6.3" H H [] (.value v) (.ok H v [])
                  else
                    match c.writeAt pl.path .hole with
                    | none =>
                        refused [] d Θ R (.use pl) "(D-Use-Move) §6.3" H .typeConfusion
                    | some c' =>
                        traced [] d Θ R (.use pl) "(D-Use-Move) §6.3" H (H.set ℓ (.full c')) []
                          (.value v) (.ok (H.set ℓ (.full c')) v [])
  | _ + 1, d, Θ, R, H, φ, .drop pl =>
      match φ.env[pl.root]? with
      | none => refused [] d Θ R (.drop pl) "@drop §6.11" H .unbound
      | some ℓ =>
        match H[ℓ]? with
        | none => refused [] d Θ R (.drop pl) "@drop §6.11" H .unbound
        | some .dead => refused [] d Θ R (.drop pl) "@drop §6.11" H .useAfterDrop
        | some (.full c) =>
          match c.declaredPlan P.decls pl.path with
          | some (πd, πs) =>
            (match c.readAt πd with
             | .error w =>
                 refused [] d Θ R (.drop pl) "@drop §6.11 at a declared-linear plan (§6.3)" H w
             | .ok cd =>
               match cd.destructure P.decls πs with
               | .error w =>
                   refused [] d Θ R (.drop pl) "@drop §6.11 at a declared-linear plan (§6.3)" H w
               | .ok (leaf, evs) =>
                 if leaf.isHole then
                   refused [] d Θ R (.drop pl) "@drop §6.11 at a declared-linear plan (§6.3)"
                     H .useAfterMove
                 else
                 match dropCell P.decls ℓ leaf with
                 | .error w =>
                     refused [] d Θ R (.drop pl) "@drop §6.11 at a declared-linear plan (§6.3)" H w
                 | .ok levs =>
                   match c.writeAt πd .hole with
                   | none =>
                       refused [] d Θ R (.drop pl) "@drop §6.11 at a declared-linear plan (§6.3)"
                         H .typeConfusion
                   | some c' =>
                       traced [] d Θ R (.drop pl) "@drop §6.11 at a declared-linear plan (§6.3)"
                         H (H.set ℓ (.full c')) (evs ++ levs) (.value .unit)
                         (.ok (H.set ℓ (.full c')) .unit (evs ++ levs)))
          | none =>
            match c.readAt pl.path with
            | .error w => refused [] d Θ R (.drop pl) "@drop §6.11" H w
            | .ok sub =>
              if sub.isHole then refused [] d Θ R (.drop pl) "@drop §6.11" H .useAfterMove else
              (match dropCell P.decls ℓ sub with
               | .error w => refused [] d Θ R (.drop pl) "@drop §6.11" H w
               | .ok evs =>
                   if sub.mult P.decls = .copy then
                     traced [] d Θ R (.drop pl) "@drop §6.11 (Copy: no glue)" H H [] (.value .unit)
                       (.ok H .unit [])
                   else
                     match c.writeAt pl.path .hole with
                     | none => refused [] d Θ R (.drop pl) "@drop §6.11" H .typeConfusion
                     | some c' =>
                         traced [] d Θ R (.drop pl) "@drop §6.11" H (H.set ℓ (.full c'))
                           evs (.value .unit) (.ok (H.set ℓ (.full c')) .unit evs))
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
         match P.decls.structs[s]? with
         | none => refused ta.steps d Θ R (.mkStruct s args) "(D-Struct) §6.5" H .unbound
         | some sd =>
             if sd.fields.length = vs.length then
               tracedIntro P ta.steps d Θ R (.mkStruct s args) "(D-Struct) §6.5" H H₁ tr
                 (fun i => .struct s i vs)
             else refused ta.steps d Θ R (.mkStruct s args) "(D-Struct) §6.5" H .typeConfusion)
  | fuel + 1, d, Θ, R, H, φ, .mkEnum e k args =>
      let ta := traceArgs (fun H' e' => traceEval M P fuel (d + 1) Θ R H' φ e') H args
      (match ta.res with
       | .abort r => didNotRun ta.steps d Θ R (.mkEnum e k args) "(D-Enum-Intro) §6.6" H r
       | .ok H₁ vs tr =>
         match P.decls.enums[e]? with
         | none => refused ta.steps d Θ R (.mkEnum e k args) "(D-Enum-Intro) §6.6" H .unbound
         | some ed =>
           match ed.variants[k]? with
           | none =>
               refused ta.steps d Θ R (.mkEnum e k args) "(D-Enum-Intro) §6.6" H .typeConfusion
           | some Ts =>
               if Ts.length = vs.length then
                 tracedIntro P ta.steps d Θ R (.mkEnum e k args) "(D-Enum-Intro) §6.6" H H₁ tr
                   (fun i => .enum e k i vs)
               else
                 refused ta.steps d Θ R (.mkEnum e k args) "(D-Enum-Intro) §6.6" H
                   .typeConfusion)
  | fuel + 1, d, Θ, R, H, φ, .«match» scrut arms =>
      let t₀ := traceEval M P fuel (d + 1) Θ R H φ scrut
      (match t₀.res with
       | .ok H₀ v tr₀ =>
         (match v with
          | .enum e k _ vs =>
            (match arms[k]? with
             | none =>
                 refused t₀.steps d Θ R (.«match» scrut arms) "(D-Match) §6.6" H .typeConfusion
             | some body =>
               let minted := mintParams H₀ vs
               let bind := adminStep (d + 1) Θ R (.«match» scrut arms)
                 "(D-Match) §6.6 (bind the arm's payload)"
                 ("bind " ++ Print.tyName (.enum e) ++ "." ++ Print.variantName k ++
                   "'s payload to " ++ locsLine minted.2)
                 H₀ minted.1 [] (.value v)
               let t₁ := traceEval M P fuel (d + 1) ((vs.map valTy).reverse ++ Θ) R minted.1
                 { env := minted.2.reverse ++ φ.env, scope := φ.scope ++ minted.2 } body
               (match t₁.res with
                | .ok H₂ v₂ tr₂ =>
                  (match unwindLocs P.decls H₂ minted.2.reverse with
                   | .error w =>
                       refused (t₀.steps ++ [bind] ++ t₁.steps) d Θ R (.«match» scrut arms)
                         "(D-EndScope) §6.6 (end the arm)" H₂ w
                   | .ok (H₃, evs) =>
                       tracedAs (t₀.steps ++ [bind] ++ t₁.steps) d Θ R (.«match» scrut arms)
                         "(D-EndScope) §6.6 (end the arm)"
                         ("endscope(" ++ locsLine minted.2.reverse ++ ")")
                         H₂ H₃ evs (.value v₂) (.ok H₃ v₂ (tr₀ ++ (tr₂ ++ evs))))
                | r =>
                    didNotRun (t₀.steps ++ [bind] ++ t₁.steps) d Θ R (.«match» scrut arms)
                      scopeNeverClosed H (r.withTrace tr₀)))
          | _ => confused t₀.steps d Θ R (.«match» scrut arms) "(D-Match) §6.6" H)
       | r => propagate t₀.steps d Θ R (.«match» scrut arms) "(D-Match) §6.6" H r)
  | fuel + 1, d, Θ, R, H, φ, .mkArray T args =>
      let ta := traceArgs (fun H' e' => traceEval M P fuel (d + 1) Θ R H' φ e') H args
      (match ta.res with
       | .abort r => didNotRun ta.steps d Θ R (.mkArray T args) "(D-Array) §6.5" H r
       | .ok H₁ vs tr =>
           tracedIntro P ta.steps d Θ R (.mkArray T args) "(D-Array) §6.5" H H₁ tr
             (fun i => .array T i vs))
  | fuel + 1, d, Θ, R, H, φ, .repeatArray T e n =>
      let rule := "(D-Array) §6.5, through §2's repeat elaboration (7.1:39)"
      let t := traceEval M P fuel (d + 1) Θ R H φ e
      (match t.res with
       | .ok H₁ v tr =>
           if v.mult P.decls = .copy then
             tracedIntro P t.steps d Θ R (.repeatArray T e n) rule H H₁ tr
               (fun i => .array T i (List.replicate n v))
           else refused t.steps d Θ R (.repeatArray T e n) rule H .typeConfusion
       | r => propagate t.steps d Θ R (.repeatArray T e n) rule H r)
  | fuel + 1, d, Θ, R, H, φ, .indexRead pl idx πs =>
      let rule := "(D-Index) §6.5"
      let ta := traceArgs (fun H' e' => traceEval M P fuel (d + 1) Θ R H' φ e') H idx
      (match ta.res with
       | .abort r => didNotRun ta.steps d Θ R (.indexRead pl idx πs) rule H r
       | .ok H₁ vs tr =>
         match dynPlace H₁ φ pl vs πs with
         | .stuck w => refused ta.steps d Θ R (.indexRead pl idx πs) rule H w
         | .bounds =>
             traced ta.steps d Θ R (.indexRead pl idx πs)
               "(D-Index-Trap) §6.5 — the bounds trap of §6.12" H H₁ []
               (.panicked .bounds) (.panic .bounds tr)
         | .at _ _ sub ρ =>
           match sub.readAt ρ with
           | .error w => refused ta.steps d Θ R (.indexRead pl idx πs) rule H w
           | .ok leaf =>
             match leaf.toVal with
             | none => refused ta.steps d Θ R (.indexRead pl idx πs) rule H .useAfterMove
             | some v =>
                 if v.mult P.decls = .copy then
                   traced ta.steps d Θ R (.indexRead pl idx πs) rule H H₁ []
                     (.value v) (.ok H₁ v tr)
                 else refused ta.steps d Θ R (.indexRead pl idx πs) rule H .typeConfusion)
  | fuel + 1, d, Θ, R, H, φ, .indexDrop pl idx πs =>
      -- The read's rows, then the drop's own: a `Copy` leaf owes no glue, so
      -- the read's navigation and bounds trap are all the form does.
      let rule := "@drop §6.11 at a Copy place below a dynamic index"
      let t := traceEval M P fuel (d + 1) Θ R H φ (.indexRead pl idx πs)
      (match t.res with
       | .ok H₁ _ tr =>
           traced t.steps d Θ R (.indexDrop pl idx πs) rule H H₁ [] (.value .unit)
             (.ok H₁ .unit tr)
       | r => propagate t.steps d Θ R (.indexDrop pl idx πs) rule H r)
  | fuel + 1, d, Θ, R, H, φ, .indexWrite pl idx πs e =>
      -- `5.2:14`'s order: the right-hand side's rows come first, then the
      -- indices', then this node's own row.
      let rule := "(D-Assign) §6.8 below a dynamic index"
      let t₁ := traceEval M P fuel (d + 1) Θ R H φ e
      (match t₁.res with
       | .ok H₁ v tr₁ =>
         let ta := traceArgs (fun H' e' => traceEval M P fuel (d + 1) Θ R H' φ e') H₁ idx
         (match ta.res with
          | .ok H₂ vs tr₂ =>
            (match dynPlace H₂ φ pl vs πs with
             | .stuck w =>
                 refused (t₁.steps ++ ta.steps) d Θ R (.indexWrite pl idx πs e) rule H w
             | .bounds =>
                 traced (t₁.steps ++ ta.steps) d Θ R (.indexWrite pl idx πs e)
                   "(D-Index-Trap) §6.5 — the bounds trap of §6.12" H H₂ []
                   (.panicked .bounds) (.panic .bounds (tr₁ ++ tr₂))
             | .at ℓ c sub ρ =>
               match sub.readAt ρ with
               | .error w =>
                   refused (t₁.steps ++ ta.steps) d Θ R (.indexWrite pl idx πs e) rule H w
               | .ok old =>
                 if old.residualLinear P.decls then
                   refused (t₁.steps ++ ta.steps) d Θ R (.indexWrite pl idx πs e)
                     rule H .linearOverwrite
                 else
                   match dropCell P.decls ℓ old with
                   | .error w =>
                       refused (t₁.steps ++ ta.steps) d Θ R (.indexWrite pl idx πs e) rule H w
                   | .ok evs =>
                     match sub.writeAt ρ (Contents.ofVal v) with
                     | none =>
                         refused (t₁.steps ++ ta.steps) d Θ R (.indexWrite pl idx πs e)
                           rule H .typeConfusion
                     | some sub' =>
                       match c.writeAt pl.path sub' with
                       | none =>
                           refused (t₁.steps ++ ta.steps) d Θ R (.indexWrite pl idx πs e)
                             rule H .typeConfusion
                       | some c' =>
                         if c'.copyClosed P.decls then
                           traced (t₁.steps ++ ta.steps) d Θ R (.indexWrite pl idx πs e)
                             (if old.isHole then
                                rule ++ " (reinitialization, 3.8:55)"
                              else rule ++ " (overwrite-drop)")
                             H (H₂.set ℓ (.full c')) evs (.value .unit)
                             (.ok (H₂.set ℓ (.full c')) .unit (tr₁ ++ (tr₂ ++ evs)))
                         else
                           refused (t₁.steps ++ ta.steps) d Θ R (.indexWrite pl idx πs e)
                             rule H .ownedUnderCopy)
          | .abort r =>
              didNotRun (t₁.steps ++ ta.steps) d Θ R (.indexWrite pl idx πs e) rule H
                (r.withTrace tr₁))
       | r => propagate t₁.steps d Θ R (.indexWrite pl idx πs e) rule H r)
  | fuel + 1, d, Θ, R, H, φ, .letIn m e₁ e₂ =>
      let t₁ := traceEval M P fuel (d + 1) Θ R H φ e₁
      match t₁.res with
      | .ok H₁ v₁ tr₁ =>
          let bind := adminStep (d + 1) Θ R (.letIn m e₁ e₂) "(D-Let) §6.7 (mint the binding)"
            ("let " ++ (if m then "mut " else "") ++ Print.binderName Θ.length ++ " = " ++
              valLine v₁ ++ " at " ++ locName H₁.length)
            H₁ (H₁ ++ [.full (Contents.ofVal v₁)]) [] (.value v₁)
          let t₂ := traceEval M P fuel (d + 1) (valTy v₁ :: Θ) R
            (H₁ ++ [.full (Contents.ofVal v₁)])
            { env := H₁.length :: φ.env, scope := φ.scope ++ [H₁.length] } e₂
          (match t₂.res with
           | .ok H₂ v₂ tr₂ =>
             (match dropRetire P.decls H₂ H₁.length with
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
  | fuel + 1, d, Θ, R, H, φ, .assign pl e =>
      let t := traceEval M P fuel (d + 1) Θ R H φ e
      match t.res with
      | .ok H₁ v tr =>
          (match φ.env[pl.root]? with
           | none => refused t.steps d Θ R (.assign pl e) "(D-Assign) §6.8" H .unbound
           | some ℓ =>
             match H₁[ℓ]? with
             | none => refused t.steps d Θ R (.assign pl e) "(D-Assign) §6.8" H .unbound
             | some .dead =>
                 refused t.steps d Θ R (.assign pl e) "(D-Assign) §6.8" H .useAfterDrop
             | some (.full c) =>
               match c.readAt pl.path with
               | .error w => refused t.steps d Θ R (.assign pl e) "(D-Assign) §6.8" H w
               | .ok old =>
                   if old.residualLinear P.decls then
                     refused t.steps d Θ R (.assign pl e) "(D-Assign) §6.8" H .linearOverwrite
                   else
                     match dropCell P.decls ℓ old with
                     | .error w => refused t.steps d Θ R (.assign pl e) "(D-Assign) §6.8" H w
                     | .ok evs =>
                         match c.writeAt pl.path (Contents.ofVal v) with
                         | none =>
                             refused t.steps d Θ R (.assign pl e) "(D-Assign) §6.8" H
                               .typeConfusion
                         | some c' =>
                           if c'.copyClosed P.decls then
                             traced t.steps d Θ R (.assign pl e)
                               (if old.isHole then "(D-Assign) §6.8 (reinitialization, 3.8:55)"
                                else "(D-Assign) §6.8 (overwrite-drop)")
                               H (H₁.set ℓ (.full c')) evs (.value .unit)
                               (.ok (H₁.set ℓ (.full c')) .unit (tr ++ evs))
                           else
                             refused t.steps d Θ R (.assign pl e) "(D-Assign) §6.8" H
                               .ownedUnderCopy)
      | r => propagate t.steps d Θ R (.assign pl e) "(D-Assign) §6.8" H r
  | fuel + 1, d, Θ, R, H, φ, .seq e₁ e₂ =>
      let t₁ := traceEval M P fuel (d + 1) Θ R H φ e₁
      match t₁.res with
      | .ok H₁ v₁ tr₁ =>
          (match v₁.mult P.decls with
           | .linear => refused t₁.steps d Θ R (.seq e₁ e₂) "(D-Seq) §6.7" H .linearDiscard
           | .affine =>
               match dropContents P.decls (Contents.ofVal v₁) with
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
          (match runAllScopeDrops P.decls H₁ φ with
           | .error w =>
               refused t.steps d Θ R (.ret e) "(D-Return) §6.9 (unwind the frame)" H₁ w
           | .ok (H₂, evs) =>
               tracedAs t.steps d Θ R (.ret e) "(D-Return) §6.9 (unwind the frame)"
                 ("run-all-scope-drops(" ++ locsLine φ.scope.reverse ++ ")")
                 H₁ H₂ evs (.unwound v) (.returned H₂ v (tr ++ evs)))
      | r => propagate t.steps d Θ R (.ret e) "(D-Return) §6.9" H r
  | _ + 1, d, Θ, R, H, φ, .brk =>
      traced [] d Θ R .brk "(D-Break) §6.10" H H [] .breaking (.broke H φ.scope [])
  | fuel + 1, d, Θ, R, H, φ, .loop e =>
      let tb := traceEval M P fuel (d + 1) Θ R H φ e
      match tb.res with
      | .ok H₁ _ tr =>
          -- (D-Loop-Iter): the body became `()`, and the loop re-enters it.
          let tl := traceEval M P fuel (d + 1) Θ R H₁ φ (.loop e)
          traced (tb.steps ++ tl.steps) d Θ R (.loop e) "(D-Loop-Iter) §6.10 (re-enter the body)"
            H (lastStore (tb.steps ++ tl.steps) H₁) [] (StepRes.ofRes tl.res)
            (tl.res.withTrace tr)
      | .broke H₁ sc tr =>
          (match unwindLocs P.decls H₁ (sc.drop φ.scope.length).reverse with
           | .error w =>
               refused tb.steps d Θ R (.loop e) "(D-Break) §6.10 (unwind to the loop)" H₁ w
           | .ok (H₂, evs) =>
               tracedAs tb.steps d Θ R (.loop e) "(D-Break) §6.10 (unwind to the loop)"
                 ("unwind-drops(" ++ locsLine (sc.drop φ.scope.length).reverse ++ ")")
                 H₁ H₂ evs (.value .unit) (.ok H₂ .unit (tr ++ evs)))
      | r => propagate tb.steps d Θ R (.loop e) "(D-Loop-Iter) §6.10" H r
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
               (match runAllScopeDrops P.decls H₃ φg with
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
             | .broke _ _ _ =>
                 refused (ta.steps ++ [push] ++ tb.steps) d Θ R (.call f args)
                   "(D-Call) §6.9 — a `break` reached the call boundary" H .typeConfusion
             | r =>
                 didNotRun (ta.steps ++ [push] ++ tb.steps) d Θ R (.call f args)
                   (match r with
                    | .panic _ _ => trapLiftsPastCall
                    | _ => "(D-Return-Value) §6.9 (pop the frame)")
                   H (r.withTrace tr))
          else refused ta.steps d Θ R (.call f args) "(D-Call) §6.9" H .typeConfusion

-- The budget is per declaration, and the `use` and `drop` arms are where it
-- goes (about 2.5 s and 3.7 s of the whole, measured arm by arm): `eval`'s two
-- place arms gained the declared-linear redex (§6.3) on top of the array
-- forms. The default fails and 250000 passes; the proof is unchanged.
set_option maxHeartbeats 400000 in
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
      | use pl =>
          simp only [traceEval, eval]
          (repeat' split) <;> first | rfl | (simp_all [traced, refused] <;> grind)
      | drop pl =>
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
            first | rfl | (simp_all [traced, tracedIntro_res, didNotRun, refused,
              EvalRes.withTrace] <;> grind)
      | mkEnum e' k args =>
          simp only [traceEval, eval,
            traceArgs_res (ev := fun H' e'' => eval M fuel P H' φ e'') (fun H' e'' => ih _ _ _ _ _ e'')]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, tracedIntro_res, didNotRun, refused,
              EvalRes.withTrace] <;> grind)
      | «match» scrut arms =>
          simp only [traceEval, eval, EvalRes.andThen, ih]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, tracedAs, didNotRun, refused, confused,
              EvalRes.withTrace] <;> grind)
      | mkArray Te args =>
          simp only [traceEval, eval,
            traceArgs_res (ev := fun H' e' => eval M fuel P H' φ e') (fun H' e' => ih _ _ _ _ _ e')]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, tracedIntro_res, didNotRun, refused,
              EvalRes.withTrace] <;> grind)
      | repeatArray Te e₁ n =>
          simp only [traceEval, eval, EvalRes.andThen, ih]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, tracedIntro_res, didNotRun, refused,
              EvalRes.withTrace] <;> grind)
      | indexRead pl idx πs =>
          simp only [traceEval, eval,
            traceArgs_res (ev := fun H' e' => eval M fuel P H' φ e') (fun H' e' => ih _ _ _ _ _ e')]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, didNotRun, refused,
              EvalRes.withTrace] <;> grind)
      | indexDrop pl idx πs =>
          simp only [traceEval, eval, EvalRes.andThen, ih]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, EvalRes.withTrace] <;> grind)
      | indexWrite pl idx πs e₁ =>
          simp only [traceEval, eval, EvalRes.andThen, ih,
            traceArgs_res (ev := fun H' e' => eval M fuel P H' φ e') (fun H' e' => ih _ _ _ _ _ e')]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, refused, didNotRun,
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
      | brk => rfl
      | loop e₁ =>
          simp only [traceEval, eval, ih]
          (repeat' split) <;>
            first | rfl | (simp_all [traced, tracedAs, refused,
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
