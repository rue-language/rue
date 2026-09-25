import Lean

/-!
# `lake exe ruecore-layers` — the layering audit (RUE-2456)

```
ruecore-layers            audit the package's import graph; exit 1 on a violation
```

The mechanization is layered so that what a claim depends on is small and
mechanically fixed (README, "Layers"):

* **L0 syntax** — `Float`, `Syntax`;
* **L1 definitions** — the semantics (`Statics`, `Dynamics`, `Step`) and the
  definitions the headline statements are written in (`*/Defs`);
* **L2 proofs** — the theorems and their proofs;
* **L3 tooling** — examples, witnesses, the printer, the corpus, the
  generator, the explain and digest reports, and the executables.

A module may import only modules of its own layer or a lower one. L3 may
import anything, and no module of L0–L2 imports L3. `table` below is the one
place a layer is declared.

The audit reads the import graph the build recorded, not the sources: each
module's imports come from its compiled `.olean` header (`readModuleData`),
starting from the library root and every `lean_exe` root of `lakefile.toml`.
It fails when

* a module imports a module of a higher layer;
* an L0–L2 module imports anything outside the package but Lean's `Init`;
* an L0–L2 module is not a `module` (the module system is adopted for the
  claim's layers: `module` headers, `public import`, `@[expose] public
  section`);
* a package module reached from the roots is missing from `table`, a `table`
  entry is not reached (stale), or a source file under `RueCore/` is not
  reached (built by nothing, so audited by nothing).

Run it from the package directory after `lake build`; a module without a
compiled `.olean` is reported rather than guessed at. Nothing here is about the
calculus, so the executable imports only `Lean` and builds in seconds.
-/

open Lean

namespace RueCore.Layers

/-- (helper) The layer table: every module of the package and its layer,
`0`–`3`. The one place a layer is declared. -/
def table : List (Name × Nat) := [
  -- L0 syntax
  (`RueCore.Float, 0),
  (`RueCore.Syntax, 0),
  -- L1 definitions: the semantics, and what the headline statements are written in
  (`RueCore.Statics, 1),
  (`RueCore.Dynamics, 1),
  (`RueCore.Step, 1),
  (`RueCore.Checker.Defs, 1),
  (`RueCore.Soundness.Defs, 1),
  (`RueCore.Trace.Defs, 1),
  (`RueCore.Adequacy.Defs, 1),
  -- L2 proofs
  (`RueCore.Soundness, 2),
  (`RueCore.Checker, 2),
  (`RueCore.Trace, 2),
  (`RueCore.Adequacy, 2),
  (`RueCore.TraceExact, 2),
  (`RueCore.TraceOrder, 2),
  -- L3 tooling
  (`RueCore.Examples, 3),
  (`RueCore.Witnesses, 3),
  (`RueCore.Print, 3),
  (`RueCore.Corpus, 3),
  (`RueCore.Gen, 3),
  (`RueCore.Explain, 3),
  (`RueCore.Explain.Ledger, 3),
  (`RueCore.Explain.Text, 3),
  (`RueCore.Explain.Html, 3),
  (`RueCore.Digest, 3),
  (`RueCore.CorpusMain, 3),
  (`RueCore.ExplainMain, 3),
  (`RueCore.DigestMain, 3),
  (`RueCore.LayersMain, 3),
  (`RueCore, 3)
]

/-- (helper) The roots the audit walks from: the library root and every
`lean_exe` root in `lakefile.toml`. -/
def roots : List Name :=
  [`RueCore, `RueCore.CorpusMain, `RueCore.ExplainMain, `RueCore.DigestMain, `RueCore.LayersMain]

/-- (helper) A layer's name, as the README's table writes it. -/
def layerName : Nat → String
  | 0 => "L0 syntax"
  | 1 => "L1 definitions"
  | 2 => "L2 proofs"
  | _ => "L3 tooling"

