import Lean
import RueCore

/-!
# RueCore.Digest — the expert validation surface (RUE-2247)

Two generated reports, for a reader who knows type systems or proof
assistants and wants to decide whether to believe the mechanization without
trusting its authors:

* **`DIGEST.md`** (`lake exe ruecore-digest`): every theorem of the `RueCore`
  namespace with its fully elaborated statement and its doc-comment, and
  every definition those statements are written in terms of. Statements, not
  proofs — a proof body is not the evidence here, the kernel is.
* **`TRUST.md`** (`lake exe ruecore-digest --trust`): for every theorem, the
  axioms it actually depends on (`Lean.collectAxioms`), the `sorry` count
  taken from those axioms rather than from a grep, the axioms the package
  declares itself, and the pinned toolchain.

Both are read out of the compiled environment (`Lean.Environment`), so they
cannot drift from the sources the way a hand-written summary can: this module
imports nothing about the calculus and knows no declaration by name.

What counts as *authored* is asked of the environment too. Lean generates a
great many constants of its own — recursors, matchers, `deriving` images,
reserved names such as `f.eq_def` and `f.induct`, field projections — and
none of them is evidence, so neither report lists them. Deciding which is
which from the last component of a name would make the reports' completeness
a matter of naming: an author could hide an unfinished proof from both by
calling it `Ty.congr`. `isGenerated` therefore asks Lean's own predicates
(`isAutoDeclOrPrivate_Internal`, the recursor, matcher, instance, projection
and structure-field tables) instead.

The definitions a statement is "written in terms of" are its transitive
*type*-level dependencies: the constants of the theorem's statement, of those
constants' types, of an inductive's constructor types, and of whatever body
the entry itself prints. An entry prints a body when the definition **is** a
type or a predicate (its type ends in a sort: `Ctx`, `CellMatches`,
`InBounds`), and otherwise when the body is short enough to read
(`maxBodyLines`) — because a signature alone cannot tell `Ty.mult` from
`fun _ => .copy`, and a reader deciding whether the linearity theorems are
vacuous needs to see which it is. A long one (`eval`, `check`, `explain`) is
reported by signature and doc-comment, and its body lives in the module named
beside it. Where the compiled value is the elaborator's output rather than
what was written — a `brecOn` term — the entry prints the defining equations
Lean derived instead, which is the same content in readable form.

Two claims the reports make about themselves are checked rather than
asserted, and `lake exe ruecore-digest` exits non-zero, naming the miss, when
either fails:

* **Closure.** Every `RueCore` constant occurring in a signature or a body
  the digest prints is an entry of the digest, or is a constructor listed
  under its type's entry. (Lean's own auxiliaries are exempt: the pretty
  printer renders a matcher as a `match` and a recursor as nothing at all, so
  they occur in the term and not in the text.)
* **Completeness against `INDEX.md`.** The index is generated from the
  *sources* by `scripts/validate-lean-xref-index.py`; this file is read from
  the *compiled environment*. Every declaration the index names in a module
  the digest imports must be a constant of the environment, must survive the
  generated-declaration filter, and — for a theorem or an axiom, which the
  digest lists unconditionally — must have an entry here. That is the
  property an expert wants and the one a name-based filter could not keep.

The fragment boundary — which calculus rules and which syntactic forms have a
core image at all — is not something the environment knows; it is `INDEX.md`'s.
The digest quotes that file's coverage lines rather than restating them.
-/

open Lean

namespace RueCore.Digest

/-! ## What the reports cover -/

