import RueCore.Syntax

/-!
# RueCore.Print — core syntax to Rue source (the bridge's printer)

`crates/rue-oracle` interprets the compiler's CFG built from Rue source and
cannot consume core syntax, so the differential bridge (ADR-0097, RUE-2227)
runs the cheap direction: every fragment program is printed as a Rue program
whose surface forms elaborate back to the core forms it came from (§2's
elaboration inventory), and the compiler, the oracle, and the native binary
are then run on that program.

A fragment program carries its own struct declarations (`Syntax.lean`), so the
printed module declares them: one `@copy struct` / `struct` / `linear struct`
item per declaration, in program order, with fields named `x0 … xk` by
position — elaboration resolves field names, so the core names fields by their
declaration slot (`3.6:9`).

## The observation channel

The interpreter's observable outcome is a value plus a drop trace
(`Dynamics.lean`, `Event`). A native Rue binary's observable outcome is its
stdout and exit status. **A user destructor is what makes the two agree**: a
Rue program has no other way to see a drop happen, so a declaration that says
`dtor` prints `drop fn S(self) { @dbg(self.x0); }`, one line per drop of a
value of that type, and the interpreter records the same drop as a `dtor`
event (§6.11 runs the destructor before the fields, and the compiler agrees —
verified by hand on a nested pair of destructor-bearing structs). A
declaration with no destructor drops silently in both.

The rest follows from the spec's constraints on destructors per class
(`3.9`):

* **`@copy`.** A `@copy` type must not have a destructor (`3.9:31`,
  `09-destructors.md`); `WfStructs` (`Statics.lean`) enforces it, so a copy
  value's drop prints nothing and the interpreter emits no `dtor` event for
  it.
* **Whole-value elimination.** `3.9:34` (E0456) rejects every projection out
  of a value whose type declares a destructor, so `Expr.consume` — which
  reads the first field — is defined only on a declaration with no destructor
  (`StructDecl.Consumable`). Such a declaration prints a consumer
  `fn consume_S(s: S) -> i64 { s.x0 }`; a copy field read through a by-value
  parameter is legal at every class, the declared-linear one included.
* **`@drop`.** Every class prints `@drop(x)`, the identity elaboration of
  `Expr.drop`: it is legal on a declared-linear place and on a place whose
  type is linear through a field (`3.9:39`, verified against the compiler),
  and it runs the same glue scope exit would, so the destructor lines it
  produces are the interpreter's `dtor` events in the same order.

## Integer typing

The fragment's `int` is `int(64, signed)`, but a Rue integer literal has no
type of its own: it unifies with its uses and, unconstrained at the end of the
function body, defaults to `i32` (4.1:3, 3.1:15). Most printed contexts fix
`i64` — a `let` binder's annotation, an `i64` field's initializer, an
assignment target, `main`'s `result` — but two do not, and both were found by
the generated corpus (RUE-2229): the operands of `<`, and an `int` expression
discarded by a sequence. So `lt e₁ e₂` prints as a call to the prelude's
`lt_i64`, whose parameters give both operands their type, and a discarded
`int` prints as `let t<n>: i64 = e₁;` rather than `e₁;`. Neither changes
evaluation order or a drop point (the operands and the discarded value are
integers, which drop silently); they are the two places where the printed
program is not the identity elaboration.

## Functions, calls, and `return`

A fragment *program* is a struct environment and a list of function
definitions (`Syntax.lean`); the functions print as one `fn f<i>(...) -> T
{ ... }` item per definition, in program order, with parameter `j` named
`v<j>` — the name the body's de Bruijn index `m-1-j` resolves to, so a
parameter and a `let` binder are spelled the same way. `call f args` prints as
`f<f>(a1, …, am)` and `ret e` as `return e`, in whatever expression position
the core form occupies: `let v0: i64 = (1 + return 5); v0` is a program the
compiler accepts, and it is the shape (D-Return) §6.9 fires in "any evaluation
context `E'`" for.

