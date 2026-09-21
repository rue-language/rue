import RueCore.Syntax

/-!
# RueCore.Print — core syntax to Rue source (the bridge's printer)

`crates/rue-oracle` interprets the compiler's CFG built from Rue source and
cannot consume core syntax, so the differential bridge (ADR-0097, RUE-2227)
runs the cheap direction: every fragment program is printed as a Rue program
whose surface forms elaborate back to the core forms it came from (§2's
elaboration inventory), with one documented exception (`drop` on a linear
binding, below), and the compiler, the oracle, and the native binary are
then run on that program.

## The observation channel

The interpreter's observable outcome is a value plus a drop trace
(`Dynamics.lean`, `Event`). A native Rue binary's observable outcome is its
stdout and exit status. The printed program makes the two agree by printing
one line per drop event and one line for the final value, through `@dbg`
(4.13:6, which borrows its argument and prints an integer as its decimal
digits). How a drop becomes a printed line depends on the multiplicity
class, because the spec constrains destructors per class (3.9) and the
interpreter emits no events for some classes at all:

* **`Copy` (`RCopy`).** A `@copy` type must not have a destructor (3.9,
  `09-destructors.md`), and the interpreter emits no event for a copy
  value's drop, `@drop`, overwrite, or discard. Nothing to print, nothing
  printed. `consume` is a plain field read.
* **`Affine` (`RAffine`).** The interpreter emits a drop event at scope
  exit, on `@drop`, on overwrite-drop, and when a temporary is discarded;
  each has no explicit site in Rue except `@drop`, so only a destructor can
  print them. `consume`, by contrast, emits no event, but a by-value
  consume leaves a husk whose destructor would print. Hence the `live`
  flag: the destructor prints `value` only while `live` holds, and
  `consume_affine` disarms the husk before it is dropped. A copy read of
  `value` through a destructor-bearing affine value is permitted (only a
  moving projection is rejected), so `consume_affine` is legal.
* **`Linear` (`RLinear`).** A whole-value destructor on a declared-linear
  type rejects every projection out of it (3.9:34, E0456), including the
  one `consume` needs, so `RLinear` has no destructor. The interpreter's
  only observable event for a linear value is an explicit `@drop`, since a
  leak, an overwrite, and a discard are refusals; so the image of `drop i`
  on a linear binding is `@dbg(consume_linear(x))`: the same ownership
  effect (`x` consumed, 3.8:33) and the same observation (one line). This
  is the one place the printed program is not the identity elaboration:
  its core image is `consume (use i)` plus an intrinsic, so the compiler's
  own `@drop`-discharges-a-linear path (3.9:39) is not exercised by the
  bridge for linear values.

## Integer typing

The fragment's `int` is `int(64, signed)`, but a Rue integer literal has no
type of its own: it unifies with its uses and, unconstrained at the end of
the function body, defaults to `i32` (4.1:3, 3.1:15). Most printed contexts
fix `i64` — a `let` binder's annotation, a resource's `value` field, an
assignment target, `main`'s `result` — but two do not, and both were found
by the generated corpus (RUE-2229): the operands of `<`, and an `int`
expression discarded by a sequence. So `lt e₁ e₂` prints as a call to the
prelude's `lt_i64`, whose parameters give both operands their type, and a
discarded `int` prints as `let t<n>: i64 = e₁;` rather than `e₁;`. Neither
changes evaluation order or a drop point (the operands and the discarded
value are integers, which drop silently); they are, with the linear `drop`
above, the places where the printed program is not the identity elaboration.

## Functions, calls, and `return`

A fragment *program* is a list of function definitions (`Syntax.lean`), and
it prints as one `fn f<i>(...) -> T { ... }` item per definition, in program
order, with parameter `j` named `v<j>` — the name the body's de Bruijn index
`m-1-j` resolves to, so a parameter and a `let` binder are spelled the same
way. `call f args` prints as `f<f>(a1, …, am)` and `ret e` as `return e`,
in whatever expression position the core form occupies: `let v0: i64 = (1 +
return 5); v0` is a program the compiler accepts, and it is the shape
(D-Return) §6.9 fires in "any evaluation context `E'`" for.