/-- (helper) The namespace both reports are about. -/
def root : Name := `RueCore

/-- (helper) Is this declaration one of the mechanization's own? -/
def inRoot (n : Name) : Bool := root.isPrefixOf n

/-- (helper) A field projection of a structure: implied by the structure's
own entry, so never listed separately. -/
def isFieldProjection (env : Environment) : Name → Bool
  | .str p f => isStructure env p && (getStructureFields env p).contains (Name.mkSimple f)
  | _ => false

/-- (helper) The one generated constant Lean's own predicate misses in
4.33.1. `deriving DecidableEq` on an enumeration adds a `T.ofNat_ctorIdx`
lemma beside the `T.ofNat`/`T.ctorIdx` pair
(`Lean/Elab/Deriving/DecEq.lean`), and `isAutoDeclOrPrivate_Internal` knows
the pair but not the lemma. It is recognized the way Lean recognizes the
pair — the parent is the inductive the handler ran on, and the pair it
generated is there beside it — rather than by the suffix on its own. -/
def isDecEqEnumLemma (env : Environment) : Name → Bool
  | .str p "ofNat_ctorIdx" =>
      (match env.find? p with | some (.inductInfo _) => true | _ => false)
        && (env.find? (Name.mkStr p "ofNat")).isSome
        && (env.find? (Name.mkStr p "ctorIdx")).isSome
  | _ => false

/-- (helper) Is this constant one Lean made rather than one the sources
wrote? Every test here is a question put to the environment:
`isAutoDeclOrPrivate_Internal` is Lean's own answer to "did I make this?"
(macro scopes, internal names, reserved names such as `f.eq_def`, `f.induct`
and `T.injEq`, the `noConfusion`/`ctorIdx` family), and the rest read the
recursor, matcher, instance, projection and structure-field tables. A
`deriving` clause produces ordinary instances, so the instance table covers
those; `isDecEqEnumLemma` above covers the one by-product of a `deriving`
handler that Lean's predicate does not claim. Anything hanging off a
constructor, or off a generated declaration, goes with it.

Deciding this from the last component of a name would put the reports'
completeness in the hands of whoever chooses the names — a `theorem Ty.congr`
or a `theorem instTyHole` would leave both reports silently — which is the
wrong shape for a surface meant to be read without trusting its authors. -/
partial def isGenerated (env : Environment) (n : Name) : CoreM Bool := do
  if n.isInternalDetail then return true
  if ← isAutoDeclOrPrivate_Internal n then return true
  if isAuxRecursor env n || isRecCore env n || isNoConfusion env n then return true
  if Meta.isMatcherCore env n || Meta.isInstanceCore env n then return true
  if env.isProjectionFn n || isFieldProjection env n then return true
  if isDecEqEnumLemma env n then return true
  match n with
  | .str p _ =>
      match env.find? p with
      | some (.ctorInfo _) => return true
      | _ => if p.isAnonymous then return false else isGenerated env p
  | _ => return false

/-- (helper) Every constant of the mechanization's namespace that Lean did
not generate: what both reports mean by "authored". -/
def authoredNames (env : Environment) : CoreM NameSet := do
  let mut out := NameSet.empty
  for (name, _) in env.constants.toList do
    if inRoot name then
      if !(← isGenerated env name) then out := out.insert name
  return out

/-- (helper) The authored declarations with their constant information, in
name order. -/
def declarations (env : Environment) (authored : NameSet) : Array (Name × ConstantInfo) := Id.run do
  let mut out := #[]
  for (name, info) in env.constants.toList do
    if authored.contains name then
      out := out.push (name, info)
  return out.qsort (fun a b => a.1.toString < b.1.toString)

/-- (helper) Trim ASCII whitespace from both ends of a string. -/
def trimmed (s : String) : String := s.trimAscii.toString

/-- (helper) The names of an array, without duplicates and in the order they
first occur. -/
def dedup (names : Array Name) : Array Name :=
  names.foldl (init := (#[] : Array Name)) fun acc n => if acc.contains n then acc else acc.push n

/-! ## What an entry prints -/

/-- (helper) Does this type end in a sort — is the declaration a type
abbreviation or a predicate, whose body is part of what a statement using it
says? -/
def isTypeLike (type : Lean.Expr) : Bool := type.getForallBody.isSort

/-- (helper) The most lines of body an entry prints for a definition that is
not itself a type. Above the limit the entry is the signature and the
doc-comment, and the body is read in the module named beside it: `eval`,
`check` and `explain` are what the limit is for. -/
def maxBodyLines : Nat := 15

/-- (helper) Does this value read as the elaborator's output rather than as
what was written — a `brecOn` application or a well-founded fixpoint? Such a
body is printed from the equations Lean derived instead, which say the same
thing in the shape the source wrote it. -/
def looksCompiled (env : Environment) (value : Lean.Expr) : Bool :=
  value.getUsedConstants.any fun c =>
    isAuxRecursor env c || isRecCore env c
      || c == ``WellFounded.fix || c == ``WellFounded.fixF

/-- (helper) Pretty-print a declaration's signature at the report's width. -/
def ppDecl (name : Name) : MetaM String := do
  let sig ← PrettyPrinter.ppSignature name
  return sig.fmt.pretty 78

/-- (helper) Pretty-print a definition's value under the binders its
signature already names. -/
def ppValue (value : Lean.Expr) : MetaM String :=
  Meta.lambdaTelescope value fun _ body => do
    return (← Meta.ppExpr body).pretty 74

/-- (helper) The defining equations Lean derived for a definition, each
pretty-printed, together with the constants they mention. -/
def ppEquations (name : Name) : MetaM (Array String × Array Name) := do
  match ← Meta.getEqnsFor? name with
  | none => return (#[], #[])
  | some eqs => do
      let mut texts := #[]
      let mut uses := #[]
      for e in eqs do
        let info ← getConstInfo e
        texts := texts.push ((← Meta.ppExpr info.type).pretty 74)
        uses := uses ++ info.type.getUsedConstants
      return (texts, uses)

/-- (helper) What an entry prints below a declaration's signature, and the
constants that printed text draws on. -/
structure Body where
  /-- The `:=` body, already split into lines; empty when none is printed. -/
  value : List String
  /-- The defining equations, when the compiled value is not what was
  written; empty otherwise. -/
  equations : Array String
  /-- The constants the printed text draws on. -/
  uses : Array Name
deriving Inhabited

/-- (helper) An entry that prints its signature and nothing below it. -/
def signatureOnly : Body := { value := [], equations := #[], uses := #[] }

/-- (helper) The body an entry prints for a constant. A type or a predicate
prints its body whatever its size, because that body is part of what a
statement using it says. Any other definition prints its body when it is
short enough to read — a signature alone cannot tell `Ty.mult` from
`fun _ => .copy`, nor `Ctx.join` from `fun _ _ => none`, and those are
exactly the definitions that decide whether the linearity theorems say
anything — and prints the derived equations when the compiled value is a
`brecOn` term rather than what was written. -/
def bodyOf (env : Environment) (name : Name) (info : ConstantInfo) : MetaM Body := do
  match info with
  | .defnInfo v =>
      if isTypeLike v.type then
        let text ← ppValue v.value
        return { value := text.splitOn "\n", equations := #[], uses := v.value.getUsedConstants }
      else
        let (eqs, eqUses) ← ppEquations name
        let eqLines := eqs.foldl (init := 0) fun acc e => acc + (e.splitOn "\n").length
        if !eqs.isEmpty then
          if eqLines ≤ maxBodyLines then
            return { value := [], equations := eqs, uses := eqUses }
          return signatureOnly
        if looksCompiled env v.value then
          return signatureOnly
        let text ← ppValue v.value
        let lines := text.splitOn "\n"
        if lines.length ≤ maxBodyLines then
          return { value := lines, equations := #[], uses := v.value.getUsedConstants }
        return signatureOnly
  | _ => return signatureOnly

/-! ## Dependencies of an entry -/

/-- (helper) The constants an entry shows: its type, its constructors' types
if it is an inductive, and whatever its printed body draws on. A theorem's
proof is never read. -/
def statementDeps (env : Environment) (info : ConstantInfo) (body : Body) : Array Name :=
  let fromCtors :=
    match info with
    | .inductInfo v =>
        v.ctors.foldl (init := #[]) fun acc c =>
          match env.find? c with
          | some ci => acc ++ ci.type.getUsedConstants
          | none => acc
    | _ => #[]
  info.type.getUsedConstants ++ body.uses ++ fromCtors

/-- (helper) A dependency as the reports name it: a constructor stands for
its inductive, and anything outside the mechanization or generated by Lean is
dropped. -/
def normalizeDep (env : Environment) (authored : NameSet) (n : Name) : Option Name :=
  let n := match env.find? n with
    | some (.ctorInfo v) => v.induct
    | _ => n
  if authored.contains n then some n else none

/-! ## Reading one declaration -/

/-- (helper) One declaration as a report prints it. -/
structure Item where
  /-- The declaration's fully qualified name. -/
  name : Name
  /-- `theorem`, `def`, `abbrev`, `inductive`, `structure`, `axiom`, … -/
  kind : String
  /-- The module it is declared in. -/
  module : Name
  /-- Import index and source line, for source order. -/
  order : Nat × Nat
  /-- Its doc-comment, verbatim. -/
  doc : Option String
  /-- Its fully elaborated signature, with the body when the entry prints
  one. -/
  signature : String
  /-- The defining equations, when the entry prints those instead. -/
  equations : Array String
  /-- For an inductive or a structure: each constructor's signature and
  doc-comment. -/
  ctors : Array (Name × String × Option String)
  /-- Its statement dependencies, normalized. -/
  deps : Array Name
  /-- Every `RueCore` constant its printed text draws on, before
  normalization: what the closure check is about. -/
  mentions : Array Name
  /-- Does its doc-comment mark it `(helper)`, the repository's
  cross-reference convention for a declaration that mechanizes nothing on its
  own (`README.md`)? -/
  helper : Bool
deriving Inhabited

/-- (helper) The declaration keyword a report prints for a constant. -/
def kindOf : ConstantInfo → String
  | .axiomInfo _ => "axiom"
  | .thmInfo _ => "theorem"
  | .opaqueInfo _ => "opaque"
  | .quotInfo _ => "quotient primitive"
  | .inductInfo _ => "inductive"
  | .ctorInfo _ => "constructor"
  | .recInfo _ => "recursor"
  | .defnInfo v => if isTypeLike v.type && v.hints.isAbbrev then "abbrev" else "def"

/-- (helper) The source line a declaration starts on, or 0 when the
environment records no range for it. -/
def lineOf (name : Name) : CoreM Nat := do
  match ← findDeclarationRanges? name with
  | some ranges => return ranges.range.pos.line
  | none => return 0

/-- (helper) Read one declaration out of the environment. -/
def readItem (env : Environment) (authored : NameSet) (name : Name) (info : ConstantInfo) :
    CoreM Item := do
  let doc := (← findDocString? env name).map trimmed
  let head ← Meta.MetaM.run' (ppDecl name)
  let body ← Meta.MetaM.run' (bodyOf env name info)
  let signature :=
    if body.value.isEmpty then s!"{kindOf info} {head}"
    else s!"{kindOf info} {head} :=\n  " ++ String.intercalate "\n  " body.value
  let moduleIdx := (env.getModuleIdxFor? name).map (·.toNat) |>.getD 0
  let module := env.header.moduleNames[moduleIdx]!
  let line ← lineOf name
  let mut ctors := #[]
  if let .inductInfo v := info then
    for c in v.ctors do
      let csig ← Meta.MetaM.run' (ppDecl c)
      let cdoc ← findDocString? env c
      ctors := ctors.push (c, csig, cdoc)
  let raw := statementDeps env info body
  let deps := dedup ((raw.filterMap (normalizeDep env authored)).filter (· != name))
  return {
    name, kind := kindOf info, module, order := (moduleIdx, line), doc, signature,
    equations := body.equations, ctors,
    deps := deps.qsort (fun a b => a.toString < b.toString),
    mentions := dedup (raw.filter inRoot),
    helper := ((doc.getD "").splitOn "(helper)").length > 1 }

/-- (helper) Every declaration reachable from `seeds` through statement
dependencies, read: the constants a set of statements is written in terms
of. -/
partial def itemClosure (env : Environment) (authored : NameSet) :
    List Name → Array Item → CoreM (Array Item)
  | [], acc => return acc
  | n :: rest, acc =>
      if acc.any (·.name == n) then itemClosure env authored rest acc
      else
        match env.find? n with
        | none => itemClosure env authored rest acc
        | some info => do
            let it ← readItem env authored n info
            itemClosure env authored (it.deps.toList ++ rest) (acc.push it)

/-! ## Ordering -/

/-- (helper) Source order: by import position, then by line, then by name, so
a report reads in the order the sources do. -/
def bySource (a b : Item) : Bool :=
  if a.order.1 != b.order.1 then a.order.1 < b.order.1
  else if a.order.2 != b.order.2 then a.order.2 < b.order.2
  else a.name.toString < b.name.toString

/-- (helper) A deterministic topological order: dependencies before
dependents, with a name tiebreak among the declarations that are ready, so
the generated file is stable under anything but a real change. -/
partial def topological (items : Array Item) : Array Item := Id.run do
  let names := items.map (·.name)
  let mut remaining := items.qsort (fun a b => a.name.toString < b.name.toString)
  let mut emitted : Array Name := #[]
  let mut out : Array Item := #[]
  while remaining.size > 0 do
    let ready := remaining.filter fun it =>
      it.deps.all fun d => !names.contains d || emitted.contains d
    -- A dependency cycle (mutual definitions) would leave nothing ready;
    -- fall back to the name order already imposed, rather than looping.
    let batch := if ready.size == 0 then remaining.extract 0 1 else ready
    for it in batch do
      out := out.push it
      emitted := emitted.push it.name
    remaining := remaining.filter fun it => !emitted.contains it.name
  return out

/-! ## The claims the reports make about themselves -/

/-- (helper) Does the pretty printer render this constant as something other
than its own name? A matcher prints as a `match`, a recursor and the
brecOn-shaped auxiliaries print as the `match` or the equation they came
from, an internal name never prints at all, an instance is an instance
argument and is elided (the `DecidableEq` a `deriving` clause makes, which
`a.st = b.st` carries and does not show), and a field projection prints as
the dot notation its structure's entry already explains. Those are the
constants that occur in a printed term but not in printed text; nothing else
is exempt from closure — in particular, a constant the generated-declaration
filter drops is not, because the filter is exactly what the check is here to
keep honest. -/
def printerHides (env : Environment) (n : Name) : Bool :=
  n.isInternalDetail || Meta.isMatcherCore env n || Meta.isInstanceCore env n
    || isAuxRecursor env n || isRecCore env n || isNoConfusion env n
    || env.isProjectionFn n || isFieldProjection env n

/-- (helper) The closure property, checked. Every `RueCore` constant the
digest prints — in a signature, in a printed body, in a constructor's type —
is an entry of the digest, or a constructor listed under its type's entry, or
one of the constants `printerHides` accounts for. Checking this is what keeps
a later change to the filter or to the seeds from quietly voiding the
sentence the file opens with: a definition dropped from the report while a
statement still mentions it fails here, whatever the reason it was
dropped. -/
def closureViolations (env : Environment) (entries : NameSet)
    (items : Array Item) : Array String := Id.run do
  let mut out := #[]
  for it in items do
    for m in it.mentions do
      if entries.contains m then continue
      if let some (.ctorInfo v) := env.find? m then
        if entries.contains v.induct then continue
      if printerHides env m then continue
      out := out.push ("`" ++ toString it.name ++ "` mentions `" ++ toString m ++
        "`, which has no entry and is no listed constructor")
  return out

