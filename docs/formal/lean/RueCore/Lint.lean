import Lean
import RueCore.Digest
import RueCore.Layers

/-!
# RueCore.Lint — the trusted-base lint (RUE-2457)

`lake exe ruecore-lint` (`RueCore/LintMain.lean`) asks the compiled
environment, for every declaration defined in a module of the package, three
questions, and exits non-zero when one of them has the wrong answer:

* **Axioms.** The axioms a declaration transitively depends on are among
  `propext` and `Quot.sound`. This is an *allow-list*: any other axiom fails,
  whatever it is called — `Classical.choice`, `sorryAx`, `Lean.ofReduceBool`,
  an `axiom` written in the package, and the auxiliary axiom that
  `native_decide` adds for each use from Lean 4.29 on (`foo._native.native_decide.ax_1_1`,
  stating `decide p = true`; `Lean/Meta/Native.lean`), which mentions none of
  the old compiler-evaluation primitives and so would pass a lint that matched
  their names. The traversal is one memoized pass over every constant's type
  and value (`axiomsOf`), after the TauCeti audit
  (`TauCetiProject/TauCeti`, `scripts/Axioms.lean`), and it walks the bodies
  itself rather than reading the per-module summaries Lean's
  `collectAxioms` precomputes when it writes an `.olean`.
* **Constructs.** In the modules `RueCore/Layers.lean` puts in L0–L2 and the
  Spec layer — the syntax, the definitions, the statements and the proofs —
  no declaration is `unsafe`, `partial`, `@[implemented_by]` or `@[extern]`,
  is an `opaque` constant (the
  kernel cannot see its value, so nothing about it can be proved from its
  definition), or mentions a compiler-evaluation primitive (`nativePrimitives`).
  Each of these makes the code `#eval` runs differ from the definition the
  kernel reasons about, or keeps a definition from the kernel altogether,
  without leaving an axiom behind. L3, the tooling, may use them; the lint
  lists every use.
