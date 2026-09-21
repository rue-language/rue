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
declaration slot. Declaration order is what `3.9:13` drops them in; `3.6:9`,
which says the stored value places each field in its declaration slot, is
informative and about layout, so nothing here rests on it.

## The observation channel

The interpreter's observable outcome is a value plus a trace
(`Dynamics.lean`, `Event`). A native Rue binary's observable outcome is its
stdout and exit status. Two kinds of event bridge them, and they share one
trace, so a line of either kind comes out where it happened.

**A user destructor is what makes a *drop* observable**: a Rue program has no
other way to see a drop happen, so a declaration that says `dtor` prints
`drop fn S(self) { @dbg(self.x0); }`, one line per drop of a value of that
type, and the interpreter records the same drop as a `dtor` event (§6.11 runs
the destructor before the fields, and the compiler agrees — verified by hand
on a nested pair of destructor-bearing structs). A declaration with no
destructor drops silently in both. **`@dbg` is the other kind**, and it needs
no bridging at all: the core form prints as itself and the interpreter emits
a `dbg` event (§6.12's observable output).

The rest follows from the spec's constraints on destructors per class
(`3.9`):

* **`@copy`.** A `@copy` type must not have a destructor (`3.9:31`,
  `09-destructors.md`); `WfStructs` (`Statics.lean`) enforces it, so a copy
  value's drop prints nothing and the interpreter emits no `dtor` event for
  it.
* **Whole-value elimination.** `Expr.consume` reads the first field and
  destroys the value **without running its drop glue**, so it is defined only
  on a declaration with no destructor (`StructDecl.Consumable`): a destructor
  there would be an event §6.11 owes and the interpreter never emits, while
  the printed consumer below lets its by-value parameter drop at the
  function's end, where that destructor *would* run — a two-line
  disagreement. That, not `3.9:34`, is the reason. `3.9:34` forbids *moving*
  a field out of such a value and permits borrowing one; the compiler accepts
  `fn consume_S(s: S) -> i64 { s.x0 }` on a destructor-bearing `S`.

  Such a declaration prints that consumer, and the compiler accepts it at
  every class — but for different reasons, and the declared-linear one is
  worth naming. `Consumable` makes every field an integer type, so `s.x0` is
  a `Copy` read: on a `@copy` or attribute-less struct nothing is consumed by it and
  the parameter simply drops at the function's end. On a **declared-linear**
  one the parameter carries a must-consume obligation (`3.8:62`), and the
  rule that a field access discharges it is `3.8:33`, the declared-linear
  destructure: the access consumes the smallest enclosing declared-linear
  place and destroys its droppable residue. That is what keeps `consume_S2`
  from leaking at its own exit, and what makes `Expr.consume` a faithful
  stand-in there. One fine point the spec does not spell out and this printer
  leans on: `3.8:33` is written for *moving* a field out, and a `Copy` field
  read is not a move — the compiler accepts `fn consume_S2(s: S2) -> i64
  { s.x0 }` all the same (verified by hand). `3.8:22`'s partial move is the
  rule for a **non-`Copy`** field, which `Consumable` excludes, so it governs
  nothing the printer emits.
* **`@drop`.** Every class prints `@drop(x)`, the identity elaboration of
  `Expr.drop`: it is legal on a declared-linear place and on a place whose
  type is linear through a field (`3.9:39`, verified against the compiler),
  and it runs the same glue scope exit would, so the destructor lines it
  produces are the interpreter's `dtor` events in the same order.

## Integer typing

A core integer expression carries its `int(w, s)` (`Syntax.lean`), but a Rue
integer literal has no type of its own: it unifies with its uses and,
unconstrained at the end of the function body, defaults to `i32` (4.1:3,
3.1:15). Most printed contexts fix the type — a `let` binder's annotation, a
field's initializer, an assignment target, a parameter, a declared return type,
`main`'s `result` — but four do not, because nothing downstream of them
mentions the operand type at all:

* the operands of an ordering compare, whose result is `bool`;
* the operand and the result of `@intCast`, whose target type `4.13:26` takes
  from the *use* site;
* the operand of `@dbg`, which renders whatever it is given;
* an integer expression discarded by a sequence.

Each of those prints as a **typed block**: `{ let t<n>a: T = e₁; let t<n>b: T =
e₂; t<n>a < t<n>b }` and its three siblings, where the annotation is the type
the core form carries. A block binds `Copy` scalars only, so it changes no
evaluation order and adds no drop point (§6.7 drops nothing for an integer).
The synthetic names are `t<level><tag>`, distinct from the `v<depth>` a core
binder gets, and Rue's surface permits shadowing in any case (`3.8:12`).

Together with two forms the printer *supplies*, those four are the places
where the printed program is not the identity elaboration of the core:
`Expr.consume` prints as a call to the generated `consume_S<s>` helper, which
is one by-value call standing in for a form the core has no surface spelling
for; and a destructor-bearing declaration prints an invented body
`drop fn S(self) { @dbg(self.x0); }`, which the core declaration does not
carry — the core records only *whether* `S` has a destructor, and that body is
the whole observation channel (above).

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

/-- The Rue type name of a core type (§2): `int(w, s)` is `i<w>` or `u<w>`,
and a struct type is named by its declaration's index, the way elaboration
resolves the surface name. -/
def tyName : Ty → String
  | .int w s => (match s with | .signed => "i" | .unsigned => "u") ++ toString w.bits
  | .float w => "f" ++ toString w.bits
  | .bool => "bool"
  | .unit => "()"
  | .struct s => "S" ++ toString s

/-- The Rue spelling of a binary operator (§2's `⊕` and `⋚`) (helper). -/
def binOpSym : BinOp → String
  | .add => "+"
  | .sub => "-"
  | .mul => "*"
  | .div => "/"
  | .rem => "%"
  | .bitAnd => "&"
  | .bitOr => "|"
  | .bitXor => "^"
  | .shl => "<<"
  | .shr => ">>"
  | .lt => "<"
  | .le => "<="
  | .gt => ">"
  | .ge => ">="
  -- `@total_cmp` is a `BinOp` in the core (`Syntax.lean`) but an *intrinsic*
  -- on the surface, so it is printed by `Print.expr`'s own arm and this
  -- spelling is never used for it.
  | .totalCmp => "@total_cmp"

/-- The Rue spelling of a unary operator (§2's `⊖`) (helper). -/
def unOpSym : UnOp → String
  | .neg => "-"
  | .not => "!"
  | .bitnot => "~"

/-- A Rue string literal, for `@panic`'s message. The fragment's messages are
plain words, and the two characters Rue's grammar would read specially are
escaped so that no message can close the literal early (helper). -/
def quoted (msg : String) : String :=
  "\"" ++ msg.foldl (fun acc c =>
    acc ++ (if c = '"' then "\\\"" else if c = '\\' then "\\\\" else String.singleton c)) ""
    ++ "\""

/-- The Rue spelling of a one-operand float intrinsic (§2's `@f`) (helper). -/
def fintrinName : FloatIntrin → String
  | .intToFloat _ => "@int_to_float"
  | .floatToInt _ _ => "@float_to_int"
  | .floatCast _ => "@float_cast"
  | .roundOp .sqrt => "@sqrt"
  | .roundOp (.round .floor) => "@floor"
  | .roundOp (.round .ceil) => "@ceil"
  | .roundOp (.round .trunc) => "@trunc"
  | .roundOp (.round .round) => "@round"

/-- A synthetic binder the printer introduces to give an expression the type
its core form carries (module docstring, "Integer typing"). `lvl` is the
nesting level and `tag` distinguishes the binders of one block (helper). -/
def tmpName (lvl : Nat) (tag : String) : String := "t" ++ toString lvl ++ tag

/-- The name of a declaration's field at position `j`. Fields are named by
position because the core names them that way; `3.6:9` — informative, and
about layout — says the stored value places each field in its declaration
slot, which is why the position is a stable name for it (helper). -/
def fieldName (j : Nat) : String := "x" ++ toString j

/-- The generated whole-value eliminator for a `Consumable` declaration: it
reads the first field, which is the payload `Expr.consume` yields (helper). -/
def consumeName (s : Nat) : String := "consume_S" ++ toString s

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
declaration has a destructor (`3.9`), and by the generated consumer when some
expression of the program eliminates a value of it (`consumed`). The
destructor prints the first field when that field is an `int`, which is the
observation channel the module docstring describes; a declaration whose first
field is not an `int` has nothing to print, and the interpreter's `dtor` event
for it is likewise silent. -/
def structItem (consumed : List Nat) (s : Nat) (sd : StructDecl) : String :=
  attrPrefix sd.attr ++ "struct " ++ tyName (.struct s) ++ " { " ++
    String.intercalate ", " (fieldDecls 0 sd.fields) ++ " }\n" ++
  (if sd.dtor then
    "drop fn " ++ tyName (.struct s) ++ "(self) { " ++
      (match sd.fields with
       | T :: _ => if T.isInt then "@dbg(self." ++ fieldName 0 ++ "); " else ""
       | [] => "") ++ "}\n"
   else "") ++
  (if sd.Consumable && consumed.contains s then
    "fn " ++ consumeName s ++ "(s: " ++ tyName (.struct s) ++ ") -> " ++
      tyName sd.payloadTy ++ " { s." ++ fieldName 0 ++ " }\n"
   else "")

/-- Every struct declaration of a program, in program order (helper). -/
def structItems (consumed : List Nat) : Nat → StructEnv → String
  | _, [] => ""
  | s, sd :: rest => structItem consumed s sd ++ structItems consumed (s + 1) rest

/-- Type inference without ownership: the fragment's types do not depend on
Σ, so the printer can recover every subexpression's type from the binders
alone. `Γ` lists binder types innermost first, exactly as `Ctx` does; `P` is
the program a call's callee is looked up in and `R` the enclosing function's
return type, which is the type `Checker.lean` gives a `return`. A `none`
means the program is ill-scoped, which elaborated programs never are
(helper). -/
def tyOf (P : Program) (R : Ty) (Γ : List Ty) : Expr → Option Ty
  | .intLit w s _ => some (.int w s)
  | .boolLit _ => some .bool
  | .unitLit => some .unit
  | .use i => Γ[i]?
  | .binop op e₁ _ =>
      match tyOf P R Γ e₁ with
      | some T => some (op.resultTy T)
      | none => none
  | .unop .not _ => some .bool
  | .unop _ e => tyOf P R Γ e
  | .intCast w s _ => some (.int w s)
  | .floatLit w _ => some (.float w)
  | .fintrin (.intToFloat w) _ => some (.float w)
  | .fintrin k e =>
      match tyOf P R Γ e with
      | some (.float w) => some (k.resTy w)
      | _ => none
  | .panic _ => some R
  | .dbg _ => some .unit
  | .mkStruct s _ => some (.struct s)
  | .consume e =>
      match tyOf P R Γ e with
      | some (.struct s) => (P.structs[s]?).map StructDecl.payloadTy
      | _ => none
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
  | .intLit _ _ n => if n < 0 then "(" ++ toString n ++ ")" else toString n
  | .boolLit b => if b then "true" else "false"
  | .unitLit => "()"
  | .use i => useName Γ i
  | .binop op e₁ e₂ =>
      if op.isCompare || op = .totalCmp then
        -- An ordering compare yields `bool` and `@total_cmp` yields `i32`, so
        -- nothing downstream names the operand type: a typed block supplies
        -- it (module docstring). `@total_cmp` is a `BinOp` in the core but an
        -- intrinsic on the surface, so its block ends in a call.
        let T := (tyOf P R Γ e₁).getD (.int .w64 .signed)
        let a := tmpName lvl "a"
        let b := tmpName lvl "b"
        "{ let " ++ a ++ ": " ++ tyName T ++ " = " ++ expr P R Γ (lvl + 1) e₁ ++ "; " ++
          "let " ++ b ++ ": " ++ tyName T ++ " = " ++ expr P R Γ (lvl + 1) e₂ ++ "; " ++
          (if op = .totalCmp then "@total_cmp(" ++ a ++ ", " ++ b ++ ")"
           else a ++ " " ++ binOpSym op ++ " " ++ b) ++ " }"
      else
        "(" ++ expr P R Γ lvl e₁ ++ " " ++ binOpSym op ++ " " ++ expr P R Γ lvl e₂ ++ ")"
  | .floatLit _ l => l.spell
  | .fintrin k e =>
      -- Each `@f` takes its result type from the *use* site (`3.12:16`,
      -- `3.12:17`, `3.12:19`) and none of them fixes its operand's type
      -- either, so both ends get a typed binder, exactly as `@intCast` does.
      let T' := (tyOf P R Γ e).getD (.float .w64)
      let T := (tyOf P R Γ (.fintrin k e)).getD (.float .w64)
      let c := tmpName lvl "c"
      let r := tmpName lvl "k"
      "{ let " ++ c ++ ": " ++ tyName T' ++ " = " ++ expr P R Γ (lvl + 1) e ++ "; " ++
        "let " ++ r ++ ": " ++ tyName T ++ " = " ++ fintrinName k ++ "(" ++ c ++ "); " ++
        r ++ " }"
  | .unop op e => "(" ++ unOpSym op ++ expr P R Γ lvl e ++ ")"
  | .intCast w s e =>
      -- `4.13:26` takes the target from the use site and nothing fixes the
      -- operand's own type either, so both ends get a typed binder.
      let T' := (tyOf P R Γ e).getD (.int .w64 .signed)
      let c := tmpName lvl "c"
      let k := tmpName lvl "k"
      "{ let " ++ c ++ ": " ++ tyName T' ++ " = " ++ expr P R Γ (lvl + 1) e ++ "; " ++
        "let " ++ k ++ ": " ++ tyName (.int w s) ++ " = @intCast(" ++ c ++ "); " ++ k ++ " }"
  | .panic msg => "@panic(" ++ quoted msg ++ ")"
  | .dbg e =>
      let T := (tyOf P R Γ e).getD (.int .w64 .signed)
      let g := tmpName lvl "g"
      "{ let " ++ g ++ ": " ++ tyName T ++ " = " ++ expr P R Γ (lvl + 1) e ++ "; " ++
        "@dbg(" ++ g ++ ") }"
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
      let T₁ := (tyOf P R Γ e₁).getD (.int .w64 .signed)
      let name := binderName Γ.length
      "{\n" ++
      indent (lvl + 1) ++ "let " ++ (if m then "mut " else "") ++ name ++
        ": " ++ tyName T₁ ++ " = " ++ expr P R Γ (lvl + 1) e₁ ++ ";\n" ++
      indent (lvl + 1) ++ expr P R (T₁ :: Γ) (lvl + 1) e₂ ++ "\n" ++
      indent lvl ++ "}"
  | .assign i e =>
      "{ " ++ useName Γ i ++ " = " ++ expr P R Γ lvl e ++ "; }"
  | .seq e₁ e₂ =>
      -- A discarded integer expression must still be typed (module
      -- docstring): nothing downstream of a statement names its type.
      let discard := match tyOf P R Γ e₁ with
        | some (.int w s) =>
            "let " ++ tmpName lvl "d" ++ ": " ++ tyName (.int w s) ++ " = " ++
              expr P R Γ (lvl + 1) e₁ ++ ";"
        | some (.float w) =>
            "let " ++ tmpName lvl "d" ++ ": " ++ tyName (.float w) ++ " = " ++
              expr P R Γ (lvl + 1) e₁ ++ ";"
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
  | .int _ _ | .float _ | .bool => "    @dbg(result);\n"
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

/-- The declaration indices an expression eliminates, read the way
`Print.expr` reads them — through `tyOf`, with the same index-`0` fallback on
an operand whose type it cannot recover, so the helpers emitted are exactly
the ones the printed calls name (helper). -/
partial def consumedIn (P : Program) (R : Ty) : List Ty → Expr → List Nat
  | Γ, .consume e =>
      (match tyOf P R Γ e with
       | some (.struct s) => [s]
       | _ => [0]) ++ consumedIn P R Γ e
  | Γ, .binop _ e₁ e₂ | Γ, .seq e₁ e₂ =>
      consumedIn P R Γ e₁ ++ consumedIn P R Γ e₂
  | Γ, .letIn _ e₁ e₂ =>
      consumedIn P R Γ e₁ ++
        consumedIn P R ((tyOf P R Γ e₁).getD (.int .w64 .signed) :: Γ) e₂
  | Γ, .ite c e₁ e₂ => consumedIn P R Γ c ++ consumedIn P R Γ e₁ ++ consumedIn P R Γ e₂
  | Γ, .mkStruct _ args | Γ, .call _ args => (args.map (consumedIn P R Γ)).flatten
  | Γ, .assign _ e | Γ, .ret e | Γ, .unop _ e | Γ, .intCast _ _ e | Γ, .fintrin _ e
  | Γ, .dbg e =>
      consumedIn P R Γ e
  | _, _ => []

/-- The declaration indices the whole program eliminates, so a printed module
declares a consumer for those and no others and carries no dead helper the
compiler would warn about (helper). -/
def consumedDecls (P : Program) : List Nat :=
  (P.fns.map (fun fd => consumedIn P fd.ret (bodyBinders fd) fd.body)).flatten

/-- Every struct declaration of a program as Rue items, with the consumers the
program actually calls (helper). -/
def moduleItems (P : Program) : String :=
  structItems (consumedDecls P) 0 P.structs

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
The program's own struct declarations come first, then its
functions as §2 writes them, then `main`, which calls the entry function `f0`
and observes its value (helper). -/
def program (name description : String) (rules : List String) (outcome : String)
    (P : Program) : String :=
  let T := match P.fns[0]? with
    | some fd => fd.ret
    | none => .int .w64 .signed
  "// Case: " ++ name ++ "\n" ++
  "// " ++ description ++ "\n" ++
  "// Rules: " ++ String.intercalate "; " rules ++ "\n" ++
  "// Expected: " ++ outcome ++ "\n" ++
  "// Printed from the RueCore fragment by docs/formal/lean/RueCore/Print.lean.\n" ++
  "// The struct declarations are the program's own (§2); a `drop fn` is how a\n" ++
  "// drop becomes observable (§6.11).\n" ++
  moduleItems P ++
  "\n" ++
  fnItems P 0 P.fns ++
  "fn main() -> i32 {\n" ++
  "    let result: " ++ tyName T ++ " = " ++ fnName 0 ++ "();\n" ++
  observeValue P.structs T ++
  "    0\n" ++
  "}\n"

end Print

end RueCore