/-- (helper) A dotted string as a `Name`. -/
def nameOf (s : String) : Name :=
  (s.splitOn ".").foldl (init := Name.anonymous) fun n part => Name.mkStr n part

/-- (helper) Every declaration `INDEX.md` names, paired with the module it
names it under: the rows of its declarations table, whose second backticked
cell is the declaration, and the helper list under that table, whose first
backticked name is the module and whose rest are declarations. -/
def indexDeclarations (index : String) : Array (Name × Name) := Id.run do
  let mut out := #[]
  for line in index.splitOn "\n" do
    let parts := line.splitOn "`"
    if line.startsWith "| `" then
      if parts.length > 3 then
        out := out.push (nameOf (parts.getD 1 ""), nameOf (parts.getD 3 ""))
    else if line.startsWith "- `" then
      let mut i := 3
      while i < parts.length do
        out := out.push (nameOf (parts.getD 1 ""), nameOf (parts.getD i ""))
        i := i + 2
  return out

/-- (helper) The mechanization's modules this report actually imported.
`INDEX.md` also indexes the executables' own modules (`RueCore.DigestMain`
and its siblings), which the `RueCore` library does not import and which are
not what these reports are about. -/
def importedModules (env : Environment) : NameSet :=
  env.header.moduleNames.foldl (init := NameSet.empty) fun acc m =>
    if inRoot m then acc.insert m else acc