* **Options.** No source file sets `debug.skipKernelTC`, which adds
  declarations without the kernel checking them, and none sets
  `maxHeartbeats` (or `synthInstance.maxHeartbeats`) to `0`, unbounded. A
  bounded override is allowed and listed. Options are not recorded in the
  environment, so this one question is put to the sources: each file under
  `RueCore/` and `RueCore.lean`, outside comments and string literals, is read
  as one stream of tokens (`tokens`), so a line break after `set_option`, a
  `«quoted»` name part or `set_option … in` changes nothing. A `set_option`
  whose name is not a literal identifier (a macro's `$o:ident`) fails,
  because nothing can be said of it; so does any other name mentioning
  `skipKernelTC`. `lakefile.toml`'s options are read too. The scan is
  best-effort: it does not parse every lexical form Lean has (an interpolated
  string's `{…}`, a raw string `r"…"`, a TOML escape in `lakefile.toml`), and
  text after one it misreads can be taken for the inside of a string.

**The source scan is a courtesy; the kernel re-check is the guarantee.** A
macro or an elaborator can set an option without writing `set_option`, and L3
imports `Lean`, so no reading of the sources can prove that every declaration
went through the kernel. What proves it is `leanchecker`, the toolchain's own
re-check, which replays every declaration of a module through the kernel on
top of that module's imports. It is run over the roots' whole import closure
outside the toolchain — every module `lake exe ruecore-layers --closure`
prints, found by walking the `.olean` import headers from each root of
`Layers.roots`, not by a module-name prefix — by `bin/chain.sh` and by the
Buck target `root//:lean-ruecore`, and a declaration added under
`skipKernelTC` that the kernel rejects fails it. The toolchain's own modules
(`Init`, `Std`, `Lean`, `Lake`) are not replayed: they are trusted as the
toolchain is. The layering audit fails on any other module in the closure, so
what is replayed is exactly the package's modules. The scan names the likely
culprit early, and on the line it is on.

**The statement/proof split** (RUE-2460, `spineProblems`). The headline
theorems are listed once, in the Spec layer (`RueCore.Spec.spine`), each
beside the `def …_stmt : Prop` that states it over L0 and L1 alone. The lint
checks that the list and the environment agree: each headline theorem's own
statement is its `_stmt`'s body, the same term up to binder names;
`RueCore.Spine.<name>` restates it with exactly the type `…_stmt`, which is
where the kernel checks the proof against the Spec statement; and no `_stmt`
or `Spine` theorem is outside the list. It also holds the layers to their
shapes (`layerShapeProblems`): L1 has no authored theorem, and a Spec module
declares nothing but the listed `_stmt`s and the list itself.

The same pass computes the **trusted base** (`trustedBase`): the package
definitions the Spec statements transitively unfold to — their constants, a
definition's body, an inductive type's constructors — which is what a
reviewer must read, beside the Spec layer, to know what the headline
theorems say. Proofs are not in it: a theorem met along the way is not
followed, because the kernel checks a proof and proof irrelevance makes its
content invisible to every statement. `TRUST.md` prints it (its "Trusted
base" section), and the lint prints its size.
-/

open Lean

namespace RueCore.Lint

/-! ## The headline statements -/

/-- (helper) The headline statements: the theorems the mechanization's claims
are made of. They are listed once, in the Spec layer (`RueCore.Spec.spine`,
RUE-2460), each beside the `…_stmt` statement it proves, and this list is
read from there: the §7 claims and their linking theorems — type safety over
`eval` and its named corollaries, the checker's soundness, the trace
properties, the reduction relation's own properties and §7 over it, and the
adequacy of `eval` to `Step` with the fuel lemmas. A lemma `03-metatheory.md`
cites as a step of a proof is not a claim and is not on the list. The
theorem names are resolved against the environment by `spineProblems`. -/
def headline : List Name := Spec.spine.map (·.1)

/-- (helper) The Spec statements, in the order of `headline`. -/
def statements : List Name := Spec.spine.map (·.2)

/-- (helper) Every entry of the Spec layer, each a theorem beside its
statement: the spine (`Spec.spine`), then the non-vacuity witnesses
(`Spec.witnesses`, RUE-2469), then the sharpness counter-examples
(`Spec.sharpness`, RUE-2485). The statement/proof checks, Comparator's
challenge and configuration, and the fingerprints are over all of them; the
headline list and the trusted base are the spine's alone. -/
def entries : List (Name × Name) :=
  Spec.spine ++ Spec.witnesses.map (fun (h, s, _) => (h, s)) ++
    Spec.sharpness.map fun (h, s, _) => (h, s)

/-! ## Where a declaration lives -/

/-- (helper) The module a constant is declared in, if the environment
imported it from one. -/
def moduleOf? (env : Environment) (n : Name) : Option Name :=
  (env.getModuleIdxFor? n).bind fun i => env.header.moduleNames[i.toNat]?

/-- (helper) Is this constant declared in a module of the package? -/
def inPackage (env : Environment) (n : Name) : Bool :=
  match moduleOf? env n with
  | some m => Layers.inPackage m
  | none => false

/-- (helper) Look a constant up with private bodies visible: a proof body in
a `module` is private, and both passes must see it. -/
def find? (env : Environment) (n : Name) : Option ConstantInfo :=
  (env.setExporting false).find? n

/-! ## The axiom pass -/

/-- (helper) The axioms this project's policy allows (`Digest.allowedAxioms`,
the same list `TRUST.md` states). -/
def allowedAxioms : List Name := Digest.allowedAxioms

/-- (helper) The compiler-evaluation primitives: what `native_decide` used
before Lean 4.29, and what a hand-written proof by reflection still uses.
`native_decide` itself no longer mentions them (its per-use axiom is caught
by the allow-list); this list catches a declaration that reaches for them
directly. -/
def nativePrimitives : List Name :=
  [``Lean.reduceBool, ``Lean.reduceNat, ``Lean.ofReduceBool, ``Lean.ofReduceNat,
   ``Lean.trustCompiler]

/-- (helper) Is this name one Lean's native evaluation made — a `_native`
component, as `Lean.Meta.nativeEqTrue` names the axiom it adds? Used only to
say *why* an axiom is there; the allow-list fails it either way. -/
def isNativeAux (n : Name) : Bool :=
  n.components.any (· == `_native)

/-- (helper) The memo of the axiom pass: every constant visited, with the
axioms it transitively depends on. One map is threaded through every
declaration of every environment the lint imports, so each constant is
walked once. -/
abbrev AxM := ReaderT Environment (StateM (NameMap (Array Name)))

/-- (helper) The union of two small axiom sets. -/
def union (a b : Array Name) : Array Name :=
  b.foldl (init := a) fun acc x => if acc.contains x then acc else acc.push x

mutual

/-- (helper) The axioms a constant transitively depends on, through its type
and its value, as `Lean.collectAxioms` walks them, memoized. The kernel's
constant graph is acyclic except within an inductive block (a type and its
constructors mention each other), which `blockAxioms` takes as one node, and
within a cluster of `unsafe` declarations, where the sentinel a constant is
given before its dependencies are walked can leave another member of the
cluster under-reported. Such a cluster is L3's (the kernel refuses a safe
declaration that mentions an unsafe one), and the member that mentions an
axiom directly is always reported with it, so no axiom is missed. -/
partial def axiomsOf (c : Name) : AxM (Array Name) := do
  if let some r := (← get).find? c then return r
  let env ← read
  match find? env c with
  | none => modify (·.insert c #[]); return #[]
  | some (.inductInfo v) => blockAxioms v.all
  | some (.ctorInfo v) =>
      let _ ← axiomsOf v.induct
      return ((← get).find? c).getD #[]
  | some (.recInfo v) =>
      let block ← blockAxioms v.all
      modify (·.insert c #[])
      let r := union block (← exprAxioms v.type)
      modify (·.insert c r)
      return r
  | some (.quotInfo _) => modify (·.insert c #[]); return #[]
  | some info =>
      modify (·.insert c #[])
      let own := if info matches .axiomInfo _ then #[c] else #[]
      let mut r := union own (← exprAxioms info.type)
      if let some v := info.value? (allowOpaque := true) then
        r := union r (← exprAxioms v)
      modify (·.insert c r)
      return r

/-- (helper) One inductive block — its types and their constructors — as one
node of the axiom pass: the axioms of every type and constructor type in it,
recorded for each member. -/
partial def blockAxioms (all : List Name) : AxM (Array Name) := do
  let env ← read
  if let some first := all.head? then
    if let some r := (← get).find? first then return r
  let mut members : Array Name := #[]
  let mut types : Array Lean.Expr := #[]
  for i in all do
    members := members.push i
    if let some (.inductInfo v) := find? env i then
      types := types.push v.type
      for c in v.ctors do
        members := members.push c
        if let some ci := find? env c then types := types.push ci.type
  for m in members do modify (·.insert m #[])
  let mut r : Array Name := #[]
  for t in types do
    for d in t.getUsedConstants do
      if !members.contains d then r := union r (← axiomsOf d)
  for m in members do modify (·.insert m r)
  return r

/-- (helper) The axioms of every constant an expression mentions. -/
partial def exprAxioms (e : Lean.Expr) : AxM (Array Name) :=
  e.getUsedConstants.foldlM (init := #[]) fun acc d => return union acc (← axiomsOf d)

end

/-! ## Findings -/

/-- (helper) One thing the lint reports about one declaration or one source
line. -/
structure Finding where
  /-- The declaration, or `file:line` for an option. -/
  subject : String
  /-- The module it is in. -/
  module : Name
  /-- Its layer (`RueCore/Layers.lean`), or 9 when the table has none. -/
  layer : Nat
  /-- What was found. -/
  detail : String
  /-- Does it fail the lint, or is it only listed? -/
  fails : Bool
  /-- Is it about axioms, rather than a construct or an option? -/
  isAxiom : Bool := false
deriving Inhabited

/-- (helper) A layer's label for a table, or a question mark for a module the
table does not have (the layering audit fails on that). -/
def layerLabel (l : Nat) : String :=
  if l ≤ Layers.toolingLayer then Layers.layerName l else "no layer"

/-- (helper) An axiom's name as a finding prints it, with the reason it is
there when Lean's native evaluation made it. -/
def axiomLabel (a : Name) : String :=
  if isNativeAux a then s!"{a} (native_decide: a result the kernel did not check)"
  else toString a

/-- (helper) Is this `opaque` constant the kernel's face of a `partial def`
(Lean compiles `partial def f` to an `opaque f` beside a `partial`
definition `f._unsafe_rec` the compiler runs)? -/
def isPartialFace (env : Environment) (n : Name) : Bool :=
  match find? env (n.str "_unsafe_rec") with
  | some info => info.isPartial || info.isUnsafe
  | none => false

/-- (helper) Is this the compiler's twin `f._unsafe_rec` of a declaration
`f`? Lean adds one beside every recursive definition — for well-founded
recursion, the code `#eval` runs instead of the `WellFounded.fix` term the
kernel sees, elaborated from the same equations; for a `partial def`, the only
code there is. Either way the finding belongs to `f`, which is reported for
what it is: a definition the kernel checks, or a `partial def`'s `opaque`
face. -/
def isRecTwin (env : Environment) : Name → Bool
  | .str p "_unsafe_rec" => (find? env p).isSome
  | _ => false

/-- (helper) The result type of a declaration's type, under its binders. -/
def resultHead (type : Lean.Expr) : Name :=
  type.getForallBody.getAppFn.constName?.getD .anonymous

/-- (helper) Does the source range `inner` lie within `outer`? -/
def rangeWithin (inner outer : DeclarationRange) : Bool :=
  let le (a b : Position) : Bool := a.line < b.line || (a.line == b.line && a.column ≤ b.column)
  le outer.pos inner.pos && le inner.endPos outer.endPos

/-- (helper) A declaration's source range, as the compiled module records it
(`Lean.declRangeExt`, in the `.olean`'s server part). -/
def rangeOf? (env : Environment) (n : Name) : Option DeclarationRange :=
  (declRangeExt.find? (level := .server) env n).map (·.range)

/-- (helper) Is this the printer a `deriving Repr` clause defines? For a
nested or mutual inductive type the `Repr` handler writes it as a `partial
def` (`instReprExpr.repr`), an `opaque` constant to the kernel. It is
recognized by where it came from, not by its name or its shape: an `opaque`
whose value type is `Std.Format`, declared under an instance of `Repr T`,
where both the printer and the instance were declared *inside the source
range of `T`'s own declaration*, in `T`'s module. That is where a `deriving`
clause puts what its handler adds (the range the compiled module records for
it is the clause's `Repr`); a printer written by hand, or by `deriving
instance` or a macro, is a command of its own and lies outside `T`'s range.
A new `deriving` handler would need `import Lean`, which L0–L2 cannot have.
It renders values for `#eval` and nothing else, so it is listed rather than
failed; if a statement reached it, the trusted base would list it.

The exemption is a policy convenience, not a trust claim. A single macro
call that writes the type, a printer and its instance gives all three the
call's range, so a printer written that way passes the range test too; such
a printer is an `opaque` of type `Std.Format`, which says nothing to the
kernel, and no headline statement reaches any `Repr` printer (`trustedBase`
would list it if one did). -/
def isDerivedReprPrinter (env : Environment) (n : Name) (info : ConstantInfo) : Bool :=
  match n, info with
  | .str p _, .opaqueInfo v =>
      resultHead v.type == ``Std.Format && Meta.isInstanceCore env p &&
        (match find? env p with
         | some pi =>
             let cls := pi.type.getForallBody
             cls.getAppFn.constName? == some ``Repr &&
               (match cls.getAppArgs.back?.map (·.getAppFn.constName?) with
                | some (some t) =>
                    moduleOf? env t == moduleOf? env n && moduleOf? env p == moduleOf? env n &&
                      (match rangeOf? env t, rangeOf? env n, rangeOf? env p with
                       | some rt, some rn, some rp => rangeWithin rn rt && rangeWithin rp rt
                       | _, _, _ => false)
                | _ => false)
         | none => false)
  | _, _ => false

/-- (helper) The constructs one declaration uses that make `#eval` and the
kernel disagree, or that keep a definition from the kernel, each with whether
it is exempt from failing in L0–L2 (`isDerivedReprPrinter`). -/
def constructs (env : Environment) (n : Name) (info : ConstantInfo) :
    Array (String × Bool) := Id.run do
  let mut out := #[]
  if isRecTwin env n then return out
  if info.isUnsafe then out := out.push ("unsafe", false)
  if info.isPartial then out := out.push ("partial", false)
  if let .opaqueInfo _ := info then
    if isDerivedReprPrinter env n info then
      out := out.push ("partial def of a `deriving Repr` printer (opaque to the kernel)", true)
    else
      out := out.push (if isPartialFace env n then ("partial def (opaque to the kernel)", false)
        else ("opaque (value invisible to the kernel)", false))
  if let some impl := Compiler.getImplementedBy? env n then
    out := out.push (s!"@[implemented_by {impl}]", false)
  if isExtern env n then out := out.push ("@[extern]", false)
  let mut used := info.type.getUsedConstants
  if let some v := info.value? (allowOpaque := true) then used := used ++ v.getUsedConstants
  for p in nativePrimitives do
    if used.contains p then out := out.push (s!"mentions {p} (compiler evaluation)", false)
  return out

/-- (helper) Does this disallowed axiom fail the lint for this declaration?
Everywhere, yes — with one exception, listed rather than failed: an L3
definition (not a theorem) that reaches `Classical.choice`. Lean's own
library puts it there — the proofs inside `String` and `Array` operations
(`String.endsWith` reaches it through `Array.extract_eq_self_iff`'s proof and
`Classical.propDecidable`), and the inhabitant of a `partial def` whose type
is only `Nonempty` (`Classical.ofNonempty`) — and no statement depends on
tooling code. A theorem in L3, and every declaration of L0–L2 and Spec, is held to the
allow-list. -/
def axiomFails (layer : Nat) (info : ConstantInfo) (a : Name) : Bool :=
  !(layer == Layers.toolingLayer && !(info matches .thmInfo _) && a == ``Classical.choice)

/-- (helper) Lint the package declarations of one environment that `done`
does not already hold: their axioms and, by layer, their constructs. Returns
the findings, the declarations and modules linted, and the axiom memo to
pass on. -/
def lintEnvironment (env : Environment) (done : NameSet) (memo : NameMap (Array Name)) :
    Array Finding × NameSet × NameSet × NameMap (Array Name) × Array Name := Id.run do
  let mut findings := #[]
  let mut linted := done
  let mut modules : NameSet := {}
  let mut memo := memo
  let mut used : Array Name := #[]
  for m in env.header.moduleNames do
    if Layers.inPackage m then modules := modules.insert m
  for (n, info) in env.constants.toList do
    if linted.contains n then continue
    let some m := moduleOf? env n | continue
    if !Layers.inPackage m then continue
    linted := linted.insert n
    let layer := (Layers.layerOf? m).getD 9
    let (axs, memo') := ((axiomsOf n).run env).run memo
    memo := memo'
    used := union used axs
    let bad := axs.filter (!allowedAxioms.contains ·)
    if let .axiomInfo _ := info then
      let d := "declares an axiom: " ++ axiomLabel n
      findings := findings.push
        { subject := toString n, module := m, layer := layer,
          detail := d, fails := true, isAxiom := true }
    if !bad.isEmpty then
      findings := findings.push
        { subject := toString n, module := m, layer := layer,
          detail := "depends on " ++ ", ".intercalate (bad.toList.map axiomLabel),
          fails := bad.any (axiomFails layer info), isAxiom := true }
    for (c, exempt) in constructs env n info do
      findings := findings.push
        { subject := toString n, module := m, layer := layer, detail := c,
          fails := (layer < Layers.toolingLayer && !exempt) || layer > Layers.toolingLayer }
  return (findings, linted, modules, memo, used)

/-! ## Options -/

/-- (helper) Can this character continue a name, so that a `'` after it is a
prime rather than a character literal? -/
def isIdentChar (c : Char) : Bool :=
  c.isAlphanum || c == '_' || c == '\'' || c == '!' || c == '?' || c.val ≥ 0x80

/-- (helper) A source text with its comments and string literals blanked out
(newlines kept, so line numbers survive): `--` to the end of the line,
nested `/- … -/` (doc-comments included), `"…"` with its escapes, and
character literals. What
is left is what Lean parses as commands. -/
def stripComments (s : String) : String := Id.run do
  let cs := s.toList.toArray
  let mut out : Array Char := #[]
  let mut i := 0
  let mut depth := 0
  let mut inStr := false
  let mut inLine := false
  while i < cs.size do
    let c := cs[i]!
    let next := cs[i+1]?
    if inLine then
      if c == '\n' then inLine := false; out := out.push c else out := out.push ' '
      i := i + 1
    else if depth > 0 then
      if c == '/' && next == some '-' then depth := depth + 1; out := out ++ #[' ', ' ']; i := i + 2
      else if c == '-' && next == some '/' then depth := depth - 1; out := out ++ #[' ', ' ']; i := i + 2
      else out := out.push (if c == '\n' then c else ' '); i := i + 1
    else if inStr then
      if c == '\\' then out := out ++ #[' ', ' ']; i := i + 2
      else
        if c == '"' then inStr := false
        out := out.push (if c == '\n' then c else ' '); i := i + 1
    else if c == '-' && next == some '-' then inLine := true; out := out ++ #[' ', ' ']; i := i + 2
    else if c == '/' && next == some '-' then depth := 1; out := out ++ #[' ', ' ']; i := i + 2
    else if c == '"' then inStr := true; out := out.push ' '; i := i + 1
    else if c == '\'' && !(i > 0 && isIdentChar cs[i-1]!) then
      -- a character literal, `'"'` or `'\n'`; a prime after a name is part of it
      let len := if next == some '\\' then
          (((cs.extract (i+3) (i+12)).findIdx? (· == '\'')).map (· + 4)).getD 1
        else if cs[i+2]? == some '\'' then 3 else 1
      out := out ++ Array.replicate len ' '; i := i + len
    else out := out.push c; i := i + 1
  return String.ofList out.toList

/-- (helper) One token of a stripped source: its text, with the `«»` of a
quoted name part dropped (`«debug».skipKernelTC` is `debug.skipKernelTC`, as
Lean reads it), the line it starts on, and whether it is a literal
identifier. An antiquotation (`$o`, `$o:ident`) is not one. -/
structure Token where
  /-- The text, unquoted. -/
  text : String
  /-- The line it starts on. -/
  line : Nat
  /-- Is it a literal identifier? -/
  ident : Bool
deriving Inhabited

/-- (helper) Can this character start a name? -/
def isIdentStart (c : Char) : Bool :=
  c.isAlpha || c == '_' || c == '«' || (c.val ≥ 0x80 && c != '»')

/-- (helper) A stripped source as one stream of tokens, across line breaks:
names (dotted, with quoted parts), numbers, antiquotations, and every other
character as a token of its own. Whitespace, line breaks included, only
separates tokens, so `set_option` on one line and its name on the next read
as they do to Lean. -/
def tokens (stripped : String) : Array Token := Id.run do
  let cs := stripped.toList.toArray
  let mut out : Array Token := #[]
  let mut i := 0
  let mut line := 1
  while i < cs.size do
    let c := cs[i]!
    if c == '\n' then line := line + 1; i := i + 1
    else if c.isWhitespace then i := i + 1
    else if isIdentStart c then
      let start := line
      let mut text : Array Char := #[]
      let mut more := true
      while more do
        if cs[i]? == some '«' then
          i := i + 1
          while i < cs.size && cs[i]! != '»' do
            if cs[i]! == '\n' then line := line + 1
            text := text.push cs[i]!; i := i + 1
          i := i + 1
        else
          while i < cs.size && isIdentChar cs[i]! && cs[i]! != '«' && cs[i]! != '»' do
            text := text.push cs[i]!; i := i + 1
        if cs[i]? == some '.' && (cs[i+1]?.map isIdentStart).getD false then
          text := text.push '.'; i := i + 1
        else more := false
      out := out.push { text := String.ofList text.toList, line := start, ident := true }
    else if c.isDigit || c == '$' then
      let mut text : Array Char := #[c]
      i := i + 1
      while i < cs.size && (isIdentChar cs[i]! || cs[i]! == '.') && cs[i]! != '«' do
        text := text.push cs[i]!; i := i + 1
      out := out.push { text := String.ofList text.toList, line := line, ident := false }
    else
      out := out.push { text := c.toString, line := line, ident := false }; i := i + 1
  return out

/-- (helper) The value of a natural-number literal as Lean reads one —
decimal, `0x`, `0b` or `0o`, with `_` separators — if the text is one. -/
def natLit? (t : String) : Option Nat :=
  let t := String.ofList (t.toList.filter (· != '_'))
  let digits (base : Nat) (s : List Char) : Option Nat :=
    if s.isEmpty then none else
    s.foldlM (init := 0) fun acc c =>
      let d := if c.isDigit then c.toNat - '0'.toNat
        else if 'a' ≤ c.toLower && c.toLower ≤ 'f' then c.toLower.toNat - 'a'.toNat + 10
        else base
      if d < base then some (acc * base + d) else none
  match t.toList with
  | '0' :: x :: rest =>
      if x == 'x' || x == 'X' then digits 16 rest
      else if x == 'b' || x == 'B' then digits 2 rest
      else if x == 'o' || x == 'O' then digits 8 rest
      else digits 10 ('0' :: x :: rest)
  | s => digits 10 s

/-- (helper) Does this text name the kernel-skipping option, in any spelling
the token stream leaves? -/
def mentionsSkipKernelTC (t : String) : Bool :=
  (t.splitOn "skipKernelTC").length > 1

/-- (helper) The verdict on one option setting: `some reason` when it fails
the lint. `debug.skipKernelTC` fails whatever it is set to; a heartbeat limit
fails at `0`, and when its value is not a number literal, since a macro could
make it `0`. -/
def optionVerdict (name value : String) : Option String :=
  if mentionsSkipKernelTC name then
    some "adds declarations the kernel does not check"
  else if name == "maxHeartbeats" || name.endsWith ".maxHeartbeats" then
    match natLit? value with
    | some 0 => some "an unbounded heartbeat limit"
    | some _ => none
    | none => some "a heartbeat limit that is not a number literal"
  else none

/-- (helper) Every option setting in a stripped source, every other
mention of the kernel-skipping option, and every `decide +kernel` (listed,
never failing: RUE-2469's policy), as `(line, what, failure)`: each
`set_option` with its name and value, where a name that is not a literal
identifier (a macro's `$o:ident`) fails, because nothing can be said of an
option a macro names; and any name mentioning `skipKernelTC` anywhere else. -/
def optionUses (stripped : String) : Array (Nat × String × Option String) := Id.run do
  let ts := tokens stripped
  let mut out := #[]
  let mut named : Array Nat := #[]
  for i in [:ts.size] do
    let t := ts[i]!
    if t.ident && t.text == "set_option" then
      match ts[i+1]?, ts[i+2]? with
      | some n, some v =>
          named := named.push (i+1)
          if n.ident then
            out := out.push (t.line, s!"set_option {n.text} {v.text}", optionVerdict n.text v.text)
          else
            out := out.push (t.line, s!"set_option {n.text} {v.text}",
              some "the option's name is not a literal identifier")
      | _, _ =>
          out := out.push (t.line, "set_option", some "the option's name is not a literal identifier")
    else if t.ident && mentionsSkipKernelTC t.text && !named.contains i then
      out := out.push (t.line, t.text, some "names the option that adds declarations the kernel does not check")
    -- `decide +kernel` and `decide (config := { kernel := true })` (RUE-2469):
    -- kernel reduction of the `Decidable` instance, no axiom, allowed where
    -- plain `decide` or `rfl` stops at the elaborator's limits; listed
    else if t.ident && t.text == "decide" &&
        ((List.range 8).any fun k => (ts[i+1+k]?.map (·.text)) == some "kernel") then
      out := out.push (t.line, "decide +kernel (kernel reduction, no axiom)", none)
  return out

/-- (helper) A source path as the module it compiles to. -/
def moduleOfPath (p : System.FilePath) : Name :=
  (p.withExtension "").components.foldl Name.str .anonymous

/-- (helper) The package's source files: `RueCore.lean` and everything under
`RueCore/`. -/
partial def sourceFiles (dir : System.FilePath) : IO (Array System.FilePath) := do
  let mut out := #[]
  for e in ← dir.readDir do
    if ← e.path.isDir then out := out ++ (← sourceFiles e.path)
    else if e.fileName.endsWith ".lean" then out := out.push e.path
  return out

/-- (helper) Every option the sources and `lakefile.toml` set, as findings.
Run from the package directory. -/
def optionFindings : IO (Array Finding × Nat) := do
  let files := #[("RueCore.lean" : System.FilePath)] ++
    ((← sourceFiles "RueCore").qsort (·.toString < ·.toString))
  let mut out := #[]
  for f in files do
    let m := moduleOfPath f
    let layer := (Layers.layerOf? m).getD 9
    for (line, what, v) in optionUses (stripComments (← IO.FS.readFile f)) do
      let d := what ++ (match v with | some r => s!" ({r})" | none => "")
      out := out.push
        { subject := s!"{f}:{line}", module := m, layer := layer,
          detail := d, fails := v.isSome }
  if ← System.FilePath.pathExists "lakefile.toml" then
    let mut line := 1
    for l in (← IO.FS.readFile "lakefile.toml").splitOn "\n" do
      let t := l.trimAscii.toString
      if mentionsSkipKernelTC t then
        out := out.push
          { subject := s!"lakefile.toml:{line}", module := `lakefile, layer := 9,
            detail := t ++ " (adds declarations the kernel does not check)", fails := true }
      else if (t.splitOn "maxHeartbeats").length > 1 then
        -- the value after the name, however the table or flag spells it
        let after := ((t.splitOn "maxHeartbeats").getLast!.toList.dropWhile
          (fun c => c == '"' || c == '\'' || c == '»' || c == '=' || c == ' ' || c == ':'))
        let zero := natLit? (String.ofList (after.takeWhile (fun c => c.isAlphanum || c == '_'))) == some 0
        out := out.push
          { subject := s!"lakefile.toml:{line}", module := `lakefile, layer := 9,
            detail := t ++ (if zero then " (an unbounded heartbeat limit)" else ""), fails := zero }
      line := line + 1
  return (out, files.size)

/-! ## The trusted base -/

/-- (helper) What the trusted-base pass found: the authored definitions, with
their module and kind; the instances; and how many Lean-generated
auxiliaries (matchers, recursors, structural-recursion images) it passed
through. -/
structure TrustedBase where
  /-- Each authored definition: name, module, layer, kind. -/
  definitions : Array (Name × Name × Nat × String)
  /-- The instances the statements' definitions use. -/
  instances : Array Name
  /-- The Lean-generated constants passed through. -/
  generated : Nat
  /-- A headline name the environment does not have as a theorem. -/
  missing : Array Name

/-- (helper) Is this a constant Lean records as one it made itself: a
recursor or one of its auxiliaries (`casesOn`, `recOn`, `brecOn`, `below`), a
`noConfusion`, a sparse `casesOn`, a matcher (`match_1`) or a structure
projection. Each of these is read from the table Lean keeps for it (the
auxiliary-recursor, `noConfusion`, sparse-`casesOn`, matcher and projection
extensions, and the kernel's own recursors), not from its name, and only
Lean's elaborator writes those tables: an L0–L2 module imports only `Init`,
so it has no way to mark a definition of its own. -/
def isRecordedAux (env : Environment) (n : Name) : Bool :=
  isAuxRecursor env n || isRecCore env n || isNoConfusion env n || isSparseCasesOn env n ||
    Meta.isMatcherCore env n || env.isProjectionFn n

/-- (helper) Is this a constant Lean made beside a declaration, with nothing
of its own to read? Decided conservatively, from what the environment
records, so that a doubtful case is listed in the trusted base rather than
counted: either Lean's own tables say so (`isRecordedAux`), or the constant is
named the way Lean names a by-product — an internal name after `private` is
stripped (`_sunfold`, `_f`, `_sparseCasesOn_1`), one of the constants Lean
adds beside an inductive type (`ctorIdx`, `ofNat`, `noConfusionType`, …),
`deriving DecidableEq`'s `ofNat_ctorIdx` lemma, or a constant under a
recorded auxiliary (`brecOn.go`) — **and** it has no source range of its own
(`Lean.declRangeExt`). Every declaration a command writes gets a range, so a
hand-written `T.ndrec`, `T.rec.helper` or `RueCore._x`, whose name looks like
Lean's, is listed; so is a `private` definition, and one declared under an
instance's name (`instDecidableEqTy.decEq`). -/
def isLeanAux (env : Environment) (n : Name) : Bool :=
  isRecordedAux env n ||
    ((rangeOf? env n).isNone &&
      ((privateToUserName n).isInternalDetail || Digest.isDecEqEnumLemma env n ||
        match n with
        | .str p s =>
            (match find? env p with
             | some (.inductInfo _) =>
                 ["ctorIdx", "toCtorIdx", "ctorElim", "ctorElimType", "noConfusionType", "ndrec",
                  "ndrecOn", "ofNat"].contains s
             | _ => false) ||
              ((find? env p).isSome && isRecordedAux env p)
        | _ => false))

/-- (helper) Every package constant the given constants transitively unfold
to, the given ones included. From a definition, its type and body are
followed; from an inductive type, its constructors' types; from a constructor
or a recursor, its type. A theorem met along the way is not followed: proofs
are the kernel's business. -/
def unfoldClosure (env : Environment) (start : Array Name) : NameSet := Id.run do
  let mut work := start
  let mut seen : NameSet := {}
  while !work.isEmpty do
    let n := work.back!
    work := work.pop
    if seen.contains n || !inPackage env n then continue
    let some info := find? env n | continue
    if info matches .thmInfo _ then continue
    seen := seen.insert n
    work := work ++ info.type.getUsedConstants
    match info with
    | .defnInfo v => work := work ++ v.value.getUsedConstants
    | .opaqueInfo v => work := work ++ v.value.getUsedConstants
    | .inductInfo v => work := work ++ v.ctors.toArray
    | .ctorInfo v => work := work.push v.induct
    | _ => pure ()
  return seen

/-- (helper) The body of a Spec statement: what its `def …_stmt : Prop`
says, or `none` when the environment has no such definition. -/
def statementBody? (env : Environment) (stmt : Name) : Option Lean.Expr :=
  match find? env stmt with
  | some (.defnInfo v) => some v.value
  | _ => none

/-- (helper) The definitions one set of constants rests on, as a reviewer
counts them: the closure, less constructors, instances and Lean's own
auxiliaries. -/
def readable (env : Environment) (seen : NameSet) : Array Name := Id.run do
  let mut out := #[]
  for n in seen.toList do
    let some info := find? env n | continue
    if info matches .ctorInfo _ then continue
    if Meta.isInstanceCore env n || isLeanAux env n then continue
    out := out.push n
  return out.qsort (·.toString < ·.toString)

/-- (helper) The trusted base of the headline statements: every package
constant the Spec statements (`statements`) transitively unfold to, the
statements themselves left out — they are the Spec layer, read in full. It
starts from each statement's body, so it is the same set the theorems' own
statements reach, since each is its statement's body (`spineProblems`). -/
def trustedBase (env : Environment) : CoreM TrustedBase := do
  -- `isLeanAux`, not `Digest.isGenerated`: a private or instance-scoped
  -- definition a statement reaches is listed, not counted
  let mut start : Array Name := #[]
  let mut missing := #[]
  for (h, s) in Spec.spine do
    match statementBody? env s with
    | some body => start := start ++ body.getUsedConstants
    | none => missing := missing.push s
    unless (find? env h) matches some (.thmInfo _) do missing := missing.push h
  let seen := unfoldClosure env start
  let mut definitions := #[]
  let mut instances := #[]
  let mut generated := 0
  for n in seen.toList do
    let some info := find? env n | continue
    if info matches .ctorInfo _ then continue
    if Meta.isInstanceCore env n then instances := instances.push n
    else if isLeanAux env n then generated := generated + 1
    else
      let m := (moduleOf? env n).getD .anonymous
      definitions := definitions.push (n, m, (Layers.layerOf? m).getD 9, Digest.kindOf info)
  let order (a b : Name × Name × Nat × String) : Bool :=
    a.2.2.1 < b.2.2.1 || (a.2.2.1 == b.2.2.1 &&
      (a.2.1.toString < b.2.1.toString || (a.2.1 == b.2.1 && a.1.toString < b.1.toString)))
  return { definitions := definitions.qsort order,
           instances := instances.qsort (·.toString < ·.toString), generated, missing }

/-! ## The spine: each headline proof against its statement -/

/-- (helper) The theorem of `RueCore.Nonvacuous.Glue` that applies a spine
theorem to a witness's facts (RUE-2469): `RueCore.Nonvacuous.dtor` and
`RueCore.Step.det` give `RueCore.Nonvacuous.Glue.dtor.Step.det`. -/
def glueName (witness thm : Name) : Name :=
  (`RueCore.Nonvacuous.Glue ++ witness.replacePrefix `RueCore.Nonvacuous .anonymous) ++
    thm.replacePrefix `RueCore .anonymous

/-- (helper) The theorem of `Spine.lean` that checks a headline theorem
against its statement: `RueCore.soundness` is checked by
`RueCore.Spine.soundness`. -/
def spineName (thm : Name) : Name :=
  thm.replacePrefix `RueCore `RueCore.Spine

/-- (helper) What is wrong with the statement/proof split, if anything
(RUE-2460), each as a sentence:

* every entry of `Spec.spine`, `Spec.witnesses` and `Spec.sharpness`
  (`entries`) names a
  theorem of the environment and a `def … : Prop` declared in a Spec-layer
  module, and no entry is repeated;
* each headline theorem's own statement *is* its Spec statement's body, the
  same term up to binder names and binder annotations (`Expr.eqv`) — so the
  statement a reviewer reads in `Spec` is word for word the one the proof
  layer states, not merely one the kernel can unfold to it;
* `RueCore.Spine.<name>` exists, is declared in `RueCore.Spine`, and has
  exactly the type `RueCore.Spec.<name>_stmt` (the kernel checked its proof,
  `@RueCore.<name>`, against that type);
* every `…_stmt` definition of a Spec-layer module is in one of the three
  lists, and `RueCore.Spine` declares no other theorem, so nothing is stated
  or bound outside them;
* every witness names at least one spine theorem, each a theorem of
  `Spec.spine`, and every spine theorem is named by some witness (RUE-2469):
  no spine statement is left without a non-vacuity witness;
* every (witness, spine theorem) pair is applied in the kernel: the theorem
  `RueCore.Nonvacuous.Glue.<witness>.<thm>` exists and its proof uses both
  `RueCore.Spine.<witness>` and `RueCore.Spine.<thm>`, an application that
  elaborates only if the witness supplies the theorem's hypotheses. -/
def spineProblems (env : Environment) : Array String := Id.run do
  let mut out := #[]
  let mut seenT : NameSet := {}
  let mut seenS : NameSet := {}
  for (h, s) in entries do
    if seenT.contains h || seenS.contains s then
      out := out.push s!"{h}/{s}: listed twice in RueCore.Spec.spine, RueCore.Spec.witnesses and RueCore.Spec.sharpness"
    seenT := seenT.insert h
    seenS := seenS.insert s
    let sLayer := (moduleOf? env s).bind Layers.layerOf?
    let body? : Option Lean.Expr := match find? env s with
      | some (.defnInfo v) =>
          if v.type == .sort .zero && sLayer == some Layers.specLayer then some v.value else none
      | _ => none
    let some body := body?
      | out := out.push s!"{s}: not a `def … : Prop` of a Spec-layer module"; continue
    match find? env h with
    | some (.thmInfo v) =>
        if !(v.type == body) then
          out := out.push s!"{h}: its statement is not {s}'s body (up to binder names), though the two may be definitionally equal"
    | _ => out := out.push s!"{h}: not a theorem of the environment"
    let b := spineName h
    match find? env b with
    | some (.thmInfo v) =>
        if v.type != .const s [] then
          out := out.push s!"{b}: its type is not exactly {s}"
        if moduleOf? env b != some `RueCore.Spine then
          out := out.push s!"{b}: not declared in RueCore.Spine"
    | _ => out := out.push s!"{b}: missing; RueCore.Spine must restate {h} as `{s}`"
  for (n, info) in env.constants.toList do
    let some m := moduleOf? env n | continue
    if Layers.layerOf? m == some Layers.specLayer then
      if let .defnInfo _ := info then
        if (n.toString.endsWith "_stmt") && !seenS.contains n then
          out := out.push s!"{n}: a Spec statement none of RueCore.Spec.spine, RueCore.Spec.witnesses and RueCore.Spec.sharpness lists"
    if m == `RueCore.Spine then
      if let .thmInfo _ := info then
        if (rangeOf? env n).isSome && !(entries.any fun (h, _) => spineName h == n) then
          out := out.push s!"{n}: a theorem of RueCore.Spine that binds no entry of RueCore.Spec.spine, RueCore.Spec.witnesses or RueCore.Spec.sharpness"
  -- the witnesses (RUE-2469): each names spine theorems, and every spine
  -- theorem is named by some witness, so no spine statement is left without
  -- a non-vacuity witness
  for (h, _, ts) in Spec.witnesses do
    if ts.isEmpty then
      out := out.push s!"{h}: a witness in RueCore.Spec.witnesses that names no spine theorem"
    for t in ts do
      if !(headline.contains t) then
        out := out.push s!"{h}: names {t}, which is not a theorem of RueCore.Spec.spine"
  for t in headline do
    if !(Spec.witnesses.any fun (_, _, ts) => ts.contains t) then
      out := out.push s!"{t}: a spine theorem no entry of RueCore.Spec.witnesses names; it has no non-vacuity witness"
  -- every listed pair is applied in the kernel (RUE-2469): the glue theorem
  -- `Glue.<witness>.<theorem>` exists and its proof uses both the witness's
  -- and the spine theorem's `RueCore.Spine` constant
  for (h, _, ts) in Spec.witnesses do
    for t in ts do
      let g := glueName h t
      match find? env g with
      | some (.thmInfo v) =>
          let us := v.value.getUsedConstants
          if !(us.contains (spineName h) && us.contains (spineName t)) then
            out := out.push s!"{g}: does not apply {spineName t} to {spineName h}'s facts"
      | _ => out := out.push s!"{h} lists {t}, but {g} is missing: no kernel-checked application of the pair"
  return out

/-! ## Sharpness: every hypothesis needed, or a reason (RUE-2485) -/

/-- (helper) A statement's **hypotheses**: its binders of a `Prop` type,
in the order they occur, walking its `∀`s, and the two sides of an `∧` or an
`↔` and the body of an `∃` in its conclusion (`SPINE.md`'s "no hypotheses" is
this list empty). `Spec.sharpness` and `Spec.sharpnessReasons` number a
hypothesis by its place here, from 1. Each is returned as its type, with the
binders before it in scope, pretty-printed. -/
partial def hypotheses (e : Lean.Expr) : MetaM (Array String) := do
  let e ← Meta.whnfR e
  match e with
  | .forallE _ d _ _ =>
      let here ← if ← Meta.isProp d then pure #[toString (← Meta.ppExpr d)] else pure #[]
      Meta.forallBoundedTelescope e (some 1) fun _ b => do
        return here ++ (← hypotheses b)
  | _ =>
      match e.getAppFnArgs with
      | (``And, #[a, b]) | (``Iff, #[a, b]) => return (← hypotheses a) ++ (← hypotheses b)
      | (``Exists, #[_, f]) =>
          match f with
          | .lam n d b bi => Meta.withLocalDecl n bi d fun x => hypotheses (b.instantiate1 x)
          | _ => return #[]
      | _ => return #[]

/-- (helper) The number of hypotheses of a spine theorem's statement, if the
environment has the statement. -/
def hypothesisCount (env : Environment) (thm : Name) : MetaM (Option Nat) := do
  let some (_, s) := Spec.spine.find? (·.1 == thm) | return none
  let some (.defnInfo v) := find? env s | return none
  return some (← hypotheses v.value).size

/-- (helper) What is wrong with the sharpness lists, if anything (RUE-2485),
each as a sentence:

* every pair of `Spec.sharpness` and `Spec.sharpnessReasons` names a theorem
  of `Spec.spine` and one of its hypotheses (`1 ≤ i ≤` the number
  `hypotheses` counts), every counter-example names at least one pair, and
  every reason is a sentence;
* **every hypothesis of every spine statement** is named by a counter-example
  or by a reason, and not by both; a statement with no hypotheses (five) is
  named by neither.

The counter-example statements themselves are `entries`, so `spineProblems`
holds each to a spine entry's checks. -/
def sharpProblems (env : Environment) : MetaM (Array String) := do
  let mut out := #[]
  let mut counts : NameMap Nat := {}
  for (t, _) in Spec.spine do
    if let some k ← hypothesisCount env t then counts := counts.insert t k
  let check (what : String) (t : Name) (i : Nat) : Option String :=
    match counts.find? t with
    | none => some s!"{what} names {t}, which is not a theorem of RueCore.Spec.spine"
    | some k =>
        if i == 0 || i > k then
          some s!"{what} names hypothesis {i} of {t}, which has {k} hypotheses (`Lint.hypotheses`)"
        else none
  for (h, _, ps) in Spec.sharpness do
    if ps.isEmpty then
      out := out.push s!"{h}: a counter-example in RueCore.Spec.sharpness that names no spine hypothesis"
    for (t, i) in ps do
      if let some p := check s!"{h}" t i then out := out.push p
  for (t, i, r) in Spec.sharpnessReasons do
    if let some p := check "RueCore.Spec.sharpnessReasons" t i then out := out.push p
    if r.all Char.isWhitespace then
      out := out.push s!"{t}: hypothesis {i} has an empty reason in RueCore.Spec.sharpnessReasons"
  for (t, _) in Spec.spine do
    let k := (counts.find? t).getD 0
    for i in List.range' 1 k do
      let byEx := Spec.sharpness.any fun (_, _, ps) => ps.contains (t, i)
      let byReason := Spec.sharpnessReasons.any fun (t', i', _) => t' == t && i' == i
      if !byEx && !byReason then
        out := out.push s!"{t}: hypothesis {i} has no counter-example in RueCore.Spec.sharpness and no reason in RueCore.Spec.sharpnessReasons"
      if byEx && byReason then
        out := out.push s!"{t}: hypothesis {i} has both a counter-example and a reason; keep one"
  return out

/-- (helper) The lists the Spec layer holds beside its statements: the spine,
the non-vacuity witnesses (RUE-2469), and the sharpness counter-examples and
reasons (RUE-2485). -/
def specLists : List Name :=
  [``Spec.spine, ``Spec.witnesses, ``Spec.sharpness, ``Spec.sharpnessReasons]

/-- (helper) What is wrong with the layers' shapes, if anything (RUE-2460),
each as a sentence. Two invariants the statement/proof split rests on:

* **L1 holds no authored theorem.** Its modules are the definitions the
  statements are written in; a proof about them belongs in L2 (the
  `…/Lemmas.lean` modules). Instances are definitions, and the constants Lean
  makes beside a declaration (equation lemmas, a `decreasing_by` proof) have
  no source range of their own, so they pass; so does a proof-valued field
  of a structure (`WfProgram.decls`), whose projection is part of the
  structure's definition. L0 is not held to this:
  `Syntax.lean` and `Float.lean` keep their few well-formedness lemmas.
* **The Spec layer holds statements only.** Every declaration a Spec module
  writes is either a `…_stmt` that `Spec.spine`, `Spec.witnesses` or
  `Spec.sharpness` lists, or one of the Spec lists itself (`specLists`, with
  `Spec.sharpnessReasons`): no theorem (a proof there would ride into Comparator's challenge,
  which imports what the statements need) and no helper definition (which
  would enter the trusted base as a statement's word without being one).

A declaration counts as written by the module when it has a source range
(`rangeOf?`), as every command's declaration does. -/
def layerShapeProblems (env : Environment) : Array String := Id.run do
  let mut out := #[]
  let stmts := entries.map (·.2)
  for (n, info) in env.constants.toList do
    let some m := moduleOf? env n | continue
    let some layer := Layers.layerOf? m | continue
    if (rangeOf? env n).isNone || env.isProjectionFn n then continue
    if layer == 1 then
      if let .thmInfo _ := info then
        out := out.push s!"{n}: an authored theorem in L1 module {m}; L1 holds definitions only, so move it to L2 (a …/Lemmas.lean module)"
    if layer == Layers.specLayer then
      match info with
      | .thmInfo _ =>
          out := out.push s!"{n}: a theorem in Spec module {m}; the Spec layer holds statements only, so move it to L2"
      | _ =>
          if !(stmts.contains n || specLists.contains n) then
            out := out.push s!"{n}: a declaration of Spec module {m} that is neither one of the Spec lists ({specLists}) nor a `_stmt` they list; the Spec layer holds statements only"
  return out

/-- (helper) The trusted base as `TRUST.md`'s "Trusted base" section. -/
def renderTrustedBase (tb : TrustedBase) : List String :=
  let modules := tb.definitions.foldl (init := (#[] : Array (Name × Nat))) fun acc (_, m, l, _) =>
    if acc.any (·.1 == m) then acc else acc.push (m, l)
  let kinds := tb.definitions.foldl (init := (#[] : Array (String × Nat))) fun acc (_, _, _, k) =>
    match acc.findIdx? (·.1 == k) with
    | some i => acc.modify i fun (k, c) => (k, c + 1)
    | none => acc.push (k, 1)
  let rows := modules.toList.map fun (m, l) =>
    let names := tb.definitions.filter (·.2.1 == m) |>.toList.map fun (n, _, _, _) =>
      s!"`{Digest.shortName n}`"
    s!"| `{m}` | {layerLabel l} | {names.length} | " ++ ", ".intercalate names ++ " |"
  ["## Trusted base",
   "",
   "What a reviewer must read to know what the headline theorems say: every",
   "definition of the package their statements transitively unfold to — the",
   "constants of each statement, a definition's body, an inductive type's",
   "constructors — computed from the compiled environment by the same pass as",
   "`lake exe ruecore-lint` (`RueCore/Lint.lean`, `trustedBase`). Proofs are not",
   "in it, because the kernel checks them; neither is anything of Lean's own",
   "library. The headline statements are the Spec layer's (`RueCore/Spec.lean`,",
   "`RueCore.Spec.spine`): the §7 claims and their linking theorems, each stated once",
   "as a `def …_stmt : Prop` and proved by the theorem beside it. The pass starts from",
   "those statements' bodies, so the statements themselves are not counted here; they",
   "are read in full, in `SPINE.md`. A lemma `03-metatheory.md` cites as a step of a",
   "proof is not a claim, and is not a headline. The non-vacuity witnesses",
   "(`RueCore.Spec.witnesses`) are not headlines either, and they may name",
   "definitions outside this base (`Float.exactOps` and its `roundRat`, the",
   "witness programs): a witness can only fail to witness, never widen a claim.",
   "",
   s!"- Headline statements: {headline.length} — " ++
     ", ".intercalate (headline.map (s!"`{Digest.shortName ·}`")) ++ ".",
   s!"- Definitions to read: **{tb.definitions.size}**, in {modules.size} modules (" ++
     ", ".intercalate (kinds.toList.map fun (k, c) => s!"{c} {k}") ++ ").",
   s!"- Instances they use: {tb.instances.size}" ++
     (if tb.instances.isEmpty then "."
      else " — " ++ ", ".intercalate (tb.instances.toList.map (s!"`{Digest.shortName ·}`")) ++
        ". A `deriving` image says nothing beyond its type; a hand-written one is read " ++
        "with the predicate it decides."),
   s!"- Lean-generated auxiliaries passed through (`isLeanAux`): {tb.generated}. Each is",
   "  Lean's rendering of a definition listed here, so there is nothing more to read in",
   "  them. A constant counts as Lean's only when Lean's own tables record it (recursors",
   "  and their auxiliaries, matchers, projections), or when it is named as Lean names a",
   "  by-product and has no source range of its own; anything else is listed above,",
   "  including a `private` definition and one declared under an instance's name.",
   "",
   "| Module | Layer | Count | Definitions |",
   "| --- | --- | --- | --- |"] ++ rows ++ [""]

end RueCore.Lint