One §2 mark has no surface spelling: a by-value parameter may carry `μ = mut`
(§2's `F` production; §5.2 then lets the body assign to it), and Rue's grammar
has no `mut` parameter today (E0100). The printer prints the parameter without
the mark, so a case whose signature used one would print a program the
compiler rejects; the corpus and the generator declare `mu := false` on every
parameter, and the core keeps the mark because (Fn) §5.8 and (Assign) §5.2 are
stated with it.

## How `main` observes the program's value

`main` binds the entry function's value and then lets §6.11 speak: a scalar is
printed with `@dbg`; `()` prints nothing; a struct value is **dropped**, which
is what the interpreter's own value line is projected from (`Corpus.lean`'s
`valueLines` reads the lines that value's drop would emit). A struct whose
class is `Linear` cannot be dropped implicitly, so `main` discharges it with
`@drop(result)` — the same glue, made explicit (§5.3). Traps (§6.12) end the
process before any of this; the bridge compares the trap kind.
-/

namespace RueCore

namespace Print

/-- The Rue type name of a core type (§2). `int` is `int(64, signed)` in the
fragment (`Syntax.lean`); a struct type is named by its declaration's index,
the way elaboration resolves the surface name. -/
def tyName : Ty → String
  | .int => "i64"
  | .bool => "bool"
  | .unit => "()"
  | .struct s => "S" ++ toString s

/-- The name of a declaration's field at position `j` (`3.6:9`: the stored
value places each field in its declaration slot) (helper). -/
def fieldName (j : Nat) : String := "x" ++ toString j

/-- The generated whole-value eliminator for a `Consumable` declaration: it
reads the first field, which is the payload `Expr.consume` yields (helper). -/
def consumeName (s : Nat) : String := "consume_S" ++ toString s

/-- The prelude every printed program starts with: the one helper the core's
`<` needs, since a Rue literal defaults to `i32` (module docstring) (helper). -/
def prelude : String :=
  "// The core's `<` on int(64, signed): Rue literals default to i32 (4.1:3), so\n" ++
  "// the parameters fix the operand type.\n" ++
  "fn lt_i64(a: i64, b: i64) -> bool { a < b }\n"

/-- The declared attribute, as §3 and the surface grammar write it
(helper). -/
def attrPrefix : Attr → String
  | .none => ""
  | .copy => "@copy "
  | .linear => "linear "

/-- One field of a declaration, `x<j>: T` (helper). -/
def fieldDecls : Nat → List Ty → List String
  | _, [] => []
  | j, T :: rest => (fieldName j ++ ": " ++ tyName T) :: fieldDecls (j + 1) rest

/-- One struct declaration as a Rue item (§2's `S { f1: T1, …, fk: Tk }` with
its `3.8:18`/`3.8:57` attribute), followed by its `drop fn` when the
declaration has a destructor (`3.9`). The destructor prints the first field
when that field is an `int`, which is the observation channel the module
docstring describes; a declaration whose first field is not an `int` has
nothing to print, and the interpreter's `dtor` event for it is likewise
silent. -/
def structItem (s : Nat) (sd : StructDecl) : String :=
  attrPrefix sd.attr ++ "struct " ++ tyName (.struct s) ++ " { " ++
    String.intercalate ", " (fieldDecls 0 sd.fields) ++ " }\n" ++
  (if sd.dtor then
    "drop fn " ++ tyName (.struct s) ++ "(self) { " ++
      (match sd.fields with
       | .int :: _ => "@dbg(self." ++ fieldName 0 ++ "); "
       | _ => "") ++ "}\n"
   else "") ++
  (if sd.Consumable then
    "fn " ++ consumeName s ++ "(s: " ++ tyName (.struct s) ++ ") -> i64 { s." ++
      fieldName 0 ++ " }\n"
   else "")

/-- Every struct declaration of a program, in program order (helper). -/
def structItems : Nat → StructEnv → String
  | _, [] => ""
  | s, sd :: rest => structItem s sd ++ structItems (s + 1) rest

/-- Type inference without ownership: the fragment's types do not depend on
Σ, so the printer can recover every subexpression's type from the binders
alone. `Γ` lists binder types innermost first, exactly as `Ctx` does; `P` is
the program a call's callee is looked up in and `R` the enclosing function's
return type, which is the type `Checker.lean` gives a `return`. A `none`
means the program is ill-scoped, which elaborated programs never are
(helper). -/
def tyOf (P : Program) (R : Ty) (Γ : List Ty) : Expr → Option Ty
  | .intLit _ => some .int
  | .boolLit _ => some .bool
  | .unitLit => some .unit
  | .use i => Γ[i]?
  | .add _ _ => some .int
  | .div _ _ => some .int
  | .lt _ _ => some .bool
  | .mkStruct s _ => some (.struct s)
  | .consume _ => some .int
  | .drop _ => some .unit
  | .letIn _ e₁ e₂ => do
      let T₁ ← tyOf P R Γ e₁
      tyOf P R (T₁ :: Γ) e₂
  | .assign _ _ => some .unit
  | .seq _ e₂ => tyOf P R Γ e₂
  | .ite _ e₁ _ => tyOf P R Γ e₁
  | .call f _ => (P.fns[f]?).map FnDef.ret
  | .ret _ => some R