/-- (helper) `INDEX.md` against the compiled environment: the completeness
claim, checked instead of asserted. The index is generated from the *sources*
by `scripts/validate-lean-xref-index.py` and this report is read from the
*environment*, so the two are independent readings of the same package. Every
declaration the index names in a module this report imports must be a
constant of the environment, must survive the generated-declaration filter,
and — when it is a theorem or an axiom, which the digest lists
unconditionally — must have an entry here. -/
def indexCrossCheck (env : Environment) (authored : NameSet) (entries : NameSet)
    (index : String) : Array String := Id.run do
  let modules := importedModules env
  let mut out := #[]
  for (module, decl) in indexDeclarations index do
    if modules.contains module && inRoot decl then
      match env.find? decl with
      | none =>
          out := out.push ("INDEX.md lists `" ++ toString decl ++ "` in `" ++ toString module ++
            "`, which the compiled environment does not contain")
      | some info =>
          if !authored.contains decl then
            out := out.push ("INDEX.md lists `" ++ toString decl ++
              "` as an authored declaration, which this report's filter drops as generated")
          else if (info matches .thmInfo _ || info matches .axiomInfo _) &&
              !entries.contains decl then
            out := out.push ("INDEX.md lists `" ++ toString decl ++
              "`, which has no entry in this report")
  return out

