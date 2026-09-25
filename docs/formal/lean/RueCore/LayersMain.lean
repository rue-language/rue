import RueCore.Layers

/-!
# `lake exe ruecore-layers` — the layering audit (RUE-2456)

```
ruecore-layers            audit the package's import graph; exit 1 on a violation
ruecore-layers --closure  print the roots' import closure outside the toolchain,
                          the modules the kernel re-check replays
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
import anything, and no module of L0–L2 imports L3. `table` is the one place a layer is
declared; it lives in `RueCore/Layers.lean`, which the trusted-base lint
(`RueCore/Lint.lean`, RUE-2457) reads too.

The audit reads the import graph the build recorded, not the sources: each
module's imports come from its compiled `.olean` header (`readModuleData`),
starting from the library root and every `lean_exe` root of `lakefile.toml`,
and it follows every import, not only the package's: the walk is the roots'
whole import closure. It stops at the toolchain's own modules (`Init`, `Std`,
`Lean`, `Lake`, when their `.olean` is the one the toolchain ships), which are
trusted as the toolchain is. It fails when

* a module in the closure is neither a module of the package nor the
  toolchain's — a library a `[[lean_lib]]` line adds, say — so that the
  closure outside the toolchain is exactly the package's modules in `table`;
* a module imports a module of a higher layer;
* an L0–L2 module imports anything outside the package but Lean's `Init`;
* an L0–L2 module is not a `module` (the module system is adopted for the
  claim's layers: `module` headers, `public import`, `@[expose] public
  section`);
* a package module reached from the roots is missing from `table`, a `table`
  entry is not reached (stale), or a source file under `RueCore/` is not
  reached (built by nothing, so audited by nothing).

Run it from the package directory after `lake build`; a module without a
compiled `.olean` is reported rather than guessed at. `--closure` prints the
same walk's modules outside the toolchain, one per line, for `lake env
leanchecker` (the kernel re-check, RUE-2457): `bin/chain.sh` and the Buck
target `root//:lean-ruecore` replay exactly that list. Nothing here is about the
calculus, so the executable imports only `Lean` (through `RueCore.Layers`)
and builds in seconds.
-/

open Lean

namespace RueCore.Layers

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
  let path? ← try some <$> findOLean m catch _ => pure none
  let some path := path?
    | return .error s!"{m}: no .olean on the search path (an unknown module prefix); run `lake build` (and build the executables) first"
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

/-- (helper) The toolchain's own module roots. A module under one of them is
Lean's — its core library, standard library, compiler or build tool — and is
trusted as the toolchain is: neither the audit nor the kernel re-check reads
past it. -/
def toolchainRoots : List Name := [`Init, `Std, `Lean, `Lake]

/-- (helper) Is this module named as one of the toolchain's? -/
def isToolchainName (m : Name) : Bool := toolchainRoots.any (·.isPrefixOf m)

/-- (helper) The import closure of the roots, read from the `.olean` headers.
Plain arrays, so that the theorems Lean generates for the structure stay
within the lint's axiom allow-list. -/
structure Closure where
  /-- Every module of the closure that is not the toolchain's, with its header,
  in the order the walk met them. -/
  modules : Array (Name × Header)
  /-- The toolchain modules the closure imports directly; not read past. -/
  toolchain : Array Name
  /-- Modules whose header could not be read. -/
  unread : Array String
  /-- Modules that are neither the package's nor the toolchain's. -/
  foreign : Array String

/-- (helper) Walk the import closure of every root, reading each header once.
A module named as the toolchain's counts as the toolchain's only when its
`.olean` is the one the toolchain ships, under `<sysroot>/lib/lean`; anything
else — a package module, or any other library a `lakefile.toml` adds — is
read and walked, and a module that is neither the package's nor the
toolchain's is recorded in `foreign`. -/
def walk : IO Closure := do
  let sysroot ← findSysroot
  initSearchPath sysroot
  let lib := (← IO.FS.realPath (sysroot / "lib" / "lean")).toString
  let mut c : Closure := { modules := #[], toolchain := #[], unread := #[], foreign := #[] }
  let mut seen : NameSet := {}
  let mut work := roots
  while !work.isEmpty do
    let m := work.head!
    work := work.tail!
    if seen.contains m then continue
    seen := seen.insert m
    if isToolchainName m then
      let path? ← try some <$> findOLean m catch _ => pure none
      let shipped ← match path? with
        | some p => do
            if !(← p.pathExists) then pure false
            else pure ((← IO.FS.realPath p).toString.startsWith lib)
        | none => pure false
      if shipped then
        c := { c with toolchain := c.toolchain.push m }
        continue
      c := { c with foreign := c.foreign.push (
        s!"{m}: named as a toolchain module, but its .olean is not the one the toolchain ships under {lib}") }
    match ← readHeader m with
    | .error e => c := { c with unread := c.unread.push e }
    | .ok h =>
      c := { c with modules := c.modules.push (m, h) }
      if !inPackage m && !isToolchainName m then
        c := { c with foreign := c.foreign.push (
          s!"{m}: in the roots' import closure, but neither a module of the package (`RueCore.*`) nor the toolchain's ({toolchainRoots}); the closure may hold only those") }
      for i in h.imports do
        if !seen.contains i.module then work := work ++ [i.module]
  return c

/-- (helper) Run the audit, print the graph and every violation, and return
the exit code. -/
def audit : IO UInt32 := do
  let c ← walk
  let headers : Std.HashMap Name Header := c.modules.foldl (init := {}) fun acc (m, h) => acc.insert m h
  let order := c.modules.map (·.1)
  let mut problems : Array String := c.unread ++ c.foreign
  -- the table and the graph agree
  for m in order do
    if inPackage m && (layerOf? m).isNone then
      problems := problems.push s!"{m}: not in the layer table (RueCore/Layers.lean, `table`)"
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
  let pkg := (order.filter inPackage).size
  if problems.isEmpty then
    IO.println s!"ruecore-layers: {pkg} modules, {edges} package imports, no upward import; import closure: {order.size} modules outside the toolchain, all the package's, and {c.toolchain.size} toolchain modules imported directly"
    return 0
  for p in problems do IO.eprintln s!"ruecore-layers: {p}"
  IO.eprintln s!"ruecore-layers: {problems.size} violation(s)"
  return 1

/-- (helper) Print the import closure's modules outside the toolchain, one
per line: what the kernel re-check replays (`lake env leanchecker $(lake exe
ruecore-layers --closure)`). Exits 1 when a header cannot be read; a module
that is neither the package's nor the toolchain's is printed too — so the
re-check replays it — and the audit fails on it. -/
def printClosure : IO UInt32 := do
  let c ← walk
  for m in (c.modules.map (·.1)).qsort (·.toString < ·.toString) do IO.println m
  for e in c.unread do IO.eprintln s!"ruecore-layers: {e}"
  return if c.unread.isEmpty then 0 else 1

end RueCore.Layers

/-- (helper) `lake exe ruecore-layers`: the layering audit; with `--closure`,
the list of modules the kernel re-check replays. -/
def main (args : List String) : IO UInt32 :=
  match args with
  | [] => RueCore.Layers.audit
  | ["--closure"] => RueCore.Layers.printClosure
  | _ => do
    IO.eprintln "usage: ruecore-layers [--closure]"
    return 2
