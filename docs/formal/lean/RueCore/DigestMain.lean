import RueCore.Lint

/-!
# `lake exe ruecore-digest` — the expert validation surface (RUE-2247)

```
ruecore-digest                 the statement digest, on stdout (DIGEST.md)
ruecore-digest --index <path>  the same, reading the scope counts elsewhere
ruecore-digest --trust         the trust report, on stdout (TRUST.md)
```

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
  "usage: ruecore-digest [--index <path>] | --trust"

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
  | _ => IO.eprintln usage; pure 1

/-- (helper) The entry point's safe face. -/
@[implemented_by mainUnsafe] opaque mainImpl (args : List String) : IO UInt32

/-- (helper) `lake exe ruecore-digest`: the statement digest, or, with
`--trust`, the trust report. -/
def main (args : List String) : IO UInt32 := mainImpl args