/-! ## Rendering -/

/-- (helper) A declaration's name without the `RueCore.` prefix both reports
are scoped to. -/
def shortName (n : Name) : String := (n.replacePrefix root .anonymous).toString

/-- (helper) A doc-comment folded onto one line, for a constructor's row. -/
def oneLine (s : String) : String :=
  String.intercalate " " (s.splitOn "\n" |>.map trimmed |>.filter (· != ""))

/-- (helper) One declaration, as a section of `DIGEST.md`. -/
def renderItem (it : Item) : List String :=
  let header := s!"### `{shortName it.name}`"
  let provenance := s!"*{it.kind}* · module `{it.module}`"
  let doc := match it.doc with
    | some d => ["", d]
    | none => ["", "*(no doc-comment)*"]
  let signature := ["", "```lean", it.signature, "```"]
  let equations :=
    if it.equations.isEmpty then []
    else
      ["", "Defining equations, as Lean derived them from the body:", "", "```lean"]
        ++ (String.intercalate "\n" it.equations.toList).splitOn "\n" ++ ["```"]
  let ctors :=
    if it.ctors.isEmpty then []
    else
      ["", "Constructors:"] ++ (it.ctors.toList.flatMap fun (c, sig, cdoc) =>
        let note := match cdoc with
          | some d => s!" — {oneLine d}"
          | none => ""
        ["", s!"**`{shortName c}`**{note}", "", "```lean", sig, "```"])
  [header, "", provenance] ++ doc ++ signature ++ equations ++ ctors ++ [""]