One §2 mark has no surface spelling: a by-value parameter may carry `μ = mut`
(§2's `F` production; §5.2 then lets the body assign to it), and Rue's
grammar has no `mut` parameter today (E0100). The printer prints the
parameter without the mark, so a case whose signature used one would print a
program the compiler rejects; the corpus and the generator declare
`mu := false` on every parameter, and the core keeps the mark because (Fn)
§5.8 and (Assign) §5.2 are stated with it.

Every printed program is a complete Rue module: a fixed prelude declaring
the three resource types and their consumers, then the program's functions,
then `main`, which binds the entry function's value by calling `f0()`, prints
it (through the class's consumer for a resource result, so a resource
returned by the program is observed as its payload rather than dropped at
`main`'s end), and returns 0. Traps (§6.12) end the process before the value
is printed; the bridge compares the trap kind.
-/

namespace RueCore

namespace Print

/-- The Rue type name of a core type (§2). `int` is `int(64, signed)` in the
fragment (`Syntax.lean`). -/
def tyName : Ty → String
  | .int => "i64"
  | .bool => "bool"
  | .unit => "()"
  | .res .copy => "RCopy"
  | .res .affine => "RAffine"
  | .res .linear => "RLinear"

/-- The consumer function for a resource class (see the module docstring)
(helper). -/
def consumeName : Mult → String
  | .copy => "consume_copy"
  | .affine => "consume_affine"
  | .linear => "consume_linear"

/-- The prelude every printed program starts with. It is the same text for
every case, so a reader learns it once (helper). -/
def prelude : String :=
  "// Resource types standing in for the calculus's abstract `res κ` (§2): one\n" ++
  "// integer payload, one multiplicity class each. The observation channel is\n" ++
  "// documented in docs/formal/lean/RueCore/Print.lean.\n" ++
  "@copy struct RCopy { value: i64 }\n" ++
  "struct RAffine { value: i64, live: bool }\n" ++
  "drop fn RAffine(self) { if self.live { @dbg(self.value); } }\n" ++
  "linear struct RLinear { value: i64 }\n" ++
  "fn consume_copy(r: RCopy) -> i64 { r.value }\n" ++
  "fn consume_affine(r: RAffine) -> i64 { let mut r = r; r.live = false; r.value }\n" ++
  "fn consume_linear(r: RLinear) -> i64 { r.value }\n" ++
  "// The core's `<` on int(64, signed): Rue literals default to i32 (4.1:3), so\n" ++
  "// the parameters fix the operand type.\n" ++
  "fn lt_i64(a: i64, b: i64) -> bool { a < b }\n"

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
  | .mkres κ _ => some (.res κ)
  | .consume _ => some .int
  | .drop _ => some .unit
  | .letIn _ e₁ e₂ => do
      let T₁ ← tyOf P R Γ e₁
      tyOf P R (T₁ :: Γ) e₂
  | .assign _ _ => some .unit
  | .seq _ e₂ => tyOf P R Γ e₂
  | .ite _ e₁ _ => tyOf P R Γ e₁
  | .call f _ => (P[f]?).map FnDef.ret
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

/-- The Rue struct literal for `mkres κ e` (§5.8 aggregate introduction). -/
def resLit (κ : Mult) (payload : String) : String :=
  match κ with
  | .copy => "RCopy { value: " ++ payload ++ " }"
  | .affine => "RAffine { value: " ++ payload ++ ", live: true }"
  | .linear => "RLinear { value: " ++ payload ++ " }"

/-- Print an expression. `Γ` is the binder environment (innermost first),
`P` the program a call's callee is looked up in, `R` the enclosing function's
return type, and `lvl` the indentation of the line the expression starts on.
Forms that Rue spells as statements (`let`, assignment, sequencing) become
blocks whose value is their tail expression, so the printed expression has the
same value and the same drop points as the core form: a `let` binder is
dropped at the close of its block (§6.7), a discarded operand at the end of
its statement (§6.7), an overwritten value at the assignment (§6.8). Integer
operands of `<` and discarded integers are typed explicitly (module docstring,
"Integer typing"). A `return` prints in place, wherever its core form stands
(§6.9's (D-Return) fires in any evaluation context). -/
partial def expr (P : Program) (R : Ty) (Γ : List Ty) (lvl : Nat) : Expr → String
  | .intLit n => if n < 0 then "(" ++ toString n ++ ")" else toString n
  | .boolLit b => if b then "true" else "false"
  | .unitLit => "()"
  | .use i => useName Γ i
  | .add e₁ e₂ => "(" ++ expr P R Γ lvl e₁ ++ " + " ++ expr P R Γ lvl e₂ ++ ")"
  | .div e₁ e₂ => "(" ++ expr P R Γ lvl e₁ ++ " / " ++ expr P R Γ lvl e₂ ++ ")"
  | .lt e₁ e₂ => "lt_i64(" ++ expr P R Γ lvl e₁ ++ ", " ++ expr P R Γ lvl e₂ ++ ")"
  | .mkres κ e => resLit κ (expr P R Γ lvl e)
  | .consume e =>
      let κ := match tyOf P R Γ e with
        | some (.res κ) => κ
        | _ => .affine   -- ill-typed input; the checker verdict says so
      consumeName κ ++ "(" ++ expr P R Γ lvl e ++ ")"
  | .drop i =>
      match Γ[i]? with
      | some (.res .linear) => "@dbg(consume_linear(" ++ useName Γ i ++ "))"
      | _ => "@drop(" ++ useName Γ i ++ ")"
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

/-- How `main` observes the program's value: an integer or boolean is
printed as is; a resource is consumed and its payload printed (the
interpreter reports the resource's payload as the value, and never drops a
returned value); `()` prints nothing (helper). -/
def observeValue (T : Ty) : String :=
  match T with
  | .int | .bool => "    @dbg(result);\n"
  | .unit => ""
  | .res κ => "    @dbg(" ++ consumeName κ ++ "(result));\n"

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
`main` calls the entry function `f0` and observes its value, so the program's
own functions print exactly as §2 writes them (helper). -/
def program (name description : String) (rules : List String) (outcome : String)
    (P : Program) : String :=
  let T := match P[0]? with
    | some fd => fd.ret
    | none => .int
  "// Case: " ++ name ++ "\n" ++
  "// " ++ description ++ "\n" ++
  "// Rules: " ++ String.intercalate "; " rules ++ "\n" ++
  "// Expected: " ++ outcome ++ "\n" ++
  "// Printed from the RueCore fragment by docs/formal/lean/RueCore/Print.lean.\n" ++
  prelude ++
  "\n" ++
  fnItems P 0 P ++
  "fn main() -> i32 {\n" ++
  "    let result: " ++ tyName T ++ " = " ++ fnName 0 ++ "();\n" ++
  observeValue T ++
  "    0\n" ++
  "}\n"

end Print

end RueCore
