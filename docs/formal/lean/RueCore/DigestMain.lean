import RueCore.Lint

/-!
# `lake exe ruecore-digest` — the expert validation surface (RUE-2247)

```
ruecore-digest                 the statement digest, on stdout (DIGEST.md)
ruecore-digest --index <path>  the same, reading the scope counts elsewhere
ruecore-digest --trust         the trust report, on stdout (TRUST.md)
ruecore-digest --spine         the spine: every Spec statement (SPINE.md)
ruecore-digest --challenge     Lean Comparator's challenge (comparator/Challenge.lean)
ruecore-digest --comparator-config   its configuration (comparator/config.json)
```

The last three are generated from the Spec layer's list, `RueCore.Spec.spine`
(RUE-2460), and exit non-zero when the lint's spine check
(`RueCore.Lint.spineProblems`) finds the list and the environment disagree.

The trust report ends with the headline statements' trusted base, computed by
`RueCore.Lint.trustedBase`, the pass `lake exe ruecore-lint` runs (RUE-2457).

Both reports are generated from the compiled `RueCore` environment, so
regenerating them and diffing against the committed `DIGEST.md` / `TRUST.md`
is how a reviewer checks that neither has drifted (RUE-2241 defers a CI gate
for that diff; nothing in CI runs the Lean build yet).

`--trust` exits non-zero when a proof depends on an axiom outside this
project's policy, after writing the report, so a build step that redirects it
to a file both keeps the evidence and fails. The digest exits non-zero the
same way when one of the two properties it states about itself — closure, and
completeness against `INDEX.md` — does not hold, naming the miss on stderr.

Importing an environment at run time needs `enableInitializersExecution`, so
the entry point is the usual `unsafe`/`implemented_by` pair; nothing about
the mechanization itself is unsafe.
-/

open Lean RueCore

/-- (helper) How to call the executable. -/
def usage : String :=
  "usage: ruecore-digest [--index <path>] | --trust | --spine | --challenge | --comparator-config"

