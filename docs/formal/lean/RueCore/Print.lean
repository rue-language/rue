import RueCore.Syntax

/-!
# RueCore.Print — core syntax to Rue source (the bridge's printer)

`crates/rue-oracle` interprets the compiler's CFG built from Rue source and
cannot consume core syntax, so the differential bridge (ADR-0097, RUE-2227)
runs the cheap direction: every fragment program is printed as a Rue program
whose surface forms elaborate back to exactly the core forms it came from
(§2's elaboration inventory), and the compiler, the oracle, and the native
binary are then run on that program.

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
  effect (`x` consumed, 3.8:33) and the same observation (one line).

Every printed program is a complete Rue module: a fixed prelude declaring
the three resource types and their consumers, then `main`, which binds the
program's value, prints it (through the class's consumer for a resource
result, so a resource returned by the program is observed as its payload
rather than dropped at `main`'s end), and returns 0. Traps (§6.12) end the
process before the value is printed; the bridge compares the trap kind.
-/

namespace RueCore

namespace Print

/-- The Rue type name of a core type. `int` is `int(64, signed)` in the
fragment (`Syntax.lean`). -/
def tyName : Ty → String
  | .int => "i64"
  | .bool => "bool"
  | .unit => "()"
  | .res .copy => "RCopy"
  | .res .affine => "RAffine"
  | .res .linear => "RLinear"

/-- The consumer function for a resource class (see the module docstring). -/
def consumeName : Mult → String
  | .copy => "consume_copy"
  | .affine => "consume_affine"
  | .linear => "consume_linear"

/-- The prelude every printed program starts with. It is the same text for
every case, so a reader learns it once. -/
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
  "fn consume_linear(r: RLinear) -> i64 { r.value }\n"

/-- Type inference without ownership: the fragment's types do not depend on
Σ, so the printer can recover every subexpression's type from the binders
alone. `Γ` lists binder types innermost first, exactly as `Ctx` does. A
`none` means the program is ill-scoped, which elaborated programs never are. -/
def tyOf (Γ : List Ty) : Expr → Option Ty
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
      let T₁ ← tyOf Γ e₁
      tyOf (T₁ :: Γ) e₂
  | .assign _ _ => some .unit
  | .seq _ e₂ => tyOf Γ e₂
  | .ite _ e₁ _ => tyOf Γ e₁

/-- The binder introduced at nesting depth `d` is named `v<d>`; a de Bruijn
index `i` under `n` binders names the binder at depth `n - 1 - i`. -/
def binderName (depth : Nat) : String := "v" ++ toString depth

def useName (Γ : List Ty) (i : Nat) : String :=
  binderName (Γ.length - 1 - i)

def indent (n : Nat) : String := "".pushn ' ' (4 * n)

/-- The Rue struct literal for `mkres κ e` (§5.8 aggregate introduction). -/
def resLit (κ : Mult) (payload : String) : String :=
  match κ with
  | .copy => "RCopy { value: " ++ payload ++ " }"
  | .affine => "RAffine { value: " ++ payload ++ ", live: true }"
  | .linear => "RLinear { value: " ++ payload ++ " }"

/-- Print an expression. `Γ` is the binder environment (innermost first)
and `lvl` the indentation of the line the expression starts on. Forms that
Rue spells as statements (`let`, assignment, sequencing) become blocks whose
value is their tail expression, so the printed expression has the same value
and the same drop points as the core form: a `let` binder is dropped at the
close of its block (§6.7), a discarded operand at the end of its statement
(§6.7), an overwritten value at the assignment (§6.8). -/
partial def expr (Γ : List Ty) (lvl : Nat) : Expr → String
  | .intLit n => if n < 0 then "(" ++ toString n ++ ")" else toString n
  | .boolLit b => if b then "true" else "false"
  | .unitLit => "()"
  | .use i => useName Γ i
  | .add e₁ e₂ => "(" ++ expr Γ lvl e₁ ++ " + " ++ expr Γ lvl e₂ ++ ")"
  | .div e₁ e₂ => "(" ++ expr Γ lvl e₁ ++ " / " ++ expr Γ lvl e₂ ++ ")"
  | .lt e₁ e₂ => "(" ++ expr Γ lvl e₁ ++ " < " ++ expr Γ lvl e₂ ++ ")"
  | .mkres κ e => resLit κ (expr Γ lvl e)
  | .consume e =>
      let κ := match tyOf Γ e with
        | some (.res κ) => κ
        | _ => .affine   -- ill-typed input; the checker verdict says so
      consumeName κ ++ "(" ++ expr Γ lvl e ++ ")"
  | .drop i =>
      match Γ[i]? with
      | some (.res .linear) => "@dbg(consume_linear(" ++ useName Γ i ++ "))"
      | _ => "@drop(" ++ useName Γ i ++ ")"
  | .letIn m e₁ e₂ =>
      let T₁ := (tyOf Γ e₁).getD .int
      let name := binderName Γ.length
      "{\n" ++
      indent (lvl + 1) ++ "let " ++ (if m then "mut " else "") ++ name ++
        ": " ++ tyName T₁ ++ " = " ++ expr Γ (lvl + 1) e₁ ++ ";\n" ++
      indent (lvl + 1) ++ expr (T₁ :: Γ) (lvl + 1) e₂ ++ "\n" ++
      indent lvl ++ "}"
  | .assign i e =>
      "{ " ++ useName Γ i ++ " = " ++ expr Γ lvl e ++ "; }"
  | .seq e₁ e₂ =>
      "{\n" ++
      indent (lvl + 1) ++ expr Γ (lvl + 1) e₁ ++ ";\n" ++
      indent (lvl + 1) ++ expr Γ (lvl + 1) e₂ ++ "\n" ++
      indent lvl ++ "}"
  | .ite c e₁ e₂ =>
      "if " ++ expr Γ lvl c ++ " {\n" ++
      indent (lvl + 1) ++ expr Γ (lvl + 1) e₁ ++ "\n" ++
      indent lvl ++ "} else {\n" ++
      indent (lvl + 1) ++ expr Γ (lvl + 1) e₂ ++ "\n" ++
      indent lvl ++ "}"

/-- How `main` observes the program's value: an integer or boolean is
printed as is; a resource is consumed and its payload printed (the
interpreter reports the resource's payload as the value, and never drops a
returned value); `()` prints nothing. -/
def observeValue (T : Ty) : String :=
  match T with
  | .int | .bool => "    @dbg(result);\n"
  | .unit => ""
  | .res κ => "    @dbg(" ++ consumeName κ ++ "(result));\n"

/-- A complete Rue program for a closed core expression, headed by a comment
naming the case and what a reader should expect (the explainability tenet:
`corpus.json` doubles as a readable example set). -/
def program (name description outcome : String) (e : Expr) : String :=
  let T := (tyOf [] e).getD .int
  "// Case: " ++ name ++ "\n" ++
  "// " ++ description ++ "\n" ++
  "// Expected: " ++ outcome ++ "\n" ++
  "// Printed from the RueCore fragment by docs/formal/lean/RueCore/Print.lean.\n" ++
  prelude ++
  "\n" ++
  "fn main() -> i32 {\n" ++
  "    let result: " ++ tyName T ++ " = " ++ expr [] 1 e ++ ";\n" ++
  observeValue T ++
  "    0\n" ++
  "}\n"

end Print

end RueCore