/-- The binder introduced at nesting depth `d` is named `v<d>`; a de Bruijn
index `i` under `n` binders names the binder at depth `n - 1 - i` (helper). -/
def binderName (depth : Nat) : String := "v" ++ toString depth

/-- The function at index `i` of the program is named `f<i>`; elaboration
resolves a surface name to the index, as it does for a binding (helper). -/
def fnName (i : Nat) : String := "f" ++ toString i

/-- The name of the binder a de Bruijn index refers to (helper). -/
def useName (Γ : List Ty) (i : Nat) : String :=
  binderName (Γ.length - 1 - i)

/-- Four spaces per nesting level (helper). -/
def indent (n : Nat) : String := "".pushn ' ' (4 * n)

/-- The field initializers of a struct literal, named by position
(`3.6:15`: a surface literal written out of order is presented here in
declaration order) (helper). -/
def fieldInits : Nat → List String → List String
  | _, [] => []
  | j, a :: rest => (fieldName j ++ ": " ++ a) :: fieldInits (j + 1) rest

/-- Print an expression. `Γ` is the binder environment (innermost first),
`P` the program a call's callee and a literal's declaration are looked up in,
`R` the enclosing function's return type, and `lvl` the indentation of the
line the expression starts on. Forms that Rue spells as statements (`let`,
assignment, sequencing) become blocks whose value is their tail expression, so
the printed expression has the same value and the same drop points as the core
form: a `let` binder is dropped at the close of its block (§6.7), a discarded
operand at the end of its statement (§6.7), an overwritten value at the
assignment (§6.8). Integer operands of `<` and discarded integers are typed
explicitly (module docstring, "Integer typing"). A `return` prints in place,
wherever its core form stands (§6.9's (D-Return) fires in any evaluation
context). -/
partial def expr (P : Program) (R : Ty) (Γ : List Ty) (lvl : Nat) : Expr → String
  | .intLit n => if n < 0 then "(" ++ toString n ++ ")" else toString n
  | .boolLit b => if b then "true" else "false"
  | .unitLit => "()"
  | .use i => useName Γ i
  | .add e₁ e₂ => "(" ++ expr P R Γ lvl e₁ ++ " + " ++ expr P R Γ lvl e₂ ++ ")"
  | .div e₁ e₂ => "(" ++ expr P R Γ lvl e₁ ++ " / " ++ expr P R Γ lvl e₂ ++ ")"
  | .lt e₁ e₂ => "lt_i64(" ++ expr P R Γ lvl e₁ ++ ", " ++ expr P R Γ lvl e₂ ++ ")"
  | .mkStruct s args =>
      tyName (.struct s) ++ " { " ++
        String.intercalate ", " (fieldInits 0 (args.map (fun a => expr P R Γ lvl a))) ++ " }"
  | .consume e =>
      let s := match tyOf P R Γ e with
        | some (.struct s) => s
        | _ => 0   -- ill-typed input; the checker verdict says so
      consumeName s ++ "(" ++ expr P R Γ lvl e ++ ")"
  | .drop i => "@drop(" ++ useName Γ i ++ ")"
  | .letIn m e₁ e₂ =>
      let T₁ := (tyOf P R Γ e₁).getD .int
      let name := binderName Γ.length
      "{\n" ++
      indent (lvl + 1) ++ "let " ++ (if m then "mut " else "") ++ name ++
        ": " ++ tyName T₁ ++ " = " ++ expr P R Γ (lvl + 1) e₁ ++ ";\n" ++
      indent (lvl + 1) ++ expr P R (T₁ :: Γ) (lvl + 1) e₂ ++ "\n" ++
      indent lvl ++ "}"
  | .assign i e =>
      "{ " ++ useName Γ i ++ " = " ++ expr P R Γ lvl e ++ "; }"
  | .seq e₁ e₂ =>
      -- A discarded int must still be typed i64 (module docstring).
      let discard := match tyOf P R Γ e₁ with
        | some .int => "let t" ++ toString lvl ++ ": i64 = " ++ expr P R Γ (lvl + 1) e₁ ++ ";"
        | _ => expr P R Γ (lvl + 1) e₁ ++ ";"
      "{\n" ++
      indent (lvl + 1) ++ discard ++ "\n" ++
      indent (lvl + 1) ++ expr P R Γ (lvl + 1) e₂ ++ "\n" ++
      indent lvl ++ "}"
  | .ite c e₁ e₂ =>
      "if " ++ expr P R Γ lvl c ++ " {\n" ++
      indent (lvl + 1) ++ expr P R Γ (lvl + 1) e₁ ++ "\n" ++
      indent lvl ++ "} else {\n" ++
      indent (lvl + 1) ++ expr P R Γ (lvl + 1) e₂ ++ "\n" ++
      indent lvl ++ "}"
  | .call f args =>
      fnName f ++ "(" ++
        String.intercalate ", " (args.map (fun a => expr P R Γ lvl a)) ++ ")"
  | .ret e => "return " ++ expr P R Γ lvl e