/-- (helper) The statement digest: theorems, helper lemmas, and the
definitions their statements are written in terms of. -/
def digestReport (index : String) (env : Environment) : CoreM (String × UInt32) := do
  let authored ← Digest.authoredNames env
  let decls := Digest.declarations env authored
  let mut theorems := #[]
  let mut helpers := #[]
  for (name, info) in decls do
    if info matches .thmInfo _ then
      let item ← Digest.readItem env authored name info
      if item.helper then helpers := helpers.push item else theorems := theorems.push item
  let seeds := (theorems ++ helpers).flatMap (·.deps)
  let reached ← Digest.itemClosure env authored seeds.toList #[]
  let definitions := reached.filter fun it => it.kind != "theorem"
  let rendered := theorems ++ helpers ++ definitions
  let entries := rendered.foldl (init := NameSet.empty) fun acc it => acc.insert it.name
  -- The two properties the file's own preamble states, checked before it is
  -- printed: a report that cannot keep them should say so rather than assert
  -- them (RUE-2247's review).
  let problems := Digest.closureViolations env entries rendered
    ++ Digest.indexCrossCheck env authored entries index
  for problem in problems do
    IO.eprintln s!"ruecore-digest: {problem}"
  match Digest.renderDigest index
      (theorems.qsort Digest.bySource) (helpers.qsort Digest.bySource)
      (Digest.topological definitions) with
  | .ok text => return (text, if problems.isEmpty then 0 else 1)
  | .error why =>
      IO.eprintln s!"ruecore-digest: {why}"
      return ("", 1)

/-- (helper) The trust report: every theorem's axioms, the `sorry` count read
from them, and the package's own declared assumptions. -/
def trustReport (env : Environment) : CoreM (String × UInt32) := do
  let authored ← Digest.authoredNames env
  let decls := Digest.declarations env authored
  let mut theorems := #[]
  let mut declared := #[]
  for (name, info) in decls do
    if info matches .thmInfo _ then
      let item ← Digest.readItem env authored name info
      let axioms ← collectAxioms name
      theorems := theorems.push (item, axioms.qsort (fun a b => a.toString < b.toString))
    else if info matches .axiomInfo _ then
      declared := declared.push (← Digest.readItem env authored name info)
  let ordered := theorems.qsort (fun a b => Digest.bySource a.1 b.1)
  let (text, clean) := Digest.renderTrust ordered (declared.qsort Digest.bySource)
  if !clean then
    IO.eprintln "ruecore-digest --trust: a proof depends on an axiom outside the policy"
  let base ← Lint.trustedBase env
  for h in base.missing do
    IO.eprintln s!"ruecore-digest --trust: headline statement {h} is not a theorem of the environment"
  let text := text ++ "\n" ++ String.intercalate "\n" (Lint.renderTrustedBase base)
  return (text, if clean && base.missing.isEmpty then 0 else 1)

/-! ## The spine (RUE-2460) -/

/-- (helper) The title a Spec module's docstring gives itself, after the
em dash: `RueCore.Spec.Safety — type safety over the interpreter (Spec
layer)` gives `type safety over the interpreter`. -/
def moduleTitle (env : Environment) (m : Name) : String :=
  let doc := (getModuleDoc? env m).bind (·[0]?) |>.map (·.doc) |>.getD ""
  let line := ((doc.splitOn "\n").find? (·.startsWith "# ")).getD s!"# {m}"
  let afterDash := (line.splitOn " — ").getD 1 line
  let title := (afterDash.splitOn " (Spec layer)").headD afterDash
  match title.toList with
  | c :: cs => String.ofList (c.toUpper :: cs)
  | [] => title

/-- (helper) The package definitions a statement names directly, a
constructor standing for its type, less instances and Lean's own
auxiliaries: the first things a reader looks up. -/
def directDefs (env : Environment) (body : Lean.Expr) : Array Name := Id.run do
  let mut out : Array Name := #[]
  for c in body.getUsedConstants do
    let c := match env.find? c with
      | some (.ctorInfo v) => v.induct
      | _ => c
    if !Lint.inPackage env c || out.contains c then continue
    if Meta.isInstanceCore env c || Lint.isLeanAux env c then continue
    out := out.push c
  return out

/-- (helper) `SPINE.md`: every Spec statement — the Lean statement, its
English reading and calculus paragraph (its doc-comment), the theorem that
proves it, and the definitions it rests on — generated from
`RueCore.Spec.spine`, so it cannot drift from what the kernel checks. -/
def spineReport (env : Environment) : CoreM (String × UInt32) := do
  let problems := Lint.spineProblems env
  for p in problems do IO.eprintln s!"ruecore-digest --spine: {p}"
  let base ← Lint.trustedBase env
  let mut out : Array String := #[
    "# The spine: what the mechanization claims",
    "",
    "Generated by `lake exe ruecore-digest --spine` from the Spec layer",
    "(`RueCore/Spec.lean`, RUE-2460); do not edit, regenerate it whenever a Spec",
    "statement or its doc-comment changes.",
    "",
    "The claim, statement by statement: read this first. Each entry is a",
    "`def …_stmt : Prop` of the Spec layer, written over layers L0 and L1 alone,",
    "with its English reading and the `../01-core-calculus.md` §7 paragraph it",
    "realizes (and where it is narrower). `RueCore/Spine.lean` restates each",
    "theorem `RueCore.X` as `RueCore.Spine.X : RueCore.Spec.X_stmt := @RueCore.X`,",
    "so the kernel checks every proof against its statement; `ruecore-lint` checks",
    "that the proof layer states each one word for word; Lean Comparator",
    "(`comparator/`, README \"The statement layer\") certifies the `Spine`",
    "theorems against a `sorry` challenge, with `propext` and `Quot.sound` only.",
    "",
    s!"A statement means its text plus the {base.definitions.size} definitions the {Lint.headline.length} statements unfold",
    "to (`TRUST.md`, \"Trusted base\"; bodies in `DIGEST.md`); \"Names\" lists",
    "those an entry mentions. `M : FloatModel` is the IEEE 754 laws assumed.",
    ""]
  let mut group : Option Name := none
  for (h, s) in Spec.spine do
    let some (.defnInfo v) := Lint.find? env s | continue
    let m := (Lint.moduleOf? env s).getD .anonymous
    if group != some m then
      group := some m
      out := out ++ #[s!"## {moduleTitle env m}", "", s!"`{m}`", ""]
    let doc := ((← findDocString? env s).map Digest.trimmed).getD "*(no doc-comment)*"
    let stmt ← Meta.MetaM.run' do
      return (← Meta.ppExpr v.value).pretty 90
    let thmModule := (Lint.moduleOf? env h).map toString |>.getD "?"
    let direct := directDefs env v.value
    let closure := Lint.readable env (Lint.unfoldClosure env v.value.getUsedConstants)
    out := out ++ #[s!"### `{Digest.shortName h}`", "",
      doc, "", "```lean", s!"def {Digest.shortName s} : Prop :=", s!"  {("\n  ".intercalate (stmt.splitOn "\n"))}", "```", "",
      s!"Proved by `{Digest.shortName h}` (`{thmModule}`). Names " ++
        ", ".intercalate (direct.toList.map (s!"`{Digest.shortName ·}`")) ++
        s!"; rests on {closure.size} definitions.", ""]
  return ("\n".intercalate out.toList, if problems.isEmpty then 0 else 1)