/-- (helper) The coverage sentence `INDEX.md` states under one of its
inverse tables. The digest quotes the generated index rather than recounting
the fragment boundary itself. -/
def coverageLine (index : String) (heading : String) : Option String :=
  let lines := index.splitOn "\n"
  let rec after : List String → Option String
    | [] => none
    | l :: rest =>
        if trimmed l == heading then
          (rest.find? (fun r => r.startsWith "Coverage:")).map
            (fun r => trimmed ((r.splitOn "Coverage:").getD 1 r))
        else after rest
  after lines

/-- (helper) The lines of one section of `INDEX.md`, between its heading and
the next one. -/
partial def sectionLines : List String → String → Bool → List String
  | [], _, _ => []
  | l :: rest, heading, inside =>
      if inside then
        if l.startsWith "## " then [] else l :: sectionLines rest heading true
      else if trimmed l == heading then sectionLines rest heading true
      else sectionLines rest heading false

/-- (helper) The grammar forms `INDEX.md` marks *(partial)*: a form the
fragment has a restricted or abstract version of rather than the form itself.
The coverage line counts them; the digest names them, so that "have a core
image" is not read as that many whole forms. -/
def partialForms (index : String) : List String :=
  (sectionLines (index.splitOn "\n") "## Abstract syntax forms → declarations" false).filterMap
    fun l =>
      if l.startsWith "| `" then
        let cells := l.splitOn "|"
        if ((cells.getD 3 "").splitOn "*(partial)*").length > 1 then
          some (trimmed ((cells.getD 2 "").replace "`" ""))
        else none
      else none

/-- (helper) Fold words into lines no wider than `width`, so a sentence the
digest assembles from the index wraps like the hand-written ones around
it. -/
def wrapWords (width : Nat) (words : List String) : List String :=
  (words.foldl (init := ([] : List String)) fun acc w =>
    match acc with
    | [] => [w]
    | line :: rest =>
        if line.length + 1 + w.length ≤ width then (line ++ " " ++ w) :: rest
        else w :: line :: rest).reverse

/-- (helper) The two coverage sentences and the forms behind the *(partial)*
count, or an error naming what is missing: a digest that cannot state its own
scope is not one an expert can use. -/
def scopeSection (index : String) : Except String (List String) := do
  let rules ← (coverageLine index "## Calculus rules → declarations").elim
    (.error "INDEX.md has no coverage line under '## Calculus rules → declarations'") pure
  let forms ← (coverageLine index "## Abstract syntax forms → declarations").elim
    (.error "INDEX.md has no coverage line under '## Abstract syntax forms → declarations'") pure
  let partials := partialForms index
  let partialSentence :=
    if partials.isEmpty then []
    else
      -- Each form is one word to the wrapper, so a form with a space in it
      -- (`int(w, s)`) is never broken across a line.
      let listed := partials.mapIdx fun i f =>
        s!"`{f}`" ++ (if i + 1 == partials.length then "." else ",")
      let tail :=
        "Each is a restricted or abstract stand-in rather than the form itself," ++
        " and `INDEX.md` says in the row what is missing. A form the fragment" ++
        " abstracts away rather than models reads *not yet mechanized* there even" ++
        " where a construct of the core stands in for part of its ownership shape," ++
        " so this count is the generous reading of neither."
      let words :=
        "The forms that count as partial are".splitOn " " ++ listed ++
        (tail.splitOn " ").filter (· != "")
      "" :: wrapWords 74 words
  return [
    "## Scope",
    "",
    "The theorems below are about a *fragment* of the core calculus",
    "(`../01-core-calculus.md`). The generated `INDEX.md` states the boundary",
    "rule by rule and form by form; its two coverage lines, quoted here so the",
    "boundary is visible before the statements are:",
    "",
    s!"- *Calculus rules → declarations*: {rules}",
    s!"- *Abstract syntax forms → declarations*: {forms}"] ++ partialSentence ++ [
    "",
    "A row reading *not yet mechanized* in those tables is a rule or a syntactic",
    "form no theorem below says anything about. Nothing in this file claims",
    "otherwise, and nothing outside the fragment is proved by omission.",
    ""]