/-- (helper) Is this module one of the package's? -/
def inPackage (m : Name) : Bool := (`RueCore).isPrefixOf m

/-- (helper) The layer `table` declares for a module, if any. -/
def layerOf? (m : Name) : Option Nat := (table.find? (·.1 == m)).map (·.2)

/-- (helper) How an import is written: `public`, `meta` and `all` as the
module system spells them; a plain `import` of a non-`module` file is public. -/
def importKind (i : Import) : String :=
  (if i.isMeta then "meta " else "") ++ (if i.isExported then "public" else "private") ++
    (if i.importAll then " all" else "")

/-- (helper) One module's header, as its compiled `.olean` records it. -/
structure Header where
  isModule : Bool
  imports : Array Import

/-- (helper) Read a module's header from its `.olean`. -/
def readHeader (m : Name) : IO (Except String Header) := do
  let path ← findOLean m
  if !(← path.pathExists) then
    return .error s!"{m}: no compiled .olean at {path}; run `lake build` (and build the executables) first"
  let (data, _) ← readModuleData path
  return .ok { isModule := data.isModule, imports := data.imports }

/-- (helper) The package's source modules, from the files under `RueCore/`. -/
partial def sourceModules (dir : System.FilePath) (pre : Name) : IO (Array Name) := do
  let mut out := #[]
  for e in ← dir.readDir do
    if ← e.path.isDir then
      out := out ++ (← sourceModules e.path (pre.str e.fileName))
    else if e.fileName.endsWith ".lean" then
      out := out.push (pre.str (e.fileName.dropEnd 5).toString)
  return out

/-- (helper) Run the audit, print the graph and every violation, and return
the exit code. -/
def audit : IO UInt32 := do
  initSearchPath (← findSysroot)
  let mut problems : Array String := #[]
  let mut headers : Std.HashMap Name Header := {}
  let mut order : Array Name := #[]
  let mut work := roots
  -- walk the package's import graph from the roots, reading each header once
  while !work.isEmpty do
    let m := work.head!
    work := work.tail!
    if headers.contains m then continue
    match ← readHeader m with
    | .error e => problems := problems.push e
    | .ok h =>
      headers := headers.insert m h
      order := order.push m
      for i in h.imports do
        if inPackage i.module && !headers.contains i.module then work := work ++ [i.module]
  -- the table and the graph agree
  for m in order do
    if (layerOf? m).isNone then
      problems := problems.push s!"{m}: not in the layer table (RueCore/LayersMain.lean, `table`)"
  for (m, _) in table do
    if !headers.contains m && !problems.any (·.startsWith s!"{m}:") then
      problems := problems.push s!"{m}: in the layer table but reached from no root (stale entry?)"
  if ← System.FilePath.isDir "RueCore" then
    for m in (← sourceModules "RueCore" `RueCore) ++ #[`RueCore] do
      if !headers.contains m && !problems.any (·.startsWith s!"{m}:") then
        problems := problems.push s!"{m}: a source file no root imports, so nothing builds or audits it"
  else
    problems := problems.push "no RueCore/ directory here; run from docs/formal/lean"
  -- every import points down, or sideways
  let mut edges := 0
  let mut lines : Array String := #[]
  for m in order.qsort (fun a b =>
      (layerOf? a).getD 9 < (layerOf? b).getD 9 ||
        ((layerOf? a).getD 9 == (layerOf? b).getD 9 && a.toString < b.toString)) do
    let some h := headers.get? m | continue
    let some l := layerOf? m | continue
    if l ≤ 2 && !h.isModule then
      problems := problems.push s!"{m} ({layerName l}): not a `module`; L0–L2 use the module system"
    let mut shown : Array String := #[]
    for i in h.imports do
      if inPackage i.module then
        edges := edges + 1
        let li := layerOf? i.module
        shown := shown.push s!"{i.module} [{importKind i}]"
        if let some li := li then
          if li > l then
            problems := problems.push
              s!"{m} ({layerName l}) imports {i.module} ({layerName li}): an upward import"
      else if l ≤ 2 && i.module != `Init then
        problems := problems.push
          s!"{m} ({layerName l}) imports {i.module}, outside the package; L0–L2 import only `Init`"
    let kind := if h.isModule then "module" else "file"
    lines := lines.push s!"{layerName l} | {m} ({kind}) <- {", ".intercalate shown.toList}"
  for line in lines do IO.println line
  if problems.isEmpty then
    IO.println s!"ruecore-layers: {order.size} modules, {edges} package imports, no upward import"
    return 0
  for p in problems do IO.eprintln s!"ruecore-layers: {p}"
  IO.eprintln s!"ruecore-layers: {problems.size} violation(s)"
  return 1

end RueCore.Layers

/-- (helper) `lake exe ruecore-layers`: the layering audit. -/
def main : IO UInt32 := RueCore.Layers.audit