/-- (helper) Lean Comparator's challenge (`comparator/Challenge.lean`): each
`RueCore.Spine` theorem with its Spec statement as its type and `sorry` for
a proof, generated from `RueCore.Spec.spine`. -/
def challengeReport (env : Environment) : CoreM (String × UInt32) := do
  let problems := Lint.spineProblems env
  for p in problems do IO.eprintln s!"ruecore-digest --challenge: {p}"
  let mut out : Array String := #[
    "import RueCore.Spec",
    "",
    "/-!",
    "# Lean Comparator's challenge (RUE-2460)",
    "",
    "Generated by `lake exe ruecore-digest --challenge` from `RueCore.Spec.spine`; do",
    "not edit. One theorem per Spec statement, with the statement as its type and",
    "`sorry` for a proof. It imports the Spec layer and, through it, layers L0 and",
    "L1 only: the part of the package a reviewer trusts. `RueCore.Spine`, the",
    "solution, states the same names with the same types and proves them;",
    "`comparator/config.json` names them. How to run it: README, \"The statement",
    "layer\".",
    "-/",
    "",
    "namespace RueCore.Spine",
    ""]
  for (h, s) in Spec.spine do
    out := out.push s!"theorem {Digest.shortName h} : {s} := sorry"
  out := out ++ #["", "end RueCore.Spine", ""]
  return ("\n".intercalate out.toList, if problems.isEmpty then 0 else 1)

/-- (helper) Lean Comparator's configuration (`comparator/config.json`):
the challenge and solution modules, the theorems to compare — one per Spec
statement — and the axioms this project allows. -/
def comparatorConfig (env : Environment) : CoreM (String × UInt32) := do
  let problems := Lint.spineProblems env
  for p in problems do IO.eprintln s!"ruecore-digest --comparator-config: {p}"
  let names := Spec.spine.map fun (h, _) => Json.str (toString (Lint.spineName h))
  let json := Json.mkObj [
    ("challenge_module", "Challenge"),
    ("solution_module", "RueCore.Spine"),
    ("theorem_names", Json.arr names.toArray),
    ("permitted_axioms", Json.arr (Lint.allowedAxioms.map (Json.str ∘ toString)).toArray),
    ("enable_nanoda", false)]
  return (json.pretty ++ "\n", if problems.isEmpty then 0 else 1)

/-- (helper) Import the mechanization and run one report against the imported
environment, with names printed as the sources write them (the `RueCore`
namespace is open). -/
unsafe def withEnvironment (act : Environment → CoreM (String × UInt32)) : IO UInt32 := do
  initSearchPath (← findSysroot)
  enableInitializersExecution
  let env ← importModules #[{ module := `RueCore }] {} (trustLevel := 1024) (loadExts := true)
  let ctx : Core.Context :=
    { fileName := "<ruecore-digest>", fileMap := default, currNamespace := `RueCore }
  let ((text, code), _) ← (act env).toIO ctx { env := env }
  IO.print text
  return code

/-- (helper) The digest, with the scope section quoted from the generated
index at `path`. The index is read before the environment is imported, so a
missing one is a sentence rather than an exception. -/
unsafe def digestWithIndex (path : String) : IO UInt32 := do
  if !(← System.FilePath.pathExists path) then
    IO.eprintln s!"ruecore-digest: cannot read {path}"
    IO.eprintln "the digest quotes INDEX.md's coverage lines for its scope section; \
      run from docs/formal/lean, or pass --index <path>"
    return 1
  withEnvironment (digestReport (← IO.FS.readFile path))

/-- (helper) Print one of the two reports on stdout. -/
unsafe def mainUnsafe (args : List String) : IO UInt32 := do
  match args with
  | [] => digestWithIndex "INDEX.md"
  | ["--index", path] => digestWithIndex path
  | ["--trust"] => withEnvironment trustReport
  | ["--spine"] => withEnvironment spineReport
  | ["--challenge"] => withEnvironment challengeReport
  | ["--comparator-config"] => withEnvironment comparatorConfig
  | _ => IO.eprintln usage; pure 1

/-- (helper) The entry point's safe face. -/
@[implemented_by mainUnsafe] opaque mainImpl (args : List String) : IO UInt32

/-- (helper) `lake exe ruecore-digest`: the statement digest, or, with
`--trust`, the trust report. -/
def main (args : List String) : IO UInt32 := mainImpl args