/-- (helper) `DIGEST.md`: the theorems, the helper lemmas, and the
definitions their statements are written in terms of. -/
def renderDigest (index : String) (theorems helpers definitions : Array Item) :
    Except String String := do
  let scope ← scopeSection index
  let preamble := [
    "# RueCore statement digest",
    "",
    "<!-- Generated by `lake exe ruecore-digest`; do not edit by hand. -->",
    "",
    "Every theorem the mechanization proves, with the statement Lean checked —",
    "elaborated, not transcribed — and every definition those statements are",
    "written in terms of. Read it to decide what is claimed, before deciding",
    "whether to believe it; `TRUST.md` (`lake exe ruecore-digest --trust`) says",
    "what each proof rests on, and `GUIDE.md` has a thirty-minute validation",
    "procedure that uses both.",
    "",
    "What is *not* here, deliberately: proof bodies. A proof is checked by the",
    "kernel, and `TRUST.md` reports the axioms that check appealed to; reading",
    "the tactic script is not how this is validated. A definition's body *is*",
    "here whenever it is a type or a predicate (`Ctx`, `CellMatches`,",
    "`InBounds`) or is short enough to read, because a signature alone cannot",
    "tell `Ty.mult` from `fun _ => .copy` or `Ctx.join` from `fun _ _ => none`,",
    "and under either of those the linearity claims below would be nearly",
    "vacuous. A long body (`eval`, `check`, `explain`) is left to the module",
    "named beside its signature. Where the compiled value is the elaborator's",
    "own output rather than what was written, the entry prints the defining",
    "equations Lean derived from it.",
    "",
    "Two properties of this file are checked by the generator, which exits",
    "non-zero and names the miss rather than printing a file whose preamble is",
    "false. First, closure: every `RueCore` constant occurring in a signature or",
    "a body printed below has an entry of its own below, or is a constructor",
    "listed under its type's entry. Second, completeness against `INDEX.md`,",
    "which a different tool generates by reading the sources rather than the",
    "compiled environment: every declaration it names in a module this file",
    "covers is a constant that survived the generated-declaration filter, and",
    "every theorem it names has an entry here.",
    "",
    "Doc-comments are reproduced verbatim from the sources, and cite the",
    "calculus rule, section, or specification paragraph the declaration",
    "mechanizes (`README.md`, \"Doc-comment convention\"); `INDEX.md` is the",
    "generated inverse of those citations.",
    ""]
  let theoremSection := [
    "## Theorems",
    "",
    "In source order. What separates this section from the next is the",
    "`(helper)` marker in the doc-comment — the repository's cross-reference",
    "convention — and not a judgment about importance: a theorem whose",
    "doc-comment does not carry the marker is listed here even where it is a",
    "step of another proof (an inversion lemma about the join, say) rather than",
    "a claim about the language. Each statement and its doc-comment say which",
    "it is. A claim's meaning is its statement together with the definitions in",
    "the last section, not its doc-comment.",
    ""] ++ theorems.toList.flatMap renderItem
  let helperSection := [
    "## Helper lemmas",
    "",
    "Also proved, and listed so that nothing proved is hidden, but each is a",
    "step of another proof rather than a claim about the language: these are",
    "the declarations whose doc-comment marks them `(helper)` under the",
    "repository's cross-reference convention.",
    ""] ++ helpers.toList.flatMap renderItem
  let definitionSection := [
    "## Definitions the statements depend on",
    "",
    "In dependency order — every definition appears after everything its own",
    "statement mentions — with ties broken by name, so this file changes only",
    "when the mechanization does.",
    ""] ++ definitions.toList.flatMap renderItem
  return String.intercalate "\n"
    (preamble ++ scope ++ theoremSection ++ helperSection ++ definitionSection)

/-! ## The trust report -/

/-- (helper) Lean's two kernel-checked assumptions that this project's policy
allows: propositional extensionality and the soundness of quotient types. -/
def allowedAxioms : List Name := [``propext, ``Quot.sound]

/-- (helper) What one axiom means for the trust boundary, and whether its
presence should fail the report. -/
def axiomVerdict (a : Name) : String × Bool :=
  if allowedAxioms.contains a then ("standard, allowed by this project's policy", false)
  else if a == ``Classical.choice then
    ("kernel-checked, but outside this project's policy of constructive proofs", true)
  else if a == ``sorryAx then
    ("**hole**: an unfinished proof — this is what `sorry` leaves behind", true)
  else if a == ``Lean.ofReduceBool || a == ``Lean.ofReduceNat then
    ("**hole**: `native_decide` — a result the kernel did not verify itself", true)
  else if inRoot a then
    ("a declared assumption of this package; read its doc-comment below", false)
  else ("**unknown axiom** — review it before trusting anything that uses it", true)