/-- How `main` observes the program's value (module docstring): a scalar is
printed; `()` prints nothing; a struct value is dropped — implicitly at
`main`'s end, or, when its class is `Linear` and an implicit drop would be
§5.6's leak, by an explicit `@drop`, which runs the same glue (§5.3)
(helper). -/
def observeValue (D : StructEnv) (T : Ty) : String :=
  match T with
  | .int | .bool => "    @dbg(result);\n"
  | .unit => ""
  | .struct _ => if T.mult D = .linear then "    @drop(result);\n" else ""

/-- One parameter per line of a signature, named the way the body's de Bruijn
indices resolve: the first parameter is the outermost binder, so it is `v0`.
The `μ = mut` mark is not printed (module docstring) (helper). -/
def paramList : Nat → List Param → List String
  | _, [] => []
  | i, p :: rest => (binderName i ++ ": " ++ tyName p.ty) :: paramList (i + 1) rest

/-- The binder environment a function body starts in: its parameter types,
innermost binder first, exactly as (Fn) §5.8's `fnCtx` orders them
(helper). -/
def bodyBinders (fd : FnDef) : List Ty := (fd.params.map Param.ty).reverse

/-- One `fn` item: §2's `F` production for a by-value signature. -/
def fnItem (P : Program) (idx : Nat) (fd : FnDef) : String :=
  "fn " ++ fnName idx ++ "(" ++ String.intercalate ", " (paramList 0 fd.params) ++
    ") -> " ++ tyName fd.ret ++ " {\n" ++
  indent 1 ++ expr P fd.ret (bodyBinders fd) 1 fd.body ++ "\n" ++
  "}\n"

/-- Every `fn` item of a program, in program order (helper). -/
def fnItems (P : Program) : Nat → List FnDef → String
  | _, [] => ""
  | i, fd :: rest => fnItem P i fd ++ "\n" ++ fnItems P (i + 1) rest

/-- A complete Rue module for a fragment program, headed by a comment naming
the case, the calculus rules it exercises, and what a reader should expect
(the explainability tenet: `corpus.json` doubles as a readable example set).
The program's own struct declarations come first, then the prelude, then its
functions as §2 writes them, then `main`, which calls the entry function `f0`
and observes its value (helper). -/
def program (name description : String) (rules : List String) (outcome : String)
    (P : Program) : String :=
  let T := match P.fns[0]? with
    | some fd => fd.ret
    | none => .int
  "// Case: " ++ name ++ "\n" ++
  "// " ++ description ++ "\n" ++
  "// Rules: " ++ String.intercalate "; " rules ++ "\n" ++
  "// Expected: " ++ outcome ++ "\n" ++
  "// Printed from the RueCore fragment by docs/formal/lean/RueCore/Print.lean.\n" ++
  "// The struct declarations are the program's own (§2); a `drop fn` is how a\n" ++
  "// drop becomes observable (§6.11).\n" ++
  structItems 0 P.structs ++
  prelude ++
  "\n" ++
  fnItems P 0 P.fns ++
  "fn main() -> i32 {\n" ++
  "    let result: " ++ tyName T ++ " = " ++ fnName 0 ++ "();\n" ++
  observeValue P.structs T ++
  "    0\n" ++
  "}\n"

end Print

end RueCore