/-- (helper) `TRUST.md`: every theorem with the axioms it actually depends
on, the `sorry` count read from those axioms, the package's own declared
assumptions, and the toolchain that checked all of it. The `Bool` is false
when the report contains something the reader must act on. -/
def renderTrust (theorems : Array (Item × Array Name)) (declared : Array Item) :
    String × Bool :=
  let flagged := theorems.filterMap fun (it, axs) =>
    let bad := axs.filter fun a => (axiomVerdict a).2
    if bad.isEmpty then none else some (it.name, bad)
  let sorryCount := (theorems.filter fun (_, axs) => axs.contains ``sorryAx).size
  let usedAxioms := theorems.foldl (init := (#[] : Array Name)) fun acc (_, axs) =>
    axs.foldl (init := acc) fun acc a => if acc.contains a then acc else acc.push a
  let verdict :=
    if flagged.isEmpty then
      ["Every theorem depends only on axioms this project's policy allows, and",
       "on nothing else. There is no `sorry` and no `native_decide` anywhere in",
       "the mechanization."]
    else
      ["**This report needs attention.** These theorems depend on an axiom",
       "outside the policy:",
       ""] ++ (flagged.toList.map fun (n, bad) =>
        s!"- `{shortName n}`: " ++ String.intercalate ", " (bad.toList.map (s!"`{·}`")))
  let header := [
    "# RueCore trust report",
    "",
    "<!-- Generated by `lake exe ruecore-digest --trust`; do not edit by hand. -->",
    "",
    "What each proved statement rests on. A Lean proof's trust boundary is the",
    "set of axioms the kernel appealed to while checking it, which",
    "`Lean.collectAxioms` reads out of the compiled environment — so the `sorry`",
    "count below is taken from the axioms, not from grepping the sources, and a",
    "`sorry` hidden behind a macro or an unfinished helper would still appear.",
    "Which declarations are the package's own is asked of the environment too",
    "(`RueCore/Digest.lean`), not guessed from their names, so a theorem cannot",
    "leave this table by being called `Ty.congr`. `DIGEST.md` says what the",
    "statements are.",
    "",
    s!"- Toolchain: Lean {Lean.versionString} (the pin in `lean-toolchain` and in",
    "  `toolchains/lean/defs.bzl`, held equal by",
    "  `scripts/validate-lean-toolchain-pin.py`).",
    s!"- Theorems checked: {theorems.size}.",
    s!"- Proofs depending on `sorryAx`: {sorryCount}.",
    s!"- Axioms declared by this package: {declared.size}.",
    s!"- Distinct axioms used: " ++
      (if usedAxioms.isEmpty then "none."
       else String.intercalate ", "
         ((usedAxioms.qsort (fun a b => a.toString < b.toString)).toList.map (s!"`{·}`")) ++ "."),
    ""] ++ verdict ++ [""]
  let policy := [
    "## The policy, and what would break it",
    "",
    "Lean's logic has three standard axioms, all kernel-checked assumptions",
    "rather than holes: `propext` (propositional extensionality), `Quot.sound`",
    "(quotient soundness), and `Classical.choice`. This project uses the first",
    "two and not the third, so its proofs are constructive; `Classical.choice`",
    "is reported as a policy break rather than as an unsound step. Two other",
    "things are genuine holes: `sorryAx`, which is what an unfinished proof",
    "leaves behind, and `Lean.ofReduceBool`/`Lean.ofReduceNat`, which",
    "`native_decide` introduces for a result the kernel did not verify itself.",
    "An axiom this package declares is neither: it is a stated assumption, and",
    "it is listed with its doc-comment below so a reader can judge the source",
    "it comes from.",
    "",
    "The Buck target `root//:lean-ruecore` checks the same boundary from the",
    "other side: it re-checks the compiled modules with the toolchain's own",
    "`leanchecker`, prints `#print axioms` for the theorems it names in `trust`,",
    "and fails the build on any axiom outside `propext`/`Quot.sound`. This",
    "report covers *every* theorem, including the ones no trusted theorem uses.",
    "Neither check runs in CI yet: nothing in CI runs the Lean build until",
    "ADR-0097's gate is met (RUE-2241), so a reviewer regenerates both reports",
    "and diffs them against the committed copies.",
    ""]
  let table := [
    "## Every theorem and its axioms",
    "",
    "| Theorem | Module | Axioms |",
    "| --- | --- | --- |"] ++ (theorems.toList.map fun (it, axs) =>
      let cell :=
        if axs.isEmpty then "*none*"
        else String.intercalate ", " (axs.toList.map (s!"`{·}`"))
      s!"| `{shortName it.name}` | `{it.module}` | {cell} |") ++ [""]
  let legend := [
    "What each axiom that appears above means:",
    ""] ++ (usedAxioms.qsort (fun a b => a.toString < b.toString)).toList.map (fun a =>
      s!"- `{a}` — {(axiomVerdict a).1}") ++ [""]
  let assumptions :=
    if declared.isEmpty then
      ["## Declared assumptions",
       "",
       "None. The package declares no axiom of its own, so nothing here is",
       "assumed beyond Lean's logic. When the project's obligation interfaces",
       "arrive (the library obligations of §6.13.5, and the adequacy obligation",
       "`../03-metatheory.md` records), each will appear in this section with",
       "its doc-comment, which is where its source belongs.",
       ""]
    else
      ["## Declared assumptions",
       "",
       "Assumed, not proved. Each is an axiom this package declares; its",
       "doc-comment states where the assumption comes from and who owes the",
       "proof.",
       ""] ++ declared.toList.flatMap renderItem
  (String.intercalate "\n" (header ++ policy ++ table ++ legend ++ assumptions), flagged.isEmpty)

end RueCore.Digest
